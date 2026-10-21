//! Listening, routing, parameter validation, error responses, static files and response
//! headers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Router};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::auth::{self, CredentialState, Session};
use crate::catalog;
use crate::codes::*;
use crate::drivers;
use crate::facts::{self, Reading};
use crate::host::Host;
use crate::plan::{self, Ctx, PlanSlot, Precheck, SourceRequest, SubmitReject};
use crate::settings::Settings;
use crate::startup::{self, Startup};
use crate::store::{self, Paths};
use crate::task_state::{self, Status, TaskState};
use crate::upload::UploadState;

mod authentication;
use authentication::{
    change_password, login_status, logout, password_login, setup_account, ssh_challenge,
    ssh_key_delete, ssh_keys_add, ssh_keys_list, ssh_login_route,
};

mod uploads;
use uploads::{
    cancel_current_upload_plan, current_upload, open_upload, receive_upload_file,
    upload_toolkit_version,
};

mod plans;
use plans::{cancel_plan, create_plan, submit_task};

mod task_routes;
use task_routes::{list_tasks, release_task, task_detail, task_source_json};

mod driver_routes;
use driver_routes::{delete_drivers, list_drivers, set_driver_keep};

#[cfg(test)]
use task_routes::{notice_json, version_change};

/// The session (if any) attached by the session middleware.
#[derive(Clone, Debug)]
pub struct SessionOpt(pub Option<Session>);

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

const NAV_403_ZH: &str = "请求来自其他网站,已被拒绝。请直接打开本程序的地址。";
const NAV_403_EN: &str =
    "The request came from another site and was refused. Open this program's address directly.";

/// The unified API error response.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub key: &'static str,
    pub params: Value,
    pub raw: Option<String>,
}

impl ApiError {
    pub fn new(status: StatusCode, key: &'static str) -> ApiError {
        ApiError {
            status,
            key,
            params: json!({}),
            raw: None,
        }
    }

    pub fn params(mut self, params: Value) -> ApiError {
        self.params = params;
        self
    }

    pub fn raw(mut self, raw: impl Into<String>) -> ApiError {
        let raw = raw.into();
        if !raw.is_empty() {
            self.raw = Some(raw);
        }
        self
    }

    pub fn body(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("error".into(), json!(self.key));
        obj.insert("params".into(), self.params.clone());
        if let Some(raw) = &self.raw {
            obj.insert("raw".into(), json!(raw));
        }
        Value::Object(obj)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::to_vec(&self.body()).unwrap_or_default();
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = self.status;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        set_csp(response.headers_mut());
        response
    }
}

fn set_csp(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
}

fn internal() -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        .params(json!({"reason": {"key": "internal.request_failed", "params": {}}}))
}

/// A JSON body extractor with the unified rejections.
pub struct JsonBody<T>(pub T);

impl<S, T> axum::extract::FromRequest<S> for JsonBody<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        // The media type is exact, parameters aside: a substring match would let
        // `text/application/json-invalid` through.
        let media_type = content_type.split(';').next().unwrap_or("").trim();
        if !media_type.eq_ignore_ascii_case("application/json") {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "request.not_json",
            ));
        }
        let bytes = match tokio::time::timeout(
            JSON_BODY_DEADLINE,
            axum::body::Bytes::from_request(req, state),
        )
        .await
        {
            Err(_) => {
                return Err(ApiError::new(StatusCode::BAD_REQUEST, "request.invalid"));
            }
            Ok(Err(e)) => {
                let status = e.status();
                return Err(if status == StatusCode::PAYLOAD_TOO_LARGE {
                    ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "request.too_large")
                } else {
                    ApiError::new(StatusCode::BAD_REQUEST, "request.invalid")
                });
            }
            Ok(Ok(bytes)) => bytes,
        };
        serde_json::from_slice::<T>(&bytes)
            .map(JsonBody)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "request.invalid"))
    }
}

/// A static file table built once at startup.
#[derive(Debug, Clone, Default)]
pub struct StaticFiles {
    pub files: BTreeMap<String, (axum::body::Bytes, &'static str)>,
}

pub fn content_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub fn load_static_files(root: &Path) -> StaticFiles {
    let mut files = BTreeMap::new();
    fn walk(
        root: &Path,
        dir: &Path,
        files: &mut BTreeMap<String, (axum::body::Bytes, &'static str)>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, files);
            } else if let Ok(relative) = path.strip_prefix(root) {
                let rel = relative.to_string_lossy().replace('\\', "/");
                if let Ok(bytes) = std::fs::read(&path) {
                    files.insert(rel.clone(), (bytes.into(), content_type_for(&rel)));
                }
            }
        }
    }
    walk(root, root, &mut files);
    StaticFiles { files }
}

/// Origin guard: a mutating request must carry an `Origin` whose authority equals the `Host`.
/// The Fetch Metadata classification was removed as redundant request limiting (cleanup
/// 20261010).
pub async fn origin_guard(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();
    let method = req.method().clone();
    if method != Method::GET && method != Method::HEAD {
        let origin = headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let ok = match (origin.as_deref(), host.as_deref()) {
            (Some(origin), Some(host)) if origin != "null" => {
                // The Origin must carry a scheme; without one it is not a valid origin and
                // the request is refused.
                let authority = origin
                    .strip_prefix("http://")
                    .or_else(|| origin.strip_prefix("https://"));
                match authority {
                    Some(authority) => authority.eq_ignore_ascii_case(host),
                    None => false,
                }
            }
            _ => false,
        };
        if !ok {
            if path.starts_with("/api/") {
                return ApiError::new(StatusCode::FORBIDDEN, "auth.origin_mismatch")
                    .into_response();
            }
            return nav_403_response();
        }
    }
    next.run(req).await
}

/// The plain-text refusal of a foreign-origin write to a non-API path: the two
/// approved lines, carrying the CSP of every response.
fn nav_403_response() -> Response {
    let body = format!("{NAV_403_ZH}\n{NAV_403_EN}\n");
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::FORBIDDEN;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    set_csp(response.headers_mut());
    response
}

/// Session check.
pub async fn session_check(State(ctx): State<Arc<Ctx>>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let exempt = path == "/api/setup"
        || path == "/api/login"
        || path == "/api/ssh/challenge"
        || path == "/api/ssh/login"
        || !path.starts_with("/api/");
    let headers = req.headers().clone();
    let session = {
        let ctx = ctx.clone();
        match tokio::task::spawn_blocking(move || {
            ctx.auth
                .check_session_at(ctx.host.as_ref(), &ctx.paths, &headers)
        })
        .await
        {
            Ok(Ok(session)) => Some(session),
            _ => None,
        }
    };
    if session.is_none() && !exempt {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    req.extensions_mut().insert(SessionOpt(session));
    next.run(req).await
}

/// The same cookie-and-token check the middleware uses, repeated after a
/// write has waited on `process_lock` or the credentials lock.
fn reject_if_session_lost(ctx: &Ctx, headers: &HeaderMap) -> Option<Response> {
    if ctx
        .auth
        .check_session_at(ctx.host.as_ref(), &ctx.paths, headers)
        .is_ok()
    {
        None
    } else {
        Some(ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response())
    }
}

fn with_common_headers(mut response: Response) -> Response {
    set_csp(response.headers_mut());
    response
}

/// The unified method-not-allowed response: a JSON body with the same `request.invalid` key,
/// CSP included, and the `Allow` header kept when axum produced one. A framework-generated
/// 400 (for example a path segment that is not valid UTF-8) is answered with the JSON error
/// shape instead of axum's plain text, before any path or command action runs.
async fn invalid_request_json(req: Request, next: Next) -> Response {
    let response = next.run(req).await;
    if response.status() != StatusCode::BAD_REQUEST {
        return response;
    }
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("application/json"))
        .unwrap_or(false);
    if is_json {
        return response;
    }
    ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response()
}

async fn method_not_allowed_json(req: Request, next: Next) -> Response {
    let response = next.run(req).await;
    if response.status() != StatusCode::METHOD_NOT_ALLOWED {
        return response;
    }
    let allow = response.headers().get(header::ALLOW).cloned();
    let mut replacement =
        ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "request.invalid").into_response();
    if let Some(allow) = allow {
        replacement.headers_mut().insert(header::ALLOW, allow);
    }
    replacement
}

pub fn router(ctx: Arc<Ctx>, static_files: StaticFiles) -> Router {
    let api = Router::new()
        .route("/api/login", get(login_status).post(password_login))
        .route("/api/setup", post(setup_account))
        .route("/api/password", post(change_password))
        .route("/api/home", get(home))
        .route("/api/gpu-users", get(gpu_users_snapshot))
        .route("/api/versions", get(versions))
        .route("/api/uploads", post(open_upload))
        .route("/api/uploads/current", get(current_upload))
        .route(
            "/api/uploads/current/cancel-plan",
            post(cancel_current_upload_plan),
        )
        .route("/api/uploads/toolkit-version", get(upload_toolkit_version))
        .route("/api/uploads/{group_id}/files", put(receive_upload_file))
        .route("/api/plans", post(create_plan))
        .route("/api/plans/{id}/cancel", post(cancel_plan))
        .route("/api/tasks", get(list_tasks).post(submit_task))
        .route("/api/tasks/{id}", get(task_detail))
        .route("/api/tasks/{id}/release", post(release_task))
        .route("/api/drivers", get(list_drivers))
        .route("/api/drivers/delete", post(delete_drivers))
        .route("/api/drivers/{id}/keep", put(set_driver_keep))
        .route("/api/logout", post(logout))
        .route("/api/ssh/challenge", post(ssh_challenge))
        .route("/api/ssh/login", post(ssh_login_route))
        .route("/api/ssh/keys", get(ssh_keys_list).post(ssh_keys_add))
        .route("/api/ssh/keys/{id}", axum::routing::delete(ssh_key_delete));

    let static_state = Arc::new(static_files);
    let router = Router::new()
        .merge(api)
        .fallback(serve_static)
        .with_state((ctx.clone(), static_state));

    router
        .layer(middleware::from_fn_with_state(ctx.clone(), session_check))
        .layer(middleware::from_fn(origin_guard))
        .layer(middleware::from_fn(method_not_allowed_json))
        .layer(middleware::from_fn(invalid_request_json))
        // Every response, including the ones produced by the middleware above
        // and by the static fallback, carries the content security policy.
        .layer(middleware::from_fn(common_headers))
        // Outermost: caps the JSON extractors. The upload route reads the raw
        // body and keeps its own streaming limit.
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT))
}

/// Add the response headers that every response carries.
async fn common_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    set_csp(response.headers_mut());
    response
}

type AppState = (Arc<Ctx>, Arc<StaticFiles>);

// ---- helpers ---------------------------------------------------------------

/// the login status `setup.problems[].key` stays in the closed set (`setting.unknown`,
/// `setting.invalid`, `startup.failed`). A known variable whose value is not valid UTF-8 is
/// the invalid-setting notice `setting.invalid` problem, marked in its params; it is not a
/// separate outer protocol key.
fn problem_json(problem: &crate::settings::SettingProblem) -> Value {
    match problem {
        crate::settings::SettingProblem::Unknown { variable } => json!({
            "key": "setting.unknown",
            "params": {"variable": variable},
        }),
        crate::settings::SettingProblem::Invalid { variable, rule } => json!({
            "key": "setting.invalid",
            "params": {
                "variable": variable,
                "requirement": {"key": rule, "params": {}},
            },
        }),
        crate::settings::SettingProblem::NotUtf8 { variable } => json!({
            "key": "setting.invalid",
            "params": {"variable": variable, "invalid_utf8": true},
        }),
    }
}

/// One startup problem item: its semantic key
/// (`startup.pid_host`, `startup.runtime`, `startup.data_mapping`) with its
/// params; a logged-in response also carries the external command output.
fn startup_item_json(problem: &crate::startup::StartupProblem, logged_in: bool) -> Value {
    let mut obj = problem.to_json();
    if logged_in {
        if let Some(raw) = problem.raw() {
            if let Some(map) = obj.as_object_mut() {
                map.insert("raw".into(), json!(raw));
            }
        }
    }
    obj
}

/// `setup.problems[].key` stays in the closed set, so the failed checks are one
/// `startup.failed` problem whose `items` are the internal item keys and params; each item
/// renders as one startup-failure notice line.
fn startup_problem_json(
    problems: &[crate::startup::StartupProblem],
    logged_in: bool,
) -> Option<Value> {
    if problems.is_empty() {
        return None;
    }
    let items: Vec<Value> = problems
        .iter()
        .map(|problem| startup_item_json(problem, logged_in))
        .collect();
    Some(json!({ "key": "startup.failed", "params": { "items": items } }))
}

fn reading_json<T: serde::Serialize>(reading: &Reading<T>) -> Value {
    match reading {
        Reading::Value(v) => json!({"value": v}),
        Reading::Unknown(reason) => json!({
            "unknown": crate::task_state::general_reason_json(
                reason,
                READING_TIMEOUT.as_secs(),
            )
        }),
    }
}

fn version_reading_json(reading: &facts::LoadedVersion) -> Value {
    match reading {
        facts::LoadedVersion::Value(v) => json!({"value": v.text()}),
        facts::LoadedVersion::NotLoaded => json!({"not_loaded": true}),
        facts::LoadedVersion::Unknown(reason) => json!({
            "unknown": crate::task_state::general_reason_json(
                reason,
                READING_TIMEOUT.as_secs(),
            )
        }),
    }
}

fn update_blocked_json(
    ctx: &Ctx,
    _session: &SessionOpt,
    link_missing: Option<bool>,
    active: Option<&crate::tasks::ActiveState>,
) -> Value {
    let mut items = Vec::new();
    if ctx.setup_problems() {
        items.push(json!({"key": "setup"}));
    }
    if let Some(true) = link_missing {
        items.push(json!({"key": "extension_missing"}));
    }
    if let Some(blocker) = active.and_then(crate::tasks::ActiveState::blocker) {
        match blocker {
            crate::tasks::Blocker::Running { .. } => {
                items.push(json!({"key": "task_running"}));
            }
            crate::tasks::Blocker::Unreadable { reason } => items.push(json!({
                "key": "task_state_unreadable",
                "reason": {"key": reason.key, "params": reason.params},
            })),
        }
    }
    Value::Array(items)
}

/// The cookie or the request token no longer matches after the wait.
struct SessionLost;

/// The blocking reconcile under the process lock: the body every bounded
/// response worker runs first.
fn reconcile_sync(
    ctx: &Arc<Ctx>,
    headers: &HeaderMap,
) -> Result<Option<crate::tasks::Reconciled>, SessionLost> {
    if !ctx.startup.pid_host_ok {
        return Ok(None);
    }
    let _guard = ctx.process_lock.blocking_lock();
    // The request waited on this lock. Recheck the cookie and the request
    // token before any task record is rewritten.
    if reject_if_session_lost(ctx, headers).is_some() {
        return Err(SessionLost);
    }
    Ok(Some(crate::tasks::reconcile_managed(
        ctx.host.as_ref(),
        &ctx.paths,
    )))
}

/// The approved reason of a reading that ran out of its budget.
fn reading_timed_out() -> crate::task_state::ReasonValue {
    crate::task_state::general_reason(REASON_TIMED_OUT, READING_TIMEOUT.as_secs())
}

/// Bound the lock wait and reconciliation by one READING_TIMEOUT.
/// Timeout leaves the worker running and its lock held until the work ends.
async fn reconcile_bounded(
    ctx: &Arc<Ctx>,
    headers: &HeaderMap,
) -> Result<Option<crate::tasks::Reconciled>, SessionLost> {
    let ctx2 = ctx.clone();
    let headers = headers.clone();
    let work = tokio::task::spawn_blocking(move || reconcile_sync(&ctx2, &headers));
    match tokio::time::timeout(READING_TIMEOUT, work).await {
        Ok(Ok(reconciled)) => reconciled,
        Ok(Err(_)) => Ok(None),
        Err(_) => Ok(Some(crate::tasks::Reconciled::unreadable(
            reading_timed_out(),
        ))),
    }
}

/// The outcome of one whole bounded response.
enum WholeResponse {
    Ready(Response),
    /// The budget passed; the caller answers the approved timed-out shape.
    TimedOut,
    Failed,
}

/// Bound reconciliation and response work by one READING_TIMEOUT.
/// The process lock covers reconciliation only; timeout does not cancel the worker.
/// Timed-out callers must build their fallback without further host reads.
async fn whole_response<F>(ctx: &Arc<Ctx>, headers: HeaderMap, work: F) -> WholeResponse
where
    F: FnOnce(&Arc<Ctx>, Option<crate::tasks::Reconciled>, bool) -> Response + Send + 'static,
{
    let ctx2 = ctx.clone();
    let joined = tokio::task::spawn_blocking(move || match reconcile_sync(&ctx2, &headers) {
        Ok(reconciled) => work(&ctx2, reconciled, true),
        Err(SessionLost) => {
            ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response()
        }
    });
    match tokio::time::timeout(READING_TIMEOUT, joined).await {
        Ok(Ok(response)) => WholeResponse::Ready(response),
        Ok(Err(_)) => WholeResponse::Failed,
        Err(_) => WholeResponse::TimedOut,
    }
}

/// Why one bounded reading produced no value.
enum ReadingFailure {
    TimedOut(Duration),
    Failed,
}

impl ReadingFailure {
    /// The `{"unknown": {key, params}}` shape of an home reading.
    fn unknown_json(&self) -> Value {
        match self {
            ReadingFailure::TimedOut(limit) => json!({
                "unknown": {"key": REASON_TIMED_OUT, "params": {"seconds": limit.as_secs()}},
            }),
            ReadingFailure::Failed => json!({
                "unknown": {"key": REASON_IO_ERROR, "params": {}},
            }),
        }
    }

    fn reason_json(&self) -> Value {
        match self {
            ReadingFailure::TimedOut(limit) => {
                crate::task_state::general_reason_json(REASON_TIMED_OUT, limit.as_secs())
            }
            ReadingFailure::Failed => {
                crate::task_state::general_reason_json(REASON_IO_ERROR, READING_TIMEOUT.as_secs())
            }
        }
    }
}

/// Run one reading on the blocking pool, bounded by `READING_TIMEOUT`.
async fn bounded_reading<T, F>(f: F) -> Result<T, ReadingFailure>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    bounded_reading_with(READING_TIMEOUT, f).await
}

/// [`bounded_reading`] with the reading's own budget: catalog query uses its
/// `CATALOG_CMD_TIMEOUT` (20 s), while kernel header reading/module reading/GPU reading keep
/// `READING_TIMEOUT`.
async fn bounded_reading_with<T, F>(limit: Duration, f: F) -> Result<T, ReadingFailure>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::time::timeout(limit, tokio::task::spawn_blocking(f)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(ReadingFailure::Failed),
        Err(_) => Err(ReadingFailure::TimedOut(limit)),
    }
}

fn startup_unknown_json() -> Value {
    json!({"unknown": {"key": REASON_STARTUP_CHECK_FAILED, "params": {}}})
}

fn startup_check_reason() -> Value {
    crate::task_state::general_reason_json(REASON_STARTUP_CHECK_FAILED, READING_TIMEOUT.as_secs())
}

fn credentials_state_json(state: &CredentialState) -> Value {
    state.to_json()
}

fn show_reset_help(ctx: &Ctx, cred: &CredentialState) -> bool {
    match cred {
        CredentialState::Unreadable(r) => *r != auth::UnreadableReason::DirectoryUnreadable,
        CredentialState::Ok => ctx.auth.consecutive_failures() >= LOGIN_RESET_HELP_AFTER,
        _ => false,
    }
}

// ---- home --------------------------------------------------------------------

async fn home(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
    headers: HeaderMap,
) -> Response {
    with_common_headers(read_home(&ctx, &session, &headers).await)
}

/// With host PID access, read seven independent facts concurrently, each bounded by `READING_TIMEOUT`.
/// An unavailable reading carries its reason without suppressing the other results;
/// saved-original lookup follows once the kernel and TrueNAS version are known.
async fn read_home(ctx: &Arc<Ctx>, session: &SessionOpt, headers: &HeaderMap) -> Response {
    if !ctx.startup.pid_host_ok {
        let h = ctx.host.clone();
        let kernel = bounded_reading(move || facts::kernel(h.as_ref())).await;
        let kernel_json = match kernel {
            Ok(r) => reading_json(&r),
            Err(f) => f.unknown_json(),
        };
        let readings = HomeReadings {
            loaded: startup_unknown_json(),
            canonical_version: startup_unknown_json(),
            canonical_sha: startup_unknown_json(),
            truenas: startup_unknown_json(),
            kernel: kernel_json,
            link: startup_unknown_json(),
            link_missing: None,
            original: startup_unknown_json(),
        };
        return home_response(
            ctx.as_ref(),
            session,
            readings,
            startup_unknown_json(),
            None,
        );
    }

    let h1 = ctx.host.clone();
    let loaded = bounded_reading(move || {
        let modules = facts::read_modules(h1.as_ref());
        facts::loaded_version(h1.as_ref(), Some(&modules))
    });
    let h2 = ctx.host.clone();
    let canonical = bounded_reading(move || facts::canonical_facts(h2.as_ref()));
    let h3 = ctx.host.clone();
    let canonical_sha = bounded_reading(move || facts::canonical_sha256(h3.as_ref()));
    let h4 = ctx.host.clone();
    let truenas = bounded_reading(move || facts::truenas_version(h4.as_ref()));
    let h5 = ctx.host.clone();
    let kernel = bounded_reading(move || facts::kernel(h5.as_ref()));
    let h6 = ctx.host.clone();
    let link = bounded_reading(move || facts::link(h6.as_ref()));
    let hgpu = ctx.host.clone();
    let gpus_read = bounded_reading(move || facts::read_gpus(hgpu.as_ref()));
    let (loaded, canonical, canonical_sha, truenas, kernel, link, gpus_read) = tokio::join!(
        loaded,
        canonical,
        canonical_sha,
        truenas,
        kernel,
        link,
        gpus_read
    );

    let loaded_json = match loaded {
        Ok(v) => version_reading_json(&v),
        Err(f) => f.unknown_json(),
    };
    let canonical_version = match canonical {
        Ok(Ok(facts)) => reading_json(&facts.file_version),
        Ok(Err(e)) => json!({"unknown": e.reason_json()}),
        Err(f) => f.unknown_json(),
    };
    let canonical_sha_json = match canonical_sha {
        Ok(r) => reading_json(&r),
        Err(f) => f.unknown_json(),
    };
    let truenas_json = match &truenas {
        Ok(r) => reading_json(r),
        Err(f) => f.unknown_json(),
    };
    let kernel_json = match &kernel {
        Ok(r) => reading_json(r),
        Err(f) => f.unknown_json(),
    };
    let (link_json, link_missing) = match link {
        Ok(link) => {
            let missing = matches!(link, facts::LinkReading::Missing);
            (link_reading_json(&link), Some(missing))
        }
        Err(f) => (
            json!({"exists": false, "unknown": f.reason_json()}),
            Some(false),
        ),
    };
    let original = match (&kernel, &truenas) {
        (Ok(Reading::Value(k)), Ok(Reading::Value(t))) => {
            let h7 = ctx.host.clone();
            let p7 = ctx.paths.clone();
            let k = k.clone();
            let t = t.clone();
            match bounded_reading(move || drivers::find_original(h7.as_ref(), &p7, &k, &t)).await {
                Ok(drivers::OriginalLookup::Found(row)) => {
                    let record = row.record_ok().unwrap();
                    json!({
                        "saved": true,
                        "version": record.version,
                        "saved_at": record.created_at,
                    })
                }
                Ok(drivers::OriginalLookup::NotFound) => json!({"saved": false}),
                Ok(drivers::OriginalLookup::Unreadable) => json!({
                    "unknown": {"key": REASON_RECORD_UNREADABLE, "params": {}},
                }),
                Err(f) => f.unknown_json(),
            }
        }
        (Err(f), _) | (_, Err(f)) => json!({"unknown": f.reason_json()}),
        (Ok(Reading::Unknown(r)), _) | (_, Ok(Reading::Unknown(r))) => json!({
            "unknown": crate::task_state::general_reason_json(r, READING_TIMEOUT.as_secs()),
        }),
    };

    let gpus_json = match gpus_read {
        Ok(Ok(gpus)) => home_gpus_value(&gpus),
        Ok(Err(e)) => json!({
            "unknown": crate::task_state::general_reason_json(
                e.reason,
                READING_TIMEOUT.as_secs(),
            ),
        }),
        Err(f) => f.unknown_json(),
    };
    let reconciled = match reconcile_bounded(ctx, headers).await {
        Ok(reconciled) => reconciled,
        Err(SessionLost) => {
            return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
        }
    };
    let readings = HomeReadings {
        loaded: loaded_json,
        canonical_version,
        canonical_sha: canonical_sha_json,
        truenas: truenas_json,
        kernel: kernel_json,
        link: link_json,
        link_missing,
        original,
    };
    home_response(ctx.as_ref(), session, readings, gpus_json, reconciled)
}

struct HomeReadings {
    loaded: Value,
    canonical_version: Value,
    canonical_sha: Value,
    truenas: Value,
    kernel: Value,
    link: Value,
    link_missing: Option<bool>,
    original: Value,
}

fn home_gpus_value(gpus: &[facts::HostGpu]) -> Value {
    let mut devices: Vec<Value> = gpus
        .iter()
        .map(|gpu| {
            let mut obj = serde_json::Map::new();
            obj.insert("address".into(), json!(gpu.address));
            obj.insert("device".into(), json!(format!("{:04x}", gpu.device)));
            obj.insert(
                "passthrough".into(),
                json!(gpu.driver.as_deref() == Some("vfio-pci")),
            );
            if let Some(entry) = catalog::entry(crate::gpu_table::GPUS, gpu.device) {
                obj.insert("name".into(), json!(entry.name));
            }
            Value::Object(obj)
        })
        .collect();
    devices.sort_by(|a, b| {
        a["address"]
            .as_str()
            .unwrap_or("")
            .cmp(b["address"].as_str().unwrap_or(""))
    });
    json!({"value": {"devices": devices}})
}

fn opt_millis(value: Option<u64>) -> Value {
    value.map(Value::from).unwrap_or(Value::Null)
}

fn task_record_json(task: &crate::tasks::ReconciledTask) -> Value {
    let times = crate::tasks::project_task_times(task.status, task.state.as_ref());
    let version = task
        .state
        .as_ref()
        .and_then(|state| state.plan.as_ref().map(|plan| plan.version.clone()));
    json!({
        "task_id": task.id,
        "status": task.status_for_ui(),
        "phase": task.phase.map(|phase| phase.as_str()),
        "created_at": opt_millis(times.created_at),
        "finished_at": opt_millis(times.finished_at),
        "interrupt_discovered_at": opt_millis(times.interrupt_discovered_at),
        "source": task_source_json(task.state.as_ref()),
        "version": version,
    })
}

fn active_task_json(reconciled: &crate::tasks::Reconciled) -> Value {
    match reconciled.active.blocker() {
        None => Value::Null,
        Some(crate::tasks::Blocker::Running { task_ids }) => {
            if let Some(id) = task_ids.first() {
                let task = reconciled.state_of(id);
                match task {
                    Some(task) if task.status.is_some() => {
                        json!({"task_id": id, "status": task.status_for_ui()})
                    }
                    _ => json!({
                        "task_id": id,
                        "status": "UNKNOWN",
                        "state_read_error": task.and_then(|task| task.state_read_error.clone()).unwrap_or_else(|| "missing".into()),
                    }),
                }
            } else {
                Value::Null
            }
        }
        Some(crate::tasks::Blocker::Unreadable { reason }) => json!({
            "unknown": {"key": reason.key, "params": reason.params},
        }),
    }
}

/// `interrupted` is set only when the unique latest recorded task is itself
/// interrupted. An unknown order does not fall back to an older interrupt.
fn home_task_fields(reconciled: Option<&crate::tasks::Reconciled>) -> (Value, Value, Value) {
    let Some(reconciled) = reconciled else {
        return (Value::Null, startup_unknown_json(), startup_unknown_json());
    };
    let active_task = active_task_json(reconciled);
    match reconciled.latest_recorded() {
        crate::tasks::LatestTask::Empty => (Value::Null, Value::Null, active_task),
        crate::tasks::LatestTask::Unknown => (
            Value::Null,
            json!({"unknown": {"key": "task.latest_unknown", "params": {}}}),
            active_task,
        ),
        crate::tasks::LatestTask::Selected(task) => {
            let record = task_record_json(task);
            let interrupted = if task.status == Some(Status::Interrupted) {
                record.clone()
            } else {
                Value::Null
            };
            (interrupted, record, active_task)
        }
    }
}

fn home_response(
    ctx: &Ctx,
    session: &SessionOpt,
    readings: HomeReadings,
    gpus: Value,
    reconciled: Option<crate::tasks::Reconciled>,
) -> Response {
    let (interrupted, latest_task, active_task) = home_task_fields(reconciled.as_ref());
    let update_blocked = update_blocked_json(
        ctx,
        session,
        readings.link_missing,
        reconciled.as_ref().map(|r| &r.active),
    );
    let body = json!({
        "loaded_version": readings.loaded,
        "canonical": {"version": readings.canonical_version, "sha256": readings.canonical_sha},
        "truenas_version": readings.truenas,
        "kernel": readings.kernel,
        "link": readings.link,
        "original_saved": readings.original,
        "gpus": gpus,
        "latest_task": latest_task,
        "interrupted": interrupted,
        "active_task": active_task,
        "update_blocked": update_blocked,
    });
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn link_reading_json(link: &facts::LinkReading) -> Value {
    match link {
        facts::LinkReading::Missing => json!({"exists": false}),
        facts::LinkReading::Ok => json!({
            "exists": true,
            "target": CANONICAL_PATH,
            "points_to_canonical": true,
        }),
        facts::LinkReading::Elsewhere(t) => json!({
            "exists": true,
            "target": t,
            "points_to_canonical": false,
        }),
        facts::LinkReading::Unknown(reason) => json!({
            "exists": false,
            "unknown": crate::task_state::general_reason_json(
                reason,
                READING_TIMEOUT.as_secs(),
            ),
        }),
    }
}

// ---- versions --------------------------------------------------------------------

async fn versions(State((ctx, _)): State<AppState>) -> Response {
    with_common_headers(read_versions(&ctx).await)
}

/// versions: the catalog, interface, module, GPU and current-kernel readings. Each runs on
/// its own blocking task under its own budget -- `CATALOG_CMD_TIMEOUT` (20 s) for catalog
/// query, `READING_TIMEOUT` (15 s) for kernel header reading/module reading/GPU
/// reading/current kernel -- so a read that blocks past its budget is answered as that
/// reading's timeout and can never hold the response past it. A timed-out reading classifies
/// exactly like the same reading's other failures: the catalog and the GPUs list an
/// `unreadable` item, kernel header reading unreadable means no branch verdict, and module
/// reading unreadable removes no branch and adds no item. An unreadable kernel only removes
/// the version that requires a 6.12 kernel.
async fn read_versions(ctx: &Arc<Ctx>) -> Response {
    if !ctx.startup.pid_host_ok {
        return json_response(json!({
            "items": [],
            "open_unbuildable": [],
            "unreadable": [{
                "object": "catalog",
                "reason": {"key": REASON_STARTUP_CHECK_FAILED, "params": {}},
            }],
            "gpus": [],
        }));
    }
    let h1 = ctx.host.clone();
    let catalog = bounded_reading_with(CATALOG_CMD_TIMEOUT, move || catalog_fetch(h1.as_ref()));
    let h2 = ctx.host.clone();
    let interfaces = bounded_reading(move || catalog::read_interfaces(h2.as_ref()));
    let h3 = ctx.host.clone();
    let modules = bounded_reading(move || facts::read_modules(h3.as_ref()));
    let h4 = ctx.host.clone();
    let gpus = bounded_reading(move || facts::read_gpus(h4.as_ref()));
    let h5 = ctx.host.clone();
    let kernel = bounded_reading(move || facts::kernel(h5.as_ref()));
    let (catalog, interfaces, modules, gpus, kernel) =
        tokio::join!(catalog, interfaces, modules, gpus, kernel);

    let mut unreadable: Vec<Value> = Vec::new();
    let catalog_result = match catalog {
        Ok(result) => result,
        Err(f) => {
            // the failure reason is the reading's `{key, params}` object, not an internal
            // short code.
            crate::log::error_json(Event::CatalogQueryFailed, &[("reason", f.reason_json())]);
            Err(catalog_item(f.reason_json(), ""))
        }
    };
    let interfaces = match interfaces {
        Ok(interfaces) => interfaces,
        Err(ReadingFailure::TimedOut(_)) => {
            catalog::KernelInterfaces::all_unknown(REASON_TIMED_OUT)
        }
        Err(ReadingFailure::Failed) => catalog::KernelInterfaces::all_unknown(REASON_IO_ERROR),
    };
    let modules = match modules {
        Ok(modules) => modules,
        Err(ReadingFailure::TimedOut(_)) => Reading::Unknown(REASON_TIMED_OUT),
        Err(ReadingFailure::Failed) => Reading::Unknown(REASON_IO_ERROR),
    };
    let gpus_result = match gpus {
        Ok(result) => result,
        Err(f) => Err(facts::GpusUnreadable {
            reason: match f {
                ReadingFailure::TimedOut(_) => REASON_TIMED_OUT,
                ReadingFailure::Failed => REASON_IO_ERROR,
            },
            path: crate::codes::PCI_DEVICES_PATH.to_string(),
        }),
    };
    let kernel = match kernel {
        Ok(Reading::Value(kernel)) => Some(kernel),
        _ => None,
    };

    let versions = match catalog_result {
        Ok(versions) => versions,
        Err(item) => {
            unreadable.push(item);
            Vec::new()
        }
    };

    let latest = catalog::branch_latest(&versions);
    let items = match catalog::items(&latest, &interfaces, &modules, kernel.as_deref()) {
        Ok(items) => items,
        Err(headers) => {
            for item in &headers.items {
                unreadable.push(json!({
                    "object": "kernel_headers",
                    "reason": crate::task_state::general_reason_json(
                        item.reason,
                        READING_TIMEOUT.as_secs(),
                    ),
                    "raw": item.path,
                }));
                crate::log::error_json(
                    Event::KernelHeadersUnreadable,
                    &[
                        ("path", json!(item.path)),
                        (
                            "reason",
                            crate::task_state::general_reason_json(
                                item.reason,
                                READING_TIMEOUT.as_secs(),
                            ),
                        ),
                    ],
                );
            }
            Vec::new()
        }
    };

    let gpus = match gpus_result {
        Ok(gpus) => Some(gpus),
        Err(e) => {
            crate::log::error_json(
                Event::GpusUnreadable,
                &[
                    ("path", json!(e.path.as_str())),
                    (
                        "reason",
                        crate::task_state::general_reason_json(e.reason, READING_TIMEOUT.as_secs()),
                    ),
                ],
            );
            unreadable.push(json!({
                "object": "gpus",
                "reason": crate::task_state::general_reason_json(
                    e.reason,
                    READING_TIMEOUT.as_secs(),
                ),
                "raw": e.path,
            }));
            None
        }
    };

    let any_unreadable = !unreadable.is_empty();
    let items_json: Vec<Value> = if any_unreadable {
        Vec::new()
    } else {
        items.iter().map(|v| json!({"version": v.text()})).collect()
    };
    let open_unbuildable = if any_unreadable {
        Vec::new()
    } else {
        catalog::open_unbuildable(&items, kernel.as_deref().unwrap_or_default(), &interfaces)
    };
    let usable_readings = (!any_unreadable && modules.is_value() && kernel.is_some())
        .then_some(open_unbuildable.as_slice());
    let gpus_json = gpus
        .map(|gpus| {
            catalog::gpu_readings_json(&catalog::gpu_list(
                &items,
                &gpus,
                crate::gpu_table::GPUS,
                crate::gpu_table::VERSIONS,
                usable_readings,
            ))
        })
        .unwrap_or_else(|| json!([]));
    json_response(json!({
        "items": items_json,
        "open_unbuildable": open_unbuildable,
        "unreadable": unreadable,
        "gpus": gpus_json,
    }))
}

/// catalog query with the failure classification. The curl stderr is the `raw` field of the
/// versions item, whatever the reason kind.
fn catalog_fetch(h: &dyn Host) -> Result<Vec<facts::Version>, Value> {
    let out = match catalog::fetch(h) {
        Ok(stdout) => stdout,
        Err(out) => {
            let reason = catalog::fetch_failure_reason(&out);
            let (key, params) = match &reason {
                crate::host::Reason::Keyed { key, params } => ((*key), params.clone()),
                crate::host::Reason::Stderr(_) => match out.exit {
                    crate::host::Exit::Code(code) => (REASON_EXIT_STATUS, json!({"code": code})),
                    crate::host::Exit::Signal(signal) => (REASON_SIGNAL, json!({"signal": signal})),
                    crate::host::Exit::TimedOut => (
                        REASON_TIMED_OUT,
                        json!({"seconds": CATALOG_CMD_TIMEOUT.as_secs()}),
                    ),
                    crate::host::Exit::SpawnFailed => (REASON_SPAWN_FAILED, json!({})),
                },
            };
            let raw = out.stderr_text();
            // the log's reason is the real `{key, params}` object and the curl stderr is the
            // external raw of that same object.
            let log_reason = crate::log::reason_object(key, params.clone(), Some(&raw));
            crate::log::error_json(Event::CatalogQueryFailed, &[("reason", log_reason)]);
            return Err(catalog_item(json!({"key": key, "params": params}), &raw));
        }
    };
    let (body, status) = catalog::split_status(&out);
    if status != Some(200) {
        let reason = json!({"key": REASON_HTTP_STATUS, "params": {"status": status}});
        crate::log::error_json(Event::CatalogQueryFailed, &[("reason", reason.clone())]);
        return Err(catalog_item(reason, ""));
    }
    let versions = catalog::parse_index(&body);
    if versions.is_empty() {
        let reason = json!({"key": REASON_INDEX_UNRECOGNIZED, "params": {}});
        crate::log::error_json(Event::CatalogQueryFailed, &[("reason", reason.clone())]);
        return Err(catalog_item(reason, ""));
    }
    // A successful query writes no failure event.
    Ok(versions)
}

/// One versions `unreadable` item; `raw` is written only when there is text.
fn catalog_item(reason: Value, raw: &str) -> Value {
    let mut item = json!({"object": "catalog", "reason": reason});
    if !raw.is_empty() {
        item["raw"] = json!(raw);
    }
    item
}

/// The snapshot never takes process_lock: plans may be waiting for GPU release while it runs.
async fn gpu_users_snapshot(State((ctx, _)): State<AppState>) -> Response {
    match tokio::task::spawn_blocking(move || ctx.gpu_users_snapshot()).await {
        Ok(snapshot) => json_response_status(StatusCode::OK, json!(snapshot)),
        Err(_) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response(),
    }
}

/// One query parameter of a request: absent, present but not valid UTF-8, or its decoded
/// value. The three cases stay distinct, so a malformed value is a format error and never the
/// default.
pub enum QueryParam {
    Missing,
    Invalid,
    Value(String),
}

/// Query-parameter parsing with percent decoding.
pub fn query_param(query: &str, name: &str) -> QueryParam {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let Some(key) = percent_decode(k) else {
            continue;
        };
        if key == name {
            return match percent_decode(v) {
                Some(value) => QueryParam::Value(value),
                None => QueryParam::Invalid,
            };
        }
    }
    QueryParam::Missing
}

pub fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

// ---- static files / logout -------------------------------------------------------------

/// static files: only GET and HEAD reach the static files.
async fn serve_static(State((_, files)): State<AppState>, request: Request) -> Response {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "request.invalid").into_response();
    }
    let path = request.uri().path().to_string();
    if path.starts_with("/api/") {
        return ApiError::new(StatusCode::NOT_FOUND, "not_found").into_response();
    }
    let relative = if path == "/" {
        "index.html".to_string()
    } else {
        path.trim_start_matches('/').to_string()
    };
    match files.files.get(&relative) {
        Some((bytes, content_type)) => {
            let mut response = Response::new(Body::from(bytes.clone()));
            *response.status_mut() = StatusCode::OK;
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            set_csp(response.headers_mut());
            response
        }
        None => {
            let mut response = Response::new(Body::from("not found"));
            *response.status_mut() = StatusCode::NOT_FOUND;
            set_csp(response.headers_mut());
            response
        }
    }
}

// ---- response helpers ------------------------------------------------------

fn json_response(value: Value) -> Response {
    json_response_status(StatusCode::OK, value)
}

fn json_response_status(status: StatusCode, value: Value) -> Response {
    let mut response = Response::new(Body::from(value.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

// ---- startup ---------------------------------------------------------------

/// Start the web process.
pub async fn serve(settings: Settings, host: Arc<dyn Host>) -> std::io::Result<()> {
    serve_with(settings, host, PathBuf::from(WEB_ROOT)).await
}

/// Test seam: the e2e server serves the built assets from another directory.
pub async fn serve_with(
    settings: Settings,
    host: Arc<dyn Host>,
    web_root: PathBuf,
) -> std::io::Result<()> {
    crate::log::info(Event::Starting, &[("version", VERSION)]);
    for problem in &settings.problems {
        match problem {
            crate::settings::SettingProblem::Unknown { variable } => {
                crate::log::error(Event::SettingUnknown, &[("variable", variable)])
            }
            crate::settings::SettingProblem::Invalid { variable, rule } => {
                crate::log::error(
                    Event::SettingInvalid,
                    &[("variable", variable), ("rule", rule)],
                );
                // /an unusable NVM_PORT listens on the default port and writes
                // `port_fallback` with that port.
                if *variable == crate::settings::VAR_PORT {
                    crate::log::error_json(Event::PortFallback, &[("port", json!(settings.port))]);
                }
            }
            crate::settings::SettingProblem::NotUtf8 { variable } => {
                // the `setting_invalid` line carries `variable` and `rule`. The
                // invalid-setting notice UTF-8 writing has no requirement sentence, so its
                // rule is the existing `setting.invalid_utf8` key the front end already
                // selects; the entered value is never logged.
                crate::log::error(
                    Event::SettingInvalid,
                    &[("variable", variable), ("rule", "setting.invalid_utf8")],
                );
                if variable == crate::settings::VAR_PORT {
                    crate::log::error_json(Event::PortFallback, &[("port", json!(settings.port))]);
                }
            }
        }
    }

    let paths = Paths::new(PathBuf::from(CONTAINER_DATA_DIR));
    let host2 = host.clone();
    let paths2 = paths.clone();
    let dirs = tokio::task::spawn_blocking(move || store::prepare_dirs(&*host2, &paths2))
        .await
        .unwrap_or_else(|_| Err(std::io::Error::other("task")));
    if let Err(e) = dirs {
        // the reason is the `{key, params}` object; the operating-system text stays as the
        // raw of the same object.
        crate::log::error_json(
            Event::DataDirsFailed,
            &[
                ("path", json!(CONTAINER_DATA_DIR)),
                (
                    "reason",
                    crate::log::reason_object(
                        crate::log::io_reason_key(&e),
                        json!({}),
                        Some(&e.to_string()),
                    ),
                ),
            ],
        );
    }

    let host3 = host.clone();
    let paths3 = paths.clone();
    let startup = tokio::task::spawn_blocking(move || startup::check(&*host3, &paths3))
        .await
        .unwrap_or(Startup {
            problems: Vec::new(),
            pid_host_ok: false,
            host_data_dir: None,
            image_id: None,
        });
    for problem in &startup.problems {
        // the reason is the startup problem's own `{key, params}` object; its external
        // command raw stays inside it.
        let mut reason = problem.log_reason_json();
        if let Some(raw) = problem.raw() {
            if !raw.is_empty() {
                if let Some(obj) = reason.as_object_mut() {
                    obj.insert("raw".into(), json!(raw));
                }
            }
        }
        crate::log::error_json(
            Event::StartupCheckFailed,
            &[("check", json!(problem.log_check())), ("reason", reason)],
        );
    }
    if startup.problems.is_empty() {
        crate::log::info(
            Event::StartupChecksPassed,
            &[
                (
                    "data_dir_host",
                    startup
                        .host_data_dir
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .as_str(),
                ),
                ("image_id", startup.image_id.as_deref().unwrap_or("")),
            ],
        );
    }

    let auth_state = std::sync::Arc::new(auth::AuthState::new());
    let pending_code = {
        let auth4 = auth_state.clone();
        let host4 = host.clone();
        let paths4 = paths.clone();
        let settings4 = settings.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = auth4.credentials_lock.blocking_lock();
            let state = auth4.load_state(&*host4, &paths4, &settings4);
            auth4.load_logged_out(&*host4, &paths4);
            match state {
                CredentialState::Unreadable(r) => crate::log::error_json(
                    Event::CredentialsUnreadable,
                    &[("reason", r.log_reason_object(json!({})))],
                ),
                CredentialState::Unsaved(r) => crate::log::error_json(
                    Event::CredentialsUnsaved,
                    &[("reason", r.log_reason_object(json!({})))],
                ),
                _ => {}
            }
            auth4.ensure_setup_code(&*host4, &paths4, &settings4, "start")
        })
        .await
        .ok()
        .flatten()
    };

    let host5 = host.clone();
    let paths5 = paths.clone();
    let leftover_group = tokio::task::spawn_blocking(move || {
        let _ = store::clean_leftovers(&*host5, &paths5);
        crate::upload::staged_group(&*host5, &paths5)
    })
    .await
    .unwrap_or(None);

    if startup.pid_host_ok {
        let host6 = host.clone();
        let paths6 = paths.clone();
        let reconciled =
            tokio::task::spawn_blocking(move || crate::tasks::reconcile_managed(&*host6, &paths6))
                .await;
        if let Ok(rec) = reconciled {
            if let Some(reason) = rec.unit_query_error {
                crate::log::error_json(
                    Event::UnitQueryFailed,
                    &[(
                        "reason",
                        json!({"key": reason.key, "params": reason.params}),
                    )],
                );
            }
        }
    }

    let static_files = load_static_files(&web_root);
    let ctx = Arc::new(Ctx {
        host,
        paths,
        settings: settings.clone(),
        startup,
        process_lock: tokio::sync::Mutex::new(()),
        gpu_users_cache: Default::default(),
        plan: std::sync::Mutex::new(PlanSlot::new()),
        upload: Arc::new(UploadState::with_current(leftover_group)),
        auth: auth_state,
    });
    let app = router(ctx, static_files);
    let address = format!("0.0.0.0:{}", settings.port);
    let listener = match tokio::net::TcpListener::bind(&address).await {
        Ok(l) => l,
        Err(e) => {
            // the bind failure's reason is an `{key, params}` object with the
            // operating-system text as its raw.
            crate::log::error_json(
                Event::ListenFailed,
                &[
                    ("address", json!(address)),
                    (
                        "reason",
                        crate::log::reason_object(
                            crate::log::io_reason_key(&e),
                            json!({}),
                            Some(&e.to_string()),
                        ),
                    ),
                ],
            );
            return Err(e);
        }
    };
    crate::log::info(Event::Listening, &[("address", &address)]);
    if let Some(issued) = pending_code {
        issued.log();
    }
    axum::serve(listener, app).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_value(query: &str, name: &str) -> Option<String> {
        match query_param(query, name) {
            QueryParam::Value(value) => Some(value),
            _ => None,
        }
    }

    #[test]
    fn query_parsing_and_percent_decoding() {
        assert_eq!(
            query_value("name=NVIDIA-Linux-x86_64-580.178.04.run&x=1", "name").as_deref(),
            Some("NVIDIA-Linux-x86_64-580.178.04.run")
        );
        assert_eq!(
            query_value("a=%2Fetc%2Fpasswd", "a").as_deref(),
            Some("/etc/passwd")
        );
        assert_eq!(query_value("a=c+d", "a").as_deref(), Some("c d"));
        assert!(matches!(query_param("other=1", "a"), QueryParam::Missing));
        // Invalid UTF-8 percent decoding is a format error, not a repaired value nor a
        // missing one.
        assert!(matches!(
            query_param("name=%ff.run", "name"),
            QueryParam::Invalid
        ));
        assert!(matches!(query_param("name=%2B1", "name"), QueryParam::Value(v) if v == "+1"));
        assert_eq!(percent_decode("%ff"), None);
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type_for("main.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type_for("styles.css"), "text/css; charset=utf-8");
        assert_eq!(content_type_for("x.bin"), "application/octet-stream");
    }

    #[test]
    fn reload_notices_use_the_final_correction_round() {
        use crate::task_state::*;
        for (pass, code) in [
            ("update", ErrorCode::ModuleUnloadRefused),
            ("restore", ErrorCode::RestoreAborted),
        ] {
            for final_ok in [false, true] {
                let mut state = TaskState::interrupted_stub(
                    "20260928T101530Z-3f9a0c12",
                    1,
                    None,
                    StateNote {
                        state_read_error: "test".into(),
                        kept_as: None,
                    },
                );
                state.status = Status::Failed;
                state.error = Some(ErrorRecord::new(code, json!({"cause": "unload_refused"})));
                state.replace.push(serde_json::from_value(json!({
                    "pass": pass,
                    "unloaded": ["nvidia"],
                    "reloaded": [{"module": "nvidia", "ok": !final_ok, "reason": if final_ok { json!({"stderr": "first failure"}) } else { Value::Null }}],
                    "module_correction": {
                        "trigger": "version_mismatch", "expected": "580.178.04",
                        "version_before": {"not_loaded": true}, "version_after": null,
                        "modules_before": [], "unloads": [], "stop_reason": null,
                        "reloaded": [{"module": "nvidia", "ok": final_ok, "reason": if final_ok { Value::Null } else { json!({"stderr": "final failure"}) }}],
                    }
                })).unwrap());
                assert_eq!(
                    notice_json(&state).unwrap()["params"]["reload_failed"],
                    !final_ok
                );
                state.status = Status::Succeeded;
                assert_eq!(
                    notice_json(&state).unwrap().get("variant").is_some(),
                    !final_ok
                );
            }
        }
    }

    #[test]
    fn version_change_conditions() {
        use crate::task_state::*;
        let mut state = TaskState {
            task_id: "20260928T101530Z-3f9a0c12".into(),
            status: Status::Succeeded,
            phase: Some(Phase::Check),
            created_at: 1,
            finished_at: Some(2),
            plan: None,
            loaded_version_start: Some(ReadingJson::value("580.173.02".into())),
            loaded_version_end: Some(ReadingJson::value("580.178.04".into())),
            reload_targets: None,
            download: None,
            rootfs: None,
            replace: Vec::new(),
            checks: Vec::new(),
            restore: None,
            error: None,
            gpu_users: None,
            nvidia_smi_gpus: None,
            state_note: None,
            released_by: None,
            released_at: None,
            build_env_pulled_image: None,
            waiting_gpu: None,
        };
        assert!(!version_change(&state));
        state.gpu_users = Some(GpuUsersRecord {
            checked_at: None,
            users: vec![GpuUserRecord {
                id: None,
                name: "x".into(),
                kind: "docker".into(),
                usage: None,
            }],
            errors: Vec::new(),
        });
        assert!(version_change(&state));
        state.loaded_version_end = Some(ReadingJson::value("580.173.02".into()));
        assert!(!version_change(&state));
        state.status = Status::Interrupted;
        state.loaded_version_end = Some(ReadingJson::value("580.178.04".into()));
        assert!(!version_change(&state));
    }
}

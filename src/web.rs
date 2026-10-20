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
        .route("/api/versions", get(versions))
        .route("/api/uploads", post(open_upload))
        .route("/api/uploads/current", get(current_upload))
        .route("/api/uploads/{group_id}/files", put(receive_upload_file))
        .route("/api/plans", post(create_plan))
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

/// Reconcile under the process lock, bounded by `READING_TIMEOUT`. The wait for the lock and
/// the reconcile together share the deadline; when it passes the caller reports the timeout
/// reason instead of leaving the request pending. The timeout does **not** cancel the
/// blocking worker: the guard follows the real worker, so the locks are released when the
/// work really ends.
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

/// Run one **whole** response — the bounded reconcile *and* the response work that follows it
/// — inside a single `READING_TIMEOUT`.
///
/// Guard scope, stated exactly: the process-lock guard is taken inside [`reconcile_sync`] and
/// is held for the reconcile alone; it is released when the reconcile returns, before the
/// read-only response work runs. The worker is never cancelled, so a timed-out answer cannot
/// release the reconcile's lock early — but nothing here claims that guard covers the
/// canonical/driver reads, which are independent read-only host calls.
///
/// `work` receives `reads = false` only from the timed-out path, where the caller must answer
/// without touching the host again.
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

// ---- login status / password login
// ---------------------------------------------------------------

async fn login_status(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
    headers: HeaderMap,
) -> Response {
    let ctx2 = ctx.clone();
    let session2 = session.clone();
    let prepared =
        tokio::task::spawn_blocking(move || prepare_login_status(&ctx2, &session2)).await;
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(_) => return internal().into_response(),
    };
    // Only a signed-in reader consumes the reconcile result (the login branch of the response
    // below), so an anonymous login-page read must not take the process lock or run the host
    // unit query (cleanup 20261010). The wait for the process lock and the reconcile together
    // are bounded by `READING_TIMEOUT`: past the deadline the timeout reason is returned
    // instead of holding the response open.
    let active = if session.0.is_some() {
        match reconcile_bounded(&ctx, &headers).await {
            Ok(reconciled) => reconciled.map(|r| r.active),
            Err(SessionLost) => {
                return with_common_headers(
                    ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response(),
                );
            }
        }
    } else {
        None
    };
    with_common_headers(login_status_response(
        ctx.as_ref(),
        &session,
        prepared,
        active,
    ))
}

struct PreparedLoginStatus {
    credentials: CredentialState,
    link_missing: Option<bool>,
    problems: Vec<Value>,
    ssh_enabled: bool,
}

fn prepare_login_status(ctx: &Ctx, session: &SessionOpt) -> PreparedLoginStatus {
    let h = ctx.host.as_ref();
    // Everything that is not credential state runs before the credentials lock is taken: the
    // extension-link reading is a host fact, and a slow host read must not delay a login or a
    // one-time password change, which need the same lock. The lock below covers the
    // credentials alone.
    let link_missing = if ctx.startup.pid_host_ok {
        matches!(facts::link(h), facts::LinkReading::Missing).then_some(true)
    } else {
        None
    };
    let mut problems: Vec<Value> = ctx.settings.problems.iter().map(problem_json).collect();
    if let Some(problem) = startup_problem_json(&ctx.startup.problems, session.0.is_some()) {
        problems.push(problem);
    }
    let _guard = ctx.auth.credentials_lock.blocking_lock();
    let credentials = ctx.auth.load_state(h, &ctx.paths, &ctx.settings);
    if let Some(issued) = ctx
        .auth
        .ensure_setup_code(h, &ctx.paths, &ctx.settings, "reset")
    {
        issued.log();
    }
    // login status always carries the real `ssh_enabled`: true only when the credentials file
    // reads back valid and holds at least one registered key. It is read under the lock, with
    // the credentials it belongs to.
    let ssh_enabled = credentials.is_ok() && ctx.auth.ssh_key_count(h, &ctx.paths) > 0;
    PreparedLoginStatus {
        credentials,
        link_missing,
        problems,
        ssh_enabled,
    }
}

fn login_status_response(
    ctx: &Ctx,
    session: &SessionOpt,
    prepared: PreparedLoginStatus,
    active: Option<crate::tasks::ActiveState>,
) -> Response {
    let mut body = serde_json::Map::new();
    body.insert("logged_in".into(), json!(session.0.is_some()));
    if session.0.is_some() {
        body.insert(
            "update_blocked".into(),
            update_blocked_json(ctx, session, prepared.link_missing, active.as_ref()),
        );
    }
    let mut setup = json!({"problems": prepared.problems});
    if matches!(prepared.credentials, CredentialState::SetupRequired) {
        setup["username"] = json!(ctx.settings.username);
        setup["valid_hours"] = json!(24);
    }
    body.insert("setup".into(), setup);
    body.insert(
        "credentials".into(),
        credentials_state_json(&prepared.credentials),
    );
    body.insert(
        "show_reset_help".into(),
        json!(show_reset_help(ctx, &prepared.credentials)),
    );
    body.insert("ssh_enabled".into(), json!(prepared.ssh_enabled));
    let body = Value::Object(body);
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupRequest {
    code: String,
    new_password: String,
}

async fn setup_account(
    State((ctx, _)): State<AppState>,
    JsonBody(req): JsonBody<SetupRequest>,
) -> Response {
    let _guard = ctx.auth.credentials_lock.lock().await;
    let result = ctx
        .auth
        .setup(
            ctx.host.clone(),
            ctx.paths.clone(),
            ctx.settings.clone(),
            req.code,
            req.new_password,
        )
        .await;
    let response = match result {
        Ok(session) => {
            let mut response = json_response(json!({"token": session.token}));
            if let Ok(cookie) = HeaderValue::from_str(&auth::session_set_cookie(&session.cookie)) {
                response.headers_mut().insert(header::SET_COOKIE, cookie);
            }
            response
        }
        Err(auth::SetupError::NotAvailable) => {
            ApiError::new(StatusCode::CONFLICT, "setup.not_available").into_response()
        }
        Err(auth::SetupError::CodeInvalid {
            show_help,
            code_renewed,
        }) => ApiError::new(StatusCode::FORBIDDEN, "setup.code_invalid")
            .params(json!({"show_help": show_help, "code_renewed": code_renewed}))
            .into_response(),
        Err(auth::SetupError::RateLimited { retry_after_s }) => {
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "login.rate_limited")
                .params(json!({"retry_after_s":retry_after_s}))
                .into_response()
        }
        Err(auth::SetupError::Rule(rule)) => {
            ApiError::new(StatusCode::BAD_REQUEST, "password.rule")
                .params(json!({"rule":rule.key()}))
                .into_response()
        }
        Err(auth::SetupError::Credentials(state)) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "login.credentials_unusable",
        )
        .params(json!({"state":state.to_json()}))
        .into_response(),
        Err(auth::SetupError::Unsaved(reason)) => {
            crate::log::error_json(
                Event::CredentialsUnsaved,
                &[("reason", reason.log_reason_object(json!({})))],
            );
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "login.credentials_unsaved",
            )
            .params(json!({"reason":reason.as_str()}))
            .into_response()
        }
    };
    with_common_headers(response)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn password_login(
    State((ctx, _)): State<AppState>,
    JsonBody(req): JsonBody<LoginRequest>,
) -> Response {
    let ctx2 = ctx.clone();
    let credentials = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.auth.credentials_lock.blocking_lock();
        ctx2.auth
            .load_state(ctx2.host.as_ref(), &ctx2.paths, &ctx2.settings)
    })
    .await
    .unwrap_or(CredentialState::Unreadable(
        auth::UnreadableReason::FileUnreadable,
    ));
    if !credentials.is_ok() {
        let response = match &credentials {
            CredentialState::SetupRequired => {
                ApiError::new(StatusCode::CONFLICT, "login.setup_required").into_response()
            }
            CredentialState::Unsaved(_) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "login.credentials_unsaved",
            )
            .params(json!({"reason": state_json_reason(&credentials)}))
            .into_response(),
            _ => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "login.credentials_unusable",
            )
            .params(json!({"state": credentials_state_json(&credentials)}))
            .into_response(),
        };
        return with_common_headers(response);
    }
    let outcome = ctx
        .auth
        .login(
            ctx.host.clone(),
            ctx.paths.clone(),
            ctx.settings.clone(),
            req.username.clone(),
            req.password.clone(),
        )
        .await;
    let response = match outcome {
        auth::LoginOutcome::Ok { cookie, token } => {
            crate::log::info(Event::LoginSucceeded, &[]);
            let body = json!({"token": token});
            let mut response = Response::new(Body::from(body.to_string()));
            *response.status_mut() = StatusCode::OK;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            if let Ok(value) = HeaderValue::from_str(&auth::session_set_cookie(&cookie)) {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            response
        }
        auth::LoginOutcome::Failed {
            show_reset_help,
            consecutive_failures,
        } => {
            crate::log::info_json(
                Event::LoginFailed,
                &[("consecutive_failures", json!(consecutive_failures))],
            );
            ApiError::new(StatusCode::UNAUTHORIZED, "login.failed")
                .params(json!({"show_reset_help": show_reset_help}))
                .into_response()
        }
        auth::LoginOutcome::RateLimited {
            retry_after_s,
            show_reset_help,
        } => {
            crate::log::info(
                Event::LoginRateLimited,
                &[("retry_after_s", &retry_after_s.to_string())],
            );
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "login.rate_limited")
                .params(json!({
                    "retry_after_s": retry_after_s,
                    "show_reset_help": show_reset_help,
                }))
                .into_response()
        }
        auth::LoginOutcome::Credentials(CredentialState::SetupRequired) => {
            ApiError::new(StatusCode::CONFLICT, "login.setup_required").into_response()
        }
        auth::LoginOutcome::Credentials(state) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "login.credentials_unusable",
        )
        .params(json!({"state": credentials_state_json(&state)}))
        .into_response(),
    };
    with_common_headers(response)
}

fn state_json_reason(state: &CredentialState) -> Value {
    match state {
        CredentialState::Unsaved(r) => json!(r.as_str()),
        _ => json!("file"),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordRequest {
    new_password: String,
}

async fn change_password(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<PasswordRequest>,
) -> Response {
    if session.0.is_none() {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        // password change runs under the credentials lock.
        let _guard = ctx2.auth.credentials_lock.blocking_lock();
        // A request that already passed the session middleware may have queued on this lock
        // while a concurrent key revoke rotated the `session_key`; recheck the request's own
        // cookie under the lock so such a stale session cannot change the password or mint a
        // fresh cookie.
        if ctx2
            .auth
            .check_session_at(ctx2.host.as_ref(), &ctx2.paths, &headers)
            .is_err()
        {
            return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
        }
        match ctx2
            .auth
            .change_password(ctx2.host.as_ref(), &ctx2.paths, &req.new_password)
        {
            Ok(issued) => {
                crate::log::info(Event::PasswordChanged, &[]);
                let mut response = json_response(json!({"token": issued.token}));
                if let Ok(value) = HeaderValue::from_str(&auth::session_set_cookie(&issued.cookie))
                {
                    response.headers_mut().insert(header::SET_COOKIE, value);
                }
                response
            }
            Err(auth::PasswordChangeError::Rule(rule)) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, rule.key()).into_response()
            }
            Err(auth::PasswordChangeError::Unsaved(reason)) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "password.unsaved")
                    .params(json!({"reason": reason.as_str()}))
                    .into_response()
            }
            Err(auth::PasswordChangeError::Credentials(state)) => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "login.credentials_unusable",
            )
            .params(json!({"state": credentials_state_json(&state)}))
            .into_response(),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

// ---- SSH sign-in -----------------------------------------

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SshChallengeRequest {}

/// `POST /api/ssh/challenge`: public, blind entropy. The issue bucket (capacity 10, one token
/// per 10 seconds) and the outstanding cap (256) bound the memory an unauthenticated caller
/// can consume.
async fn ssh_challenge(
    State((ctx, _)): State<AppState>,
    JsonBody(_req): JsonBody<SshChallengeRequest>,
) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.auth.credentials_lock.blocking_lock();
        if matches!(
            ctx2.auth
                .load_state(ctx2.host.as_ref(), &ctx2.paths, &ctx2.settings),
            CredentialState::SetupRequired
        ) {
            return Err(CredentialState::SetupRequired);
        }
        Ok::<_, CredentialState>(ctx2.auth.ssh_challenges.issue(ctx2.host.as_ref()))
    })
    .await;
    let result = match result {
        Ok(Err(CredentialState::SetupRequired)) => {
            return ApiError::new(StatusCode::CONFLICT, "login.setup_required").into_response()
        }
        Ok(Err(state)) => {
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "login.credentials_unusable",
            )
            .params(json!({"state":state.to_json()}))
            .into_response()
        }
        Ok(Ok(result)) => Ok(result),
        Err(error) => Err(error),
    };
    let issued = match result {
        Ok(Ok(issued)) => issued,
        Ok(Err(crate::ssh_login::IssueError::Busy)) => {
            return ApiError::new(StatusCode::TOO_MANY_REQUESTS, "ssh.challenge_busy")
                .into_response()
        }
        _ => return internal().into_response(),
    };
    let body = json!({
        "challenge_id": issued.id,
        "challenge": issued.message,
        "namespace": SSH_NAMESPACE,
        "expires_in_s": SSH_CHALLENGE_TTL.as_secs(),
    });
    with_common_headers(json_response(body))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SshLoginRequest {
    challenge_id: String,
    signature: String,
}

/// `POST /api/ssh/login`. The field limits are explicit — `JsonBody` only checks the media
/// type, the total body and the decode.
async fn ssh_login_route(
    State((ctx, _)): State<AppState>,
    JsonBody(req): JsonBody<SshLoginRequest>,
) -> Response {
    if req.challenge_id.len() > SSH_CHALLENGE_ID_MAX || req.signature.len() > SSH_SIGNATURE_MAX {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let outcome = crate::ssh_login::ssh_login(
        ctx.auth.as_ref(),
        ctx.host.clone(),
        &ctx.paths,
        req.challenge_id,
        req.signature,
    )
    .await;
    let response = match outcome {
        crate::ssh_login::SshLoginOutcome::Ok {
            cookie,
            token,
            fingerprint,
            algorithm,
        } => {
            // the success line names the verified registered key's public fingerprint and
            // algorithm — nothing else.
            crate::log::info_json(
                Event::SshLoginSucceeded,
                &[
                    ("fingerprint", json!(fingerprint)),
                    ("algorithm", json!(algorithm)),
                ],
            );
            let body = json!({"token": token});
            let mut response = json_response(body);
            if let Ok(value) = HeaderValue::from_str(&auth::session_set_cookie(&cookie)) {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            response
        }
        crate::ssh_login::SshLoginOutcome::ChallengeInvalid => {
            ApiError::new(StatusCode::UNAUTHORIZED, "ssh.challenge_invalid").into_response()
        }
        crate::ssh_login::SshLoginOutcome::Failed {
            kind,
            fingerprint,
            algorithm,
            show_reset_help,
        } => {
            // The log line carries only the public fingerprint, the algorithm and the fixed
            // reason category: the consecutive-failure count stays in the shared bucket and
            // in the password login-shaped response params below.
            let mut fields: Vec<(&str, serde_json::Value)> =
                vec![("reason", json!(kind.log_reason()))];
            if let Some(fingerprint) = &fingerprint {
                fields.push(("fingerprint", json!(fingerprint)));
            }
            if let Some(algorithm) = &algorithm {
                fields.push(("algorithm", json!(algorithm)));
            }
            crate::log::info_json(Event::SshLoginFailed, &fields);
            // Parsing and verification share one error code; an unregistered key has its own.
            let key = if kind == crate::ssh_login::SshFailKind::KeyNotRegistered {
                "ssh.key_not_registered"
            } else {
                "ssh.login_failed"
            };
            ApiError::new(StatusCode::UNAUTHORIZED, key)
                .params(json!({"show_reset_help": show_reset_help}))
                .into_response()
        }
        crate::ssh_login::SshLoginOutcome::RateLimited {
            retry_after_s,
            show_reset_help,
        } => {
            crate::log::info(
                Event::LoginRateLimited,
                &[("retry_after_s", &retry_after_s.to_string())],
            );
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "login.rate_limited")
                .params(json!({
                    "retry_after_s": retry_after_s,
                    "show_reset_help": show_reset_help,
                }))
                .into_response()
        }
        crate::ssh_login::SshLoginOutcome::Credentials(CredentialState::SetupRequired) => {
            ApiError::new(StatusCode::CONFLICT, "login.setup_required").into_response()
        }
        crate::ssh_login::SshLoginOutcome::Credentials(state) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "login.credentials_unusable",
        )
        .params(json!({"state": credentials_state_json(&state)}))
        .into_response(),
    };
    with_common_headers(response)
}

/// `GET /api/ssh/keys`: needs a session; a must-change session may read the (necessarily
/// empty) list.
async fn ssh_keys_list(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
) -> Response {
    if session.0.is_none() {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        ctx2.auth
            .snapshot_credentials(ctx2.host.as_ref(), &ctx2.paths)
    })
    .await;
    match result {
        Ok(Ok(credentials)) => {
            let mut keys = credentials.ssh_keys.clone();
            keys.sort_by(|a, b| a.added_at.cmp(&b.added_at).then(a.id.cmp(&b.id)));
            let items: Vec<Value> = keys.iter().map(|key| key.to_item()).collect();
            json_response(json!({"items": items}))
        }
        // An unreadable file cannot validate any session — the same answer
        // `check_session_at` gives.
        _ => ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SshKeyRequest {
    public_key: String,
    #[serde(default)]
    comment: String,
}

/// The whole critical section of the two key-changing endpoints: under the
/// `credentials_lock`, revalidate the request's own cookie against a fresh read, then
/// read-modify-write the credentials in one atomic publication.
fn ssh_key_write(ctx: &Arc<Ctx>, headers: &HeaderMap, change: KeyChange) -> Response {
    let _guard = ctx.auth.credentials_lock.blocking_lock();
    // The middleware check may have passed before this request queued on the lock; a
    // concurrent revoke rotated the `session_key` meanwhile, so a stale session must be
    // refused here rather than write or re-mint anything.
    if ctx
        .auth
        .check_session_at(ctx.host.as_ref(), &ctx.paths, headers)
        .is_err()
    {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    let Ok(mut credentials) = ctx.auth.snapshot_credentials(ctx.host.as_ref(), &ctx.paths) else {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    };
    match change {
        KeyChange::Add { line, comment } => {
            match crate::ssh_login::add_ssh_key(
                ctx.host.as_ref(),
                &mut credentials,
                &line,
                &comment,
            ) {
                Ok(item) => {
                    match auth::write_credentials(ctx.host.as_ref(), &ctx.paths, &credentials) {
                        Ok(()) => {
                            crate::log::info_json(
                                Event::SshKeyAdded,
                                &[
                                    ("fingerprint", item["fingerprint"].clone()),
                                    ("algorithm", item["algorithm"].clone()),
                                ],
                            );
                            json_response_status(StatusCode::CREATED, item)
                        }
                        Err(reason) => ssh_keys_unsaved(reason),
                    }
                }
                Err(crate::ssh_login::AddKeyError::Invalid(kind)) => {
                    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "ssh.key_invalid")
                        .params(json!({"reason": kind.reason()}))
                        .into_response()
                }
                Err(crate::ssh_login::AddKeyError::Exists(fingerprint)) => {
                    ApiError::new(StatusCode::CONFLICT, "ssh.key_exists")
                        .params(json!({"fingerprint": fingerprint}))
                        .into_response()
                }
                Err(crate::ssh_login::AddKeyError::Limit) => {
                    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "ssh.key_limit").into_response()
                }
            }
        }
        KeyChange::Remove { id } => {
            match crate::ssh_login::remove_ssh_key(&mut credentials, &id) {
                Ok(fingerprint) => {
                    // One atomic publication removes the key and rotates the
                    // `session_key`; only after it succeeded are the
                    // logged-out cookies cleared and a new cookie issued.
                    match auth::write_credentials(ctx.host.as_ref(), &ctx.paths, &credentials) {
                        Ok(()) => {
                            ctx.auth.clear_logged_out(ctx.host.as_ref(), &ctx.paths);
                            crate::log::info_json(
                                Event::SshKeyRemoved,
                                &[("fingerprint", json!(fingerprint))],
                            );
                            let issued =
                                auth::issue_session(&credentials.session_key, ctx.host.wall_ms());
                            let mut response = json_response(json!({"token": issued.token}));
                            if let Ok(value) =
                                HeaderValue::from_str(&auth::session_set_cookie(&issued.cookie))
                            {
                                response.headers_mut().insert(header::SET_COOKIE, value);
                            }
                            response
                        }
                        Err(reason) => ssh_keys_unsaved(reason),
                    }
                }
                Err(crate::ssh_login::RemoveKeyError::NotFound) => {
                    ApiError::new(StatusCode::NOT_FOUND, "ssh.key_not_found").into_response()
                }
            }
        }
    }
}

enum KeyChange {
    Add { line: String, comment: String },
    Remove { id: String },
}

/// credentials-save-failure notice-mode write failure of the key endpoints: the old file
/// bytes stay the value of record, so the response reports the failure as it is.
fn ssh_keys_unsaved(reason: auth::UnsavedReason) -> Response {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "ssh.keys_unsaved")
        .params(json!({"reason": reason.as_str()}))
        .into_response()
}

/// `POST /api/ssh/keys`. The must-change gate blocks every non-GET, non-`/api/password`
/// route, so registration can only happen after the initial password was changed.
async fn ssh_keys_add(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<SshKeyRequest>,
) -> Response {
    if session.0.is_none() {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    if req.public_key.len() > SSH_KEY_LINE_MAX || req.comment.len() > SSH_COMMENT_MAX {
        return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "ssh.key_invalid")
            .params(json!({"reason": crate::ssh_login::KeyInvalid::BadFormat.reason()}))
            .into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        ssh_key_write(
            &ctx2,
            &headers,
            KeyChange::Add {
                line: req.public_key,
                comment: req.comment,
            },
        )
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

/// `DELETE /api/ssh/keys/{id}`: one atomic revoke plus `session_key` rotation; the response
/// carries the caller's new cookie.
async fn ssh_key_delete(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if session.0.is_none() {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        ssh_key_write(&ctx2, &headers, KeyChange::Remove { id })
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
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
        let modules = plan::read_modules(h1.as_ref());
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
    let modules = bounded_reading(move || plan::read_modules(h3.as_ref()));
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

    // Host GPUs.
    let mut gpus_json = json!([]);
    let mut gpus_readable = true;
    match gpus_result {
        Ok(gpus) => {
            let listed = catalog::gpu_list(
                &items,
                &gpus,
                crate::gpu_table::GPUS,
                crate::gpu_table::VERSIONS,
            );
            gpus_json = catalog::gpu_readings_json(&listed);
        }
        Err(e) => {
            gpus_readable = false;
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
        }
    }

    let any_unreadable = !unreadable.is_empty();
    let items_json: Vec<Value> = if any_unreadable {
        Vec::new()
    } else {
        items.iter().map(|v| json!({"version": v.text()})).collect()
    };
    let open_unbuildable: Vec<&str> = if any_unreadable {
        Vec::new()
    } else {
        items
            .iter()
            .filter(|v| {
                matches!(
                    catalog::official_verdict(
                        v,
                        kernel.as_deref().unwrap_or_default(),
                        &interfaces,
                        Some(catalog::KernelModuleType::Open)
                    ),
                    catalog::BranchVerdict::Unbuildable(_)
                )
            })
            .map(|v| v.text())
            .collect()
    };
    json_response(json!({
        "items": items_json,
        "open_unbuildable": open_unbuildable,
        "unreadable": unreadable,
        "gpus": if gpus_readable { gpus_json } else { json!([]) },
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

// ---- upload creation / file upload / current upload
// ----------------------------------------------------------

async fn open_upload(State((ctx, _)): State<AppState>, headers: HeaderMap) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        match crate::upload::open_group(ctx2.host.as_ref(), &ctx2.paths, &ctx2.upload) {
            Ok(group) => json_response_status(StatusCode::CREATED, json!({"group_id": group})),
            Err(crate::upload::UploadReject::Busy) => {
                ApiError::new(StatusCode::CONFLICT, "upload.busy").into_response()
            }
            Err(crate::upload::UploadReject::GroupReplaced) => {
                ApiError::new(StatusCode::CONFLICT, "upload.group_replaced").into_response()
            }
            Err(crate::upload::UploadReject::Unauthorized) => {
                ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response()
            }
            Err(crate::upload::UploadReject::Rejected {
                file_name,
                reason,
                status,
                params,
            }) => upload_reject_response(&file_name, reason, status, params),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

fn upload_reject_response(file_name: &str, reason: &str, status: u16, params: Value) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
    let mut params = params;
    if let Some(map) = params.as_object_mut() {
        map.insert("file_name".into(), json!(file_name));
        map.insert("reason".into(), json!(reason));
    }
    ApiError::new(code, "upload.rejected")
        .params(params)
        .into_response()
}

async fn receive_upload_file(
    State((ctx, _)): State<AppState>,
    AxumPath(group_id): AxumPath<String>,
    request: Request,
) -> Response {
    // Parameter validation: the group id and the file name are checked before anything
    // is looked up or written; a name that is not valid UTF-8 after percent decoding, empty,
    // longer than 255 bytes or containing `/` or a control character is a format error.
    if !store::group_id_ok(&group_id) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let query = request.uri().query().unwrap_or("").to_string();
    let name = match query_param(&query, "name") {
        QueryParam::Value(name) => name,
        // Absent and not-valid-UTF-8 are both a format error here, but they stay distinct at
        // the parsing boundary.
        QueryParam::Missing | QueryParam::Invalid => {
            return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response()
        }
    };
    if !crate::upload::name_ok(&name) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let content_length = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let headers = request.headers().clone();
    let body = request.into_body();
    let ctx2 = ctx.clone();
    let session_ctx = ctx.clone();
    let session_ok = move || {
        session_ctx
            .auth
            .check_session_at(session_ctx.host.as_ref(), &session_ctx.paths, &headers)
            .is_ok()
    };
    let result = crate::upload::receive(
        ctx2.host.clone(),
        ctx2.paths.clone(),
        ctx2.startup.host_data_dir.clone(),
        group_id,
        name,
        content_length,
        body,
        ctx2.upload.clone(),
        &ctx2.process_lock,
        session_ok,
    )
    .await;
    match result {
        Ok((file, group)) => {
            let _ = &group;
            json_response(json!({"file": file, "group": group}))
        }
        Err(crate::upload::UploadReject::Busy) => {
            ApiError::new(StatusCode::CONFLICT, "upload.busy").into_response()
        }
        Err(crate::upload::UploadReject::GroupReplaced) => {
            ApiError::new(StatusCode::CONFLICT, "upload.group_replaced").into_response()
        }
        Err(crate::upload::UploadReject::Unauthorized) => {
            ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response()
        }
        Err(crate::upload::UploadReject::Rejected {
            file_name,
            reason,
            status,
            params,
        }) => {
            if reason == "interrupted" {
                ApiError::new(StatusCode::BAD_REQUEST, "upload.interrupted")
                    .params(json!({"file_name": file_name}))
                    .into_response()
            } else {
                upload_reject_response(&file_name, reason, status, params)
            }
        }
    }
}

async fn current_upload(State((ctx, _)): State<AppState>) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        match ctx2.upload.current() {
            Some(group) => json_response(json!(crate::upload::summary(
                ctx2.host.as_ref(),
                &ctx2.paths,
                &group
            ))),
            None => json_response(Value::Null),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
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

// ---- plan creation --------------------------------------------------------------------

/// plan creation's request body is `{source}`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanRequest {
    source: SourceRequest,
}

async fn create_plan(
    State((ctx, _)): State<AppState>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<PlanRequest>,
) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        match plan::precheck(&ctx2, &req.source) {
            Ok((Precheck::Ready(dialog), snapshot)) => {
                let plan_id = plan::remember(&ctx2, snapshot);
                json_response(json!({
                    "result": "ready",
                    "plan_id": plan_id,
                    "dialog": dialog,
                }))
            }
            Ok((other, _)) => plan_response(other),
            Err(e) => source_invalid_response(e),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

fn plan_response(precheck: Precheck) -> Response {
    match precheck {
        Precheck::Ready(dialog) => json_response(json!({"result": "ready", "dialog": dialog})),
        Precheck::Blocked { failures, gpu } => {
            let mut obj = serde_json::Map::new();
            obj.insert("result".into(), json!("blocked"));
            obj.insert("failures".into(), json!(failures));
            obj.insert(
                "gpu".into(),
                match gpu {
                    Some(g) => json!(g),
                    None => Value::Null,
                },
            );
            json_response(Value::Object(obj))
        }
        Precheck::Uncomputable { items } => json_response(json!({
            "result": "uncomputable",
            "items": items.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
        })),
    }
}

fn source_invalid_response(e: plan::SourceInvalid) -> Response {
    use plan::SourceInvalid as S;
    match e {
        S::SetupBlocked => ApiError::new(StatusCode::CONFLICT, "setup.blocked").into_response(),
        S::RequestInvalid => {
            ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response()
        }
        S::VersionFormat => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.version_format",
        )
        .into_response(),
        S::VersionBelow470 => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.version_below_470",
        )
        .into_response(),
        S::BranchUnbuildable { branch, reasons } => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.branch_unbuildable",
        )
        .params(json!({
            "branch": branch,
            // Every reason field has the `{key, params}` shape; these phrases have no
            // placeholder.
            "reasons": reasons
                .iter()
                .map(|key| json!({"key": key, "params": {}}))
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        S::BranchUndecidable { unreadable } => ApiError::new(
            StatusCode::CONFLICT,
            "plan.source_invalid.branch_undecidable",
        )
        .params(json!({
            "unreadable": unreadable.iter().map(|u| u.to_json()).collect::<Vec<_>>(),
        }))
        .into_response(),
        S::GpuUndecidable { unreadable } => {
            ApiError::new(StatusCode::CONFLICT, "plan.source_invalid.gpu_undecidable")
                .params(json!({
                    "unreadable": unreadable.iter().map(|u| u.to_json()).collect::<Vec<_>>(),
                }))
                .into_response()
        }
        S::GpuUnsupported { gpus, conflict } => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.gpu_unsupported",
        )
        .params(json!({"gpus": gpus, "conflict": conflict}))
        .into_response(),
        S::GpusChanged => ApiError::new(StatusCode::CONFLICT, "plan.changed")
            .params(json!({"changed": ["gpus"]}))
            .into_response(),
        S::UploadGone => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.upload_group_gone",
        )
        .into_response(),
        S::UploadIncomplete { missing } => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.upload_incomplete",
        )
        .params(json!({"missing": missing}))
        .into_response(),
        S::UploadProblem => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.upload_problem",
        )
        .into_response(),
        S::UploadVersionRequired => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.upload_version_required",
        )
        .into_response(),
        S::UploadVersionNotCandidate { version } => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.upload_version_not_candidate",
        )
        .params(json!({"version": version}))
        .into_response(),
        S::ToolkitTooOld {
            driver_version,
            toolkit_version,
        } => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan.source_invalid.toolkit_too_old",
        )
        .params(json!({
            "driver_version": driver_version,
            "toolkit_version": toolkit_version,
        }))
        .into_response(),
        S::LocalRowMissing => ApiError::new(
            StatusCode::NOT_FOUND,
            "plan.source_invalid.local_row_missing",
        )
        .into_response(),
    }
}

// ---- task submission -------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskRequest {
    plan_id: String,
    button: plan::Button,
}

async fn submit_task(
    State((ctx, _)): State<AppState>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<TaskRequest>,
) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        if !store::group_id_ok(&req.plan_id) {
            return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
        }
        match plan::submit(&ctx2, &req.plan_id, req.button) {
            Ok(task_id) => json_response_status(StatusCode::ACCEPTED, json!({"task_id": task_id})),
            Err(reject) => submit_reject_response(reject),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

fn submit_reject_response(reject: SubmitReject) -> Response {
    match reject {
        SubmitReject::Used => ApiError::new(StatusCode::CONFLICT, "plan.used").into_response(),
        SubmitReject::SetupBlocked => {
            ApiError::new(StatusCode::CONFLICT, "setup.blocked").into_response()
        }
        SubmitReject::ButtonInvalid => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "plan.button_invalid").into_response()
        }
        SubmitReject::TaskRunning => {
            ApiError::new(StatusCode::CONFLICT, "task.running").into_response()
        }
        SubmitReject::PrecheckFailed { failures } => {
            ApiError::new(StatusCode::CONFLICT, "plan.precheck_failed")
                .params(json!({"failures": failures}))
                .into_response()
        }
        SubmitReject::Uncomputable { items } => {
            ApiError::new(StatusCode::CONFLICT, "plan.uncomputable")
                .params(json!({
                    "items": items.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                }))
                .into_response()
        }
        SubmitReject::UploadBusy => {
            ApiError::new(StatusCode::CONFLICT, "upload.busy").into_response()
        }
        SubmitReject::Changed { changed } => ApiError::new(StatusCode::CONFLICT, "plan.changed")
            .params(json!({"changed": changed}))
            .into_response(),
        SubmitReject::Failed { reason, raw } => {
            let mut error = ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "submit.failed")
                .params(json!({"reason": {"key": reason.key, "params": reason.params}}));
            if let Some(raw) = raw {
                error = error.raw(raw);
            }
            error.into_response()
        }
    }
}

// ---- task list / task detail -------------------------------------------------------------

async fn list_tasks(State((ctx, _)): State<AppState>, headers: HeaderMap) -> Response {
    // The whole response is bounded by `READING_TIMEOUT`: while a pause or a drain holds the
    // OS locks the endpoint answers with the timeout reason instead of blocking, and the
    // response work that follows the reconcile shares the same budget.
    match whole_response(&ctx, headers, |ctx, reconciled, reads| {
        tasks_response(ctx, reconciled, reads)
    })
    .await
    {
        WholeResponse::Ready(response) => with_common_headers(response),
        WholeResponse::Failed => internal().into_response(),
        WholeResponse::TimedOut => with_common_headers(tasks_response(
            &ctx,
            Some(crate::tasks::Reconciled::unreadable(reading_timed_out())),
            false,
        )),
    }
}

fn task_source_json(state: Option<&TaskState>) -> Value {
    match state.and_then(|s| s.plan.as_ref()) {
        None => Value::Null,
        Some(plan) => match &plan.source {
            crate::task_state::PlanSource::Official => json!({"kind": "official"}),
            crate::task_state::PlanSource::Upload { fingerprint, .. } => {
                json!({"kind": "upload", "fingerprint": fingerprint})
            }
            crate::task_state::PlanSource::Local {
                row_kind,
                fingerprint,
                ..
            } => {
                let mut obj = serde_json::Map::new();
                obj.insert("kind".into(), json!("local"));
                obj.insert(
                    "row_kind".into(),
                    json!(match row_kind {
                        crate::task_state::RowKind::Official => "official",
                        crate::task_state::RowKind::Upload => "upload",
                        crate::task_state::RowKind::Original => "original",
                    }),
                );
                if let Some(f) = fingerprint {
                    obj.insert("fingerprint".into(), json!(f));
                }
                Value::Object(obj)
            }
        },
    }
}

fn tasks_response(
    ctx: &Ctx,
    reconciled: Option<crate::tasks::Reconciled>,
    reads: bool,
) -> Response {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    // The timed-out path answers without another host read.
    let state_dir_unreadable = if reads {
        h.read_dir(&p.state_dir()).err().map(|e| {
            json!({
                "reason": {"key": match e.kind() {
                    std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
                    _ => REASON_IO_ERROR,
                }, "params": {}},
            })
        })
    } else {
        Some(json!({
            "reason": {"key": REASON_TIMED_OUT, "params": {"seconds": READING_TIMEOUT.as_secs()}},
        }))
    };
    let Some(reconciled) = reconciled else {
        // `pid: host` failed: no reconciliation, no writes; the unit query is reported as
        // unreadable.
        return json_response(json!({
            "tasks": [],
            "active_task": {"unknown": startup_check_reason()},
            "unit_query": {"reason": startup_check_reason()},
            "state_dir": state_dir_unreadable,
        }));
    };
    let mut tasks = Vec::new();
    let mut sorted = reconciled.tasks.clone();
    sorted.sort_by(|a, b| b.id.cmp(&a.id));
    for t in &sorted {
        let times = crate::tasks::project_task_times(t.status, t.state.as_ref());
        let (source, version, phase) = match &t.state {
            Some(state) => (
                task_source_json(Some(state)),
                state.plan.as_ref().map(|p| p.version.clone()),
                state.phase.map(|p| p.as_str()),
            ),
            None => (Value::Null, None, None),
        };
        let mut obj = serde_json::Map::new();
        obj.insert("task_id".into(), json!(t.id));
        obj.insert("status".into(), json!(t.status_for_ui()));
        obj.insert("phase".into(), json!(phase));
        obj.insert("source".into(), source);
        obj.insert(
            "version".into(),
            version.map(Value::from).unwrap_or(Value::Null),
        );
        obj.insert("created_at".into(), opt_millis(times.created_at));
        obj.insert("finished_at".into(), opt_millis(times.finished_at));
        obj.insert(
            "interrupt_discovered_at".into(),
            opt_millis(times.interrupt_discovered_at),
        );
        if let Some(state_read_error) = &t.state_read_error {
            obj.insert("state_read_error".into(), json!(state_read_error));
        }
        tasks.push(Value::Object(obj));
    }
    let active_task = match reconciled.active.blocker() {
        None => Value::Null,
        Some(crate::tasks::Blocker::Running { task_ids }) => match task_ids.first() {
            Some(id) => {
                let t = reconciled.state_of(id);
                let status = t
                    .map(|t| t.status_for_ui())
                    .unwrap_or_else(|| "UNKNOWN".into());
                json!({"task_id": id, "status": status})
            }
            None => Value::Null,
        },
        Some(crate::tasks::Blocker::Unreadable { reason }) => json!({
            "unknown": {"key": reason.key, "params": reason.params},
        }),
    };
    let unit_query = reconciled
        .unit_query_error
        .as_ref()
        .map(|r| json!({"reason": {"key": r.key, "params": r.params}}));
    json_response(json!({
        "tasks": tasks,
        "active_task": active_task,
        "unit_query": unit_query,
        "state_dir": state_dir_unreadable,
    }))
}

async fn release_task(
    State((ctx, _)): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if !store::task_id_ok(&id) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx, &headers) {
            return response;
        }
        if !ctx.startup.pid_host_ok {
            return ApiError::new(StatusCode::CONFLICT, "release.state_unknown")
                .params(json!({"reason": startup_check_reason()}))
                .into_response();
        }
        let h = ctx.host.as_ref();
        let _control = match crate::admission::control_lock(h, crate::admission::LOCK_WAIT) {
            Ok(lock) => lock,
            Err(blocked) => {
                return ApiError::new(StatusCode::CONFLICT, "release.lock_busy")
                    .params(json!({"reason": {"key": blocked.key, "params": blocked.params}}))
                    .into_response();
            }
        };
        match crate::tasks::release_recovery(h, &ctx.paths, &id) {
            Ok(_) => {
                terminal_task_detail(&ctx, &id, None).unwrap_or_else(|| internal().into_response())
            }
            Err(refusal) => {
                let mut error = ApiError::new(StatusCode::CONFLICT, refusal.key);
                error.raw = refusal.raw;
                error.into_response()
            }
        }
    })
    .await;
    match result {
        Ok(response) => response,
        Err(_) => internal().into_response(),
    }
}

async fn task_detail(
    State((ctx, _)): State<AppState>,
    AxumPath(id): AxumPath<String>,
    request: Request,
) -> Response {
    let query = request.uri().query().unwrap_or("").to_string();
    let after = match query_param(&query, "after") {
        QueryParam::Missing => None,
        QueryParam::Invalid => {
            return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response()
        }
        QueryParam::Value(v) => {
            // A strict decimal u64: a plus sign or any other byte is a format error, never an
            // accepted value.
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
            }
            match v.parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid")
                        .into_response()
                }
            }
        }
    };
    if !store::task_id_ok(&id) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    // A terminal record's outcome is fixed, so its detail (including any
    // persisted release marker) is answered from the state file; the whole-host
    // reconciliation runs only while a task is running or its record is not
    // trustworthy. The projection keeps the reconciled shape.
    if ctx.startup.pid_host_ok {
        let ctx2 = ctx.clone();
        let id2 = id.clone();
        if let Ok(Some(response)) =
            tokio::task::spawn_blocking(move || terminal_task_detail(&ctx2, &id2, after)).await
        {
            return with_common_headers(response);
        }
    }
    // The reconcile and the detail work share one `READING_TIMEOUT`, so a held pause/drain
    // lock cannot block this read.
    let id_for_work = id.clone();
    let headers = request.headers().clone();
    match whole_response(&ctx, headers, move |ctx, reconciled, reads| {
        reconciled_task_response(ctx, &id_for_work, after, reconciled, reads)
    })
    .await
    {
        WholeResponse::Ready(response) => with_common_headers(response),
        WholeResponse::Failed => internal().into_response(),
        WholeResponse::TimedOut => with_common_headers(reconciled_task_response(
            &ctx,
            &id,
            after,
            Some(crate::tasks::Reconciled::unreadable(reading_timed_out())),
            false,
        )),
    }
}

fn reconciled_task_response(
    ctx: &Ctx,
    id: &str,
    after: Option<u64>,
    reconciled: Option<crate::tasks::Reconciled>,
    reads: bool,
) -> Response {
    if !reads {
        // The budget passed: answer the unknown shape without touching the host again, and
        // never as a missing task.
        return unknown_task_response(
            id,
            None,
            reconciled
                .as_ref()
                .and_then(|r| r.unit_query_error.as_ref()),
        );
    }
    let Some(reconciled) = reconciled else {
        return unreconciled_task_detail(ctx, id, after);
    };
    let Some(t) = reconciled.state_of(id) else {
        // only a **trusted complete absence** is a 404. An incomplete or unreadable
        // collection, a running unit and any id the listing still carries stay UNKNOWN
        // instead of disappearing from the API.
        let listed = reconciled.tasks.iter().any(|t| t.id == id);
        let running = matches!(
            &reconciled.active,
            crate::tasks::ActiveState::Running { task_ids } if task_ids.iter().any(|t| t == id)
        );
        let unproven = !reconciled.collection_complete
            || reconciled.unit_query_error.is_some()
            || matches!(
                reconciled.active,
                crate::tasks::ActiveState::Unreadable { .. }
            );
        if listed || running || unproven {
            return unknown_task_response(
                id,
                if running { Some("missing") } else { None },
                reconciled.unit_query_error.as_ref(),
            );
        }
        return ApiError::new(StatusCode::NOT_FOUND, "not_found").into_response();
    };
    task_detail_response(ctx, id, after, t, reconciled.unit_query_error.as_ref())
}

/// The task detail detail of a record that is already terminal: it is read directly and
/// projected as if the reconciliation had listed it, without asking the host for the unit
/// list, the containers or the whole state directory. An unreadable or missing record returns
/// `None`, so the caller runs the full reconciliation (which may rebuild or report it). A
/// RUNNING record also returns `None`: only reconciliation may decide that it was
/// interrupted.
fn terminal_task_detail(ctx: &Ctx, id: &str, after: Option<u64>) -> Option<Response> {
    let state = match crate::task_state::read(ctx.host.as_ref(), &ctx.paths, id) {
        store::JsonRead::Ok(state) => state,
        _ => return None,
    };
    if state.status == Status::Running {
        return None;
    }
    let task = crate::tasks::ReconciledTask {
        id: id.to_string(),
        status: Some(state.status),
        phase: state.phase,
        state_note: state.state_note.clone(),
        state: Some(state),
        state_read_error: None,
    };
    Some(task_detail_response(ctx, id, after, &task, None))
}

/// The `UNKNOWN` detail of a task whose record could not be shown: a running unit without a
/// state file, or a collection that is not trustworthy.
fn unknown_task_response(
    id: &str,
    state_read_error: Option<&str>,
    unit_query_error: Option<&crate::task_state::ReasonValue>,
) -> Response {
    json_response(json!({
        "task_id": id,
        "status": "UNKNOWN",
        "state_read_error": state_read_error,
        "state_note": null,
        "recovery_material_kept": false,
        "phase": null,
        "created_at": null,
        "finished_at": null,
        "interrupt_discovered_at": null,
        "plan": null,
        "loaded_version_start": null,
        "loaded_version_end": null,
        "download": null,
        "replace": [],
        "checks": [],
        "restore": null,
        "error": null,
        "gpu_users": null,
        "nvidia_smi_gpus": null,
        "version_change": false,
        "notice": null,
        "log": {"lines": [], "next": 0, "reset": false},
        "unit_query": unit_query_error.map(|r| json!({
            "reason": {"key": r.key, "params": r.params},
        })),
    }))
}

/// task detail when reconciliation is skipped (`pid: host` failed): the state files and the
/// unit list are left untouched; only the state file and the log are read.
fn unreconciled_task_detail(ctx: &Ctx, id: &str, after: Option<u64>) -> Response {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    let read = crate::task_state::read(h, p, id);
    let (status, state_read_error, state) = match read {
        store::JsonRead::Ok(state) => (state.status.as_str().to_string(), Value::Null, Some(state)),
        store::JsonRead::Missing => {
            // A trusted disk `Missing` is a real absence: this path lists the state directory
            // directly, so the 404 stays.
            return ApiError::new(StatusCode::NOT_FOUND, "not_found").into_response();
        }
        other => (
            "UNKNOWN".to_string(),
            json!(store::state_read_error_key(&other)),
            None,
        ),
    };
    let times = crate::tasks::project_task_times(state.as_ref().map(|s| s.status), state.as_ref());
    let log = task_state::log_tail(h, p, id, after);
    let log_json = json!({
        "lines": log.lines,
        "next": log.next,
        "reset": log.reset,
    });
    let version_change = state.as_ref().map(version_change).unwrap_or(false);
    let notice = state.as_ref().and_then(notice_json);
    let body = json!({
        "task_id": id,
        "status": status,
        "state_read_error": state_read_error,
        "state_note": state.as_ref().and_then(|s| s.state_note.as_ref()).map(|n| json!(n)),
        "recovery_material_kept": h.lstat(&p.task_dir(id)).is_ok(),
        "released_by": state.as_ref().and_then(|s| s.released_by.as_ref()),
        "released_at": state.as_ref().and_then(|s| s.released_at),
        "phase": state.as_ref().and_then(|s| s.phase).map(|p| p.as_str()),
        "created_at": opt_millis(times.created_at),
        "finished_at": opt_millis(times.finished_at),
        "interrupt_discovered_at": opt_millis(times.interrupt_discovered_at),
        "plan": state.as_ref().and_then(|s| s.plan.as_ref().map(plan_json)),
        "loaded_version_start": state.as_ref().and_then(|s| s.loaded_version_start.as_ref().map(reading_json_generic)),
        "loaded_version_end": state.as_ref().and_then(|s| s.loaded_version_end.as_ref().map(reading_json_generic)),
        "download": state.as_ref().and_then(|s| s.download.as_ref()).map(|d| json!(d)),
        "replace": state.as_ref().map(|s| json!(s.replace)).unwrap_or(json!([])),
        "checks": state.as_ref().map(|s| json!(s.checks)).unwrap_or(json!([])),
        "restore": state.as_ref().and_then(|s| s.restore.as_ref()).map(|r| json!(r)),
        "error": state.as_ref().and_then(|s| s.error.as_ref()).map(|e| json!({
            "code": e.code,
            "params": e.params,
            "command": e.command,
            "exit": e.exit,
            "raw": e.raw,
        })),
        "gpu_users": state.as_ref().and_then(|s| s.gpu_users.as_ref()).map(|g| json!(g)),
        "nvidia_smi_gpus": state.as_ref().and_then(|s| s.nvidia_smi_gpus.as_ref()).map(|g| json!(g)),
        "version_change": version_change,
        "notice": notice,
        "log": log_json,
        "unit_query": {"reason": startup_check_reason()},
    });
    json_response(body)
}

/// Build the task detail body from one reconciled task.
fn task_detail_response(
    ctx: &Ctx,
    id: &str,
    after: Option<u64>,
    t: &crate::tasks::ReconciledTask,
    unit_query_error: Option<&crate::task_state::ReasonValue>,
) -> Response {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    let state = t.state.clone();
    let status = t.status_for_ui();
    let times = crate::tasks::project_task_times(t.status, state.as_ref());
    let version_change = state.as_ref().map(version_change).unwrap_or(false);
    let notice = state.as_ref().and_then(notice_json);
    let log = task_state::log_tail(h, p, id, after);
    let log_json = json!({
        "lines": log.lines,
        "next": log.next,
        "reset": log.reset,
    });
    let mut body = json!({
        "task_id": id,
        "status": status,
        "state_read_error": t.state_read_error,
        "state_note": t.state_note,
        "recovery_material_kept": h.lstat(&p.task_dir(id)).is_ok(),
        "released_by": state.as_ref().and_then(|s| s.released_by.as_ref()),
        "released_at": state.as_ref().and_then(|s| s.released_at),
        "phase": state.as_ref().and_then(|s| s.phase).map(|p| p.as_str()),
        "created_at": opt_millis(times.created_at),
        "finished_at": opt_millis(times.finished_at),
        "interrupt_discovered_at": opt_millis(times.interrupt_discovered_at),
        "plan": state.as_ref().and_then(|s| s.plan.as_ref().map(plan_json)),
        "loaded_version_start": state.as_ref().and_then(|s| s.loaded_version_start.as_ref().map(reading_json_generic)),
        "loaded_version_end": state.as_ref().and_then(|s| s.loaded_version_end.as_ref().map(reading_json_generic)),
        "download": state.as_ref().and_then(|s| s.download.as_ref()).map(|d| json!(d)),
        "replace": state.as_ref().map(|s| json!(s.replace)).unwrap_or(json!([])),
        "checks": state.as_ref().map(|s| json!(s.checks)).unwrap_or(json!([])),
        "restore": state.as_ref().and_then(|s| s.restore.as_ref()).map(|r| json!(r)),
        "error": state.as_ref().and_then(|s| s.error.as_ref()).map(|e| json!({
            "code": e.code,
            "params": e.params,
            "command": e.command,
            "exit": e.exit,
            "raw": e.raw,
        })),
        "gpu_users": state.as_ref().and_then(|s| s.gpu_users.as_ref()).map(|g| json!(g)),
        "nvidia_smi_gpus": state.as_ref().and_then(|s| s.nvidia_smi_gpus.as_ref()).map(|g| json!(g)),
        "version_change": version_change,
        "notice": notice,
        "log": log_json,
        "unit_query": unit_query_error.map(|r| json!({
            "reason": {"key": r.key, "params": r.params},
        })),
    });
    if status == "RUNNING" && state.as_ref().is_some_and(|s| s.waiting_gpu == Some(true)) {
        body["waiting_gpu"] = json!(true);
    }
    json_response(body)
}

fn reading_json_generic<T: serde::Serialize>(r: &crate::task_state::ReadingJson<T>) -> Value {
    json!(r)
}
fn plan_json(plan: &crate::task_state::Plan) -> Value {
    json!(plan)
}

/// version-change notice is computed at read time.
fn version_change(state: &TaskState) -> bool {
    if !matches!(
        state.status,
        Status::Succeeded | Status::StockRestored | Status::Failed
    ) {
        return false;
    }
    let start = state
        .loaded_version_start
        .as_ref()
        .and_then(|r| r.as_value())
        .cloned();
    let end = state
        .loaded_version_end
        .as_ref()
        .and_then(|r| r.as_value())
        .cloned();
    let (Some(start), Some(end)) = (start, end) else {
        return false;
    };
    if start == end {
        return false;
    }
    state
        .gpu_users
        .as_ref()
        .map(|g| !g.users.is_empty() || !g.errors.is_empty())
        .unwrap_or(false)
}

/// The result notification of a finished task.
fn notice_json(state: &TaskState) -> Option<Value> {
    match state.status {
        Status::Succeeded => {
            // The check passed, but a module of the effective pass may not have
            // come back: the success notice then names it and says what to do,
            // instead of leaving the failure only in the task log.
            let failed: Vec<&str> = state
                .replace
                .last()
                .map(|r| {
                    r.final_reloaded()
                        .iter()
                        .filter(|x| !x.ok)
                        .map(|x| x.module.as_str())
                        .collect()
                })
                .unwrap_or_default();
            let version = state
                .plan
                .as_ref()
                .map(|p| p.version.clone())
                .unwrap_or_default();
            if failed.is_empty() {
                Some(json!({
                    "key": "notice.update_complete",
                    "params": {"version": version},
                }))
            } else {
                Some(json!({
                    "key": "notice.update_complete",
                    "variant": "reload_warning",
                    "params": {"version": version, "modules": failed.join(", ")},
                }))
            }
        }
        Status::StockRestored => {
            let restore = state.restore.as_ref()?;
            let original = restore.original.as_ref()?;
            Some(json!({
                "key": "notice.restored",
                "params": {
                    "target": state.plan.as_ref().map(|p| p.version.clone()).unwrap_or_default(),
                    "original": original.version,
                    "date": original.saved_at,
                },
            }))
        }
        Status::Failed => {
            let error = state.error.as_ref()?;
            let code = error.code.as_str();
            let reload_failed = |pass: crate::task_state::Pass| {
                state
                    .replace
                    .iter()
                    .find(|r| r.pass == pass)
                    .map(|r| r.final_reloaded().iter().any(|x| !x.ok))
                    .unwrap_or(false)
            };
            match ErrorCode::parse(code) {
                Some(ErrorCode::GpuInUse) => Some(json!({
                    "key": "notice.gpu_in_use",
                    "variant": if error.params.get("occupancy").and_then(|v| v.as_str()) == Some("unreadable") {
                        "task_unreadable"
                    } else {
                        "task"
                    },
                    "params": {},
                })),
                Some(ErrorCode::ModuleUnloadRefused) => {
                    let mut params = json!({
                        "reload_failed": reload_failed(crate::task_state::Pass::Update),
                    });
                    if error.params.get("waited_in_use").and_then(|v| v.as_bool()) == Some(true) {
                        params["waited_in_use"] = json!(true);
                    }
                    Some(json!({
                        "key": "notice.unload_refused",
                        "params": params,
                    }))
                }
                Some(ErrorCode::RestoreAborted) => {
                    let cause = error
                        .params
                        .get("cause")
                        .and_then(|v| v.as_str())
                        .unwrap_or("busy");
                    let mut params = json!({
                        "reload_failed": reload_failed(crate::task_state::Pass::Restore),
                    });
                    if error.params.get("waited_in_use").and_then(|v| v.as_bool()) == Some(true) {
                        params["waited_in_use"] = json!(true);
                    }
                    Some(json!({
                        "key": "notice.restore_stopped",
                        "variant": cause,
                        "params": params,
                    }))
                }
                Some(ErrorCode::RestoreNotWritten) => Some(json!({
                    "key": "notice.restore_failed",
                    "variant": "before_write",
                    "params": {"reload_failed": reload_failed(crate::task_state::Pass::Restore)},
                })),
                Some(ErrorCode::RestoreCheckFailed) => Some(json!({
                    "key": "notice.restore_failed",
                    "variant": "written",
                    "params": {"reload_failed": false},
                })),
                Some(ErrorCode::NoUsableOriginal) => Some(json!({
                    "key": "notice.no_restore",
                    "params": {},
                })),
                Some(ErrorCode::SaveOriginalFailed) => Some(json!({
                    "key": "code.explanation",
                    "params": {"code": code, "reload_failed": false},
                })),
                Some(ErrorCode::WriteFailed) => Some(json!({
                    "key": "code.explanation",
                    "params": {"code": code, "reload_failed": reload_failed(crate::task_state::Pass::Update)},
                })),
                Some(ErrorCode::ReplaceDeadlineExceeded) => Some(json!({
                    "key": "code.explanation",
                    "params": {"code": code, "reload_failed": reload_failed(crate::task_state::Pass::Update)},
                })),
                Some(ErrorCode::DownloadFailed) | Some(ErrorCode::BuildFailed) => {
                    Some(json!({"key": "code.explanation", "params": {"code": code}}))
                }
                Some(ErrorCode::BuildTimeout) => {
                    Some(json!({"key": "code.explanation", "params": {"code": code}}))
                }
                None => None,
            }
        }
        Status::Running | Status::Interrupted => None,
    }
}

// ---- driver list / driver deletion / driver retention
// -------------------------------------------------------

async fn list_drivers(State((ctx, _)): State<AppState>, headers: HeaderMap) -> Response {
    // Reconciliation and listing share one response budget, including canonical reads.
    // When it expires, `delete_unavailable` prevents deletion based on incomplete facts.
    match whole_response(&ctx, headers, |ctx, reconciled, reads| {
        drivers_response(ctx, reconciled, reads)
    })
    .await
    {
        WholeResponse::Ready(response) => with_common_headers(response),
        WholeResponse::Failed => internal().into_response(),
        WholeResponse::TimedOut => with_common_headers(drivers_response(
            &ctx,
            Some(crate::tasks::Reconciled::unreadable(reading_timed_out())),
            false,
        )),
    }
}

fn driver_row_json(
    row: &drivers::Row,
    canonical: &Reading<String>,
    kernel: &str,
    keep: Option<bool>,
) -> Value {
    let record = row.record_ok();
    let status = drivers::status(row, canonical, kernel);
    let delete_blocked = drivers::delete_blocked(row, canonical);
    let can_update = drivers::can_update(row, kernel);
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(row.id));
    match record {
        Some(record) => {
            obj.insert(
                "kind".into(),
                json!(match record.kind {
                    crate::task_state::RowKind::Official => "official",
                    crate::task_state::RowKind::Upload => "upload",
                    crate::task_state::RowKind::Original => "original",
                }),
            );
            obj.insert("version".into(), json!(record.version));
            obj.insert(
                "fingerprint".into(),
                record
                    .fingerprint
                    .clone()
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            );
            obj.insert("created_at".into(), json!(record.created_at));
            obj.insert("size".into(), json!(record.size));
            obj.insert("target_kernel".into(), json!(record.target_kernel));
            obj.insert("status".into(), json!(status.as_str()));
            obj.insert(
                "keep".into(),
                keep.or(record.keep).map(Value::from).unwrap_or(Value::Null),
            );
            obj.insert("can_update".into(), json!(can_update));
            obj.insert(
                "delete_blocked".into(),
                delete_blocked.map(Value::from).unwrap_or(Value::Null),
            );
        }
        None => {
            for field in [
                "kind",
                "version",
                "fingerprint",
                "created_at",
                "size",
                "target_kernel",
            ] {
                obj.insert(field.into(), Value::Null);
            }
            obj.insert("status".into(), json!("UNKNOWN"));
            obj.insert("keep".into(), Value::Null);
            obj.insert("can_update".into(), json!(false));
            obj.insert("delete_blocked".into(), json!("unknown"));
        }
    }
    Value::Object(obj)
}

fn drivers_response(
    ctx: &Ctx,
    reconciled: Option<crate::tasks::Reconciled>,
    reads: bool,
) -> Response {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    // The timed-out path answers from the timeout reason alone: no host read is attempted
    // after the budget passed.
    let (canonical, kernel, rows) = if reads {
        let canonical = facts::canonical_sha256(h);
        let kernel = facts::kernel(h);
        let rows = drivers::list(h, p);
        (canonical, kernel, rows)
    } else {
        (
            facts::Reading::Unknown(REASON_TIMED_OUT),
            facts::Reading::Unknown(REASON_TIMED_OUT),
            Vec::new(),
        )
    };
    let kernel_str = kernel.value().cloned().unwrap_or_default();
    let rows_json: Vec<Value> = rows
        .iter()
        .map(|r| driver_row_json(r, &canonical, &kernel_str, None))
        .collect();
    let delete_unavailable = match reconciled.as_ref().map(|r| r.active.blocker()) {
        None => json!({
            "key": "task_state_unreadable",
            "reason": startup_check_reason(),
        }),
        Some(Some(crate::tasks::Blocker::Running { .. })) => json!({"key": "task_running"}),
        Some(Some(crate::tasks::Blocker::Unreadable { reason })) => json!({
            "key": "task_state_unreadable",
            "reason": {"key": reason.key, "params": reason.params},
        }),
        Some(None) => {
            if canonical.unknown_reason().is_some() {
                json!({"key": "canonical_unreadable"})
            } else {
                Value::Null
            }
        }
    };
    json_response(json!({
        "rows": rows_json,
        "kernel": kernel.value().cloned().map(Value::from).unwrap_or(Value::Null),
        "delete_unavailable": delete_unavailable,
    }))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteRequest {
    ids: Vec<String>,
}

async fn delete_drivers(
    State((ctx, _)): State<AppState>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<DeleteRequest>,
) -> Response {
    if req.ids.is_empty() {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        let h = ctx2.host.as_ref();
        let p = &ctx2.paths;
        for id in &req.ids {
            if !store::row_id_ok(id) {
                return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
            }
        }
        // `pid: host` failed: no reconciliation and no deletion.
        if !ctx2.startup.pid_host_ok {
            return ApiError::new(StatusCode::CONFLICT, "delete.task_state_unknown")
                .params(json!({
                    "reason": startup_check_reason(),
                }))
                .into_response();
        }
        // Cross-process ordering: the reconciliation, the state check and the deletion itself
        // run inside the global control lock, in the fixed order `process_lock -> control ->
        // execution`. The core is called directly because this call already holds the control
        // lock; taking it a second time on another descriptor would look busy.
        let _control = match crate::admission::control_lock(h, crate::admission::LOCK_WAIT) {
            Ok(lock) => lock,
            Err(blocked) => {
                return ApiError::new(StatusCode::CONFLICT, "delete.task_state_unknown")
                    .params(json!({"reason": {"key": blocked.key, "params": blocked.params}}))
                    .into_response()
            }
        };
        // The lock is kept until this critical section (including the row
        // deletion below) is finished; it is released when the guard drops.
        let reconciled = crate::tasks::reconcile(h, p);
        match reconciled.active.blocker() {
            Some(crate::tasks::Blocker::Running { .. }) => {
                return ApiError::new(StatusCode::CONFLICT, "delete.task_running").into_response()
            }
            Some(crate::tasks::Blocker::Unreadable { reason }) => {
                return ApiError::new(StatusCode::CONFLICT, "delete.task_state_unknown")
                    .params(json!({"reason": {"key": reason.key, "params": reason.params}}))
                    .into_response()
            }
            None => {}
        }
        let canonical = facts::canonical_sha256(h);
        let Reading::Value(canonical) = canonical else {
            return ApiError::new(StatusCode::CONFLICT, "delete.canonical_unreadable")
                .into_response();
        };
        let mut ids = req.ids.clone();
        ids.sort();
        ids.dedup();
        match drivers::delete(h, p, &ids, &canonical) {
            Ok(deleted) => {
                crate::log::info(
                    Event::DriversDeleted,
                    &[("rows", &deleted.len().to_string())],
                );
                json_response(json!({"deleted": deleted}))
            }
            Err(drivers::DeleteError::Missing(ids)) => {
                ApiError::new(StatusCode::NOT_FOUND, "delete.row_missing")
                    .params(json!({"ids": ids}))
                    .into_response()
            }
            Err(drivers::DeleteError::Refused(rows)) => {
                let rows_json: Vec<Value> = rows
                    .iter()
                    .map(|r| {
                        json!({
                            "id": r.id,
                            "reason": r.reason,
                            "version": r.version,
                            "kind": r.kind,
                        })
                    })
                    .collect();
                ApiError::new(StatusCode::CONFLICT, "delete.refused")
                    .params(json!({"rows": rows_json}))
                    .into_response()
            }
            Err(drivers::DeleteError::Failed { deleted, reason }) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "delete.failed")
                    .params(json!({"deleted": deleted, "reason": reason}))
                    .into_response()
            }
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KeepRequest {
    keep: bool,
}

async fn set_driver_keep(
    State((ctx, _)): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<KeepRequest>,
) -> Response {
    if !store::row_id_ok(&id) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        let h = ctx2.host.as_ref();
        let p = &ctx2.paths;
        match drivers::set_keep(h, p, &id, req.keep) {
            Ok(row) => {
                let canonical = facts::canonical_sha256(h);
                let kernel = facts::kernel(h);
                let value = driver_row_json(
                    &row,
                    &canonical,
                    kernel.value().map(|s| s.as_str()).unwrap_or(""),
                    Some(req.keep),
                );
                json_response(value)
            }
            Err(drivers::KeepError::Missing) => {
                ApiError::new(StatusCode::NOT_FOUND, "delete.row_missing").into_response()
            }
            Err(drivers::KeepError::Unreadable) => {
                ApiError::new(StatusCode::CONFLICT, "keep.row_unreadable").into_response()
            }
            Err(drivers::KeepError::NotOriginal) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "keep.not_original").into_response()
            }
            Err(drivers::KeepError::Unsaved(reason)) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "keep.unsaved")
                    .params(json!({"reason": reason}))
                    .into_response()
            }
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
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

async fn logout(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
) -> Response {
    let Some(session) = session.0 else {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    };
    let saved = ctx
        .auth
        .logout(ctx.host.as_ref(), &ctx.paths, &session, ctx.host.wall_ms());
    crate::log::info(Event::Logout, &[]);
    let mut response = if saved.is_ok() {
        json_response(json!({}))
    } else {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "logout.unsaved").into_response()
    };
    if let Ok(value) = HeaderValue::from_str(&auth::clear_set_cookie()) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    with_common_headers(response)
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
            users: vec![GpuUserRecord {
                name: "x".into(),
                kind: "docker".into(),
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

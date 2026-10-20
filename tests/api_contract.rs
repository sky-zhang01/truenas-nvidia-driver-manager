//! API guard ordering, request rejection, parameter validation, task responses, and response
//! headers.

mod common;

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use common::*;
use http_body::{Body as HttpBody, Frame};
use serde_json::json;
use tower::ServiceExt;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{
    self, CheckRecord, Pass, Phase, PlanSource, ReadingJson, ReasonJson, ReloadTargetsJson,
    ReloadedRecord, ReplaceRecord, Status, TaskState,
};

const API_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/password"),
    ("POST", "/api/password"),
    ("GET", "/api/home"),
    ("GET", "/api/versions"),
    ("POST", "/api/uploads"),
    ("GET", "/api/uploads/current"),
    (
        "PUT",
        "/api/uploads/00000000000000000000000000000000/files?name=x.run",
    ),
    ("POST", "/api/plans"),
    ("GET", "/api/tasks"),
    ("POST", "/api/tasks"),
    ("GET", "/api/tasks/20260928T101530Z-3f9a0c12"),
    ("POST", "/api/tasks/20260928T101530Z-3f9a0c12/release"),
    ("GET", "/api/drivers"),
    ("POST", "/api/drivers/delete"),
    ("PUT", "/api/drivers/20260928T101530Z-3f9a0c12-build/keep"),
    ("POST", "/api/logout"),
];

async fn setup() -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    signed_in_app(host).await
}

/// One request with fully controlled headers.
async fn raw_request(
    app: &TestApp,
    method: Method,
    uri: &str,
    extra: &[(header::HeaderName, &str)],
) -> (StatusCode, HeaderMap, serde_json::Value, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    builder = builder.header(header::HOST, "nas.example:8199");
    let mut saw_token = false;
    let mut session_cookie = None;
    for (name, value) in extra {
        if *name == header::COOKIE {
            session_cookie = value
                .split(';')
                .map(str::trim)
                .find_map(|part| part.strip_prefix("nvm_session=").map(str::to_string));
        }
        if name.as_str().eq_ignore_ascii_case("x-nvm-token") {
            saw_token = true;
        }
        builder = builder.header(name, *value);
    }
    if !saw_token {
        if let Some(cookie) = session_cookie {
            if let Some(token) = token_for(&cookie) {
                builder = builder.header("x-nvm-token", token);
            }
        }
    }
    let request = builder.body(Body::empty()).unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, headers, value, text)
}

#[tokio::test]
async fn the_origin_guard_refuses_missing_and_foreign_origins_before_anything() {
    let (app, _cookie) = setup().await;
    // A same-origin-shaped mutation without the Origin header is refused.
    let (status, _, body, _) = raw_request(&app, Method::POST, "/api/login", &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "auth.origin_mismatch");
    // `null` and any other host are refused as well.
    for origin in [
        "null",
        "http://elsewhere.example:8199",
        "http://nas.example:8200",
    ] {
        let (status, _, body, _) = raw_request(
            &app,
            Method::POST,
            "/api/login",
            &[(header::ORIGIN, origin)],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{origin}: {body}");
        assert_eq!(body["error"], "auth.origin_mismatch", "{origin}");
    }
    // Case-insensitive host equivalence is accepted (the login body decides).
    let (status, _, _, _) = raw_request(
        &app,
        Method::POST,
        "/api/login",
        &[
            (header::ORIGIN, "http://NAS.EXAMPLE:8199"),
            (header::CONTENT_TYPE, "application/json"),
        ],
    )
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
}

/// One real static page, so a navigation can reach a page (the default test app serves no static
/// files).
const SAMPLE_HTML: &[u8] =
    b"<!doctype html><html><head><title>Driver Manager</title></head><body><div id=\"app\"></div></body></html>";

fn static_router(app: &TestApp) -> axum::Router {
    let mut files = truenas_nvidia_driver_manager::web::StaticFiles::default();
    files.files.insert(
        "index.html".to_string(),
        (SAMPLE_HTML.to_vec().into(), "text/html; charset=utf-8"),
    );
    truenas_nvidia_driver_manager::web::router(app.ctx.clone(), files)
}

#[tokio::test]
async fn sec_fetch_metadata_no_longer_gates_routing_or_the_api() {
    let (app, _cookie) = setup().await;
    // A browser-labelled cross-site API read keeps the endpoint's own auth: anonymous is 401, and
    // the response still carries the CSP.
    let (status, headers, body, _) = raw_request(
        &app,
        Method::GET,
        "/api/home",
        &[
            (
                header::HeaderName::from_static("sec-fetch-site"),
                "cross-site",
            ),
            (
                header::HeaderName::from_static("sec-fetch-mode"),
                "navigate",
            ),
            (
                header::HeaderName::from_static("sec-fetch-dest"),
                "document",
            ),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "auth.required");
    assert!(headers
        .get(header::CONTENT_SECURITY_POLICY)
        .is_some_and(|v| v.to_str().unwrap().contains("frame-ancestors 'none'")));

    // The label changes nothing for a navigation: the same path answers the same way with and
    // without it.
    let (with_label, _, _, _) = raw_request(
        &app,
        Method::GET,
        "/",
        &[(
            header::HeaderName::from_static("sec-fetch-site"),
            "cross-site",
        )],
    )
    .await;
    let (without_label, _, _, _) = raw_request(&app, Method::GET, "/", &[]).await;
    assert_eq!(with_label, without_label);

    let (with_label, _, _, _) = raw_request(
        &app,
        Method::GET,
        "/nope.js",
        &[(
            header::HeaderName::from_static("sec-fetch-site"),
            "cross-site",
        )],
    )
    .await;
    let (without_label, _, _, _) = raw_request(&app, Method::GET, "/nope.js", &[]).await;
    assert_eq!(with_label, without_label);
    assert_eq!(with_label, StatusCode::NOT_FOUND);

    let (status, _, body, _) = raw_request(
        &app,
        Method::GET,
        "/api/login",
        &[
            (
                header::HeaderName::from_static("sec-fetch-site"),
                "cross-site",
            ),
            (
                header::HeaderName::from_static("sec-fetch-mode"),
                "navigate",
            ),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["logged_in"], false);

    // Non-API writes with an unsafe Origin return the two-line rejection text.
    let (status, headers, _, text) = raw_request(&app, Method::POST, "/", &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    assert!(text.contains("请求来自其他网站,已被拒绝。请直接打开本程序的地址。"));
    assert!(text.contains("The request came from another site and was refused."));
    assert!(headers
        .get(header::CONTENT_SECURITY_POLICY)
        .is_some_and(|v| v.to_str().unwrap().contains("frame-ancestors 'none'")));
}

#[tokio::test]
async fn a_legitimate_cross_site_top_level_document_navigation_reaches_static_files() {
    let (app, _cookie) = setup().await;
    let mut files = truenas_nvidia_driver_manager::web::StaticFiles::default();
    let sample_html = b"<!doctype html><html><head><title>Driver Manager</title></head><body><div id=\"app\"></div></body></html>";
    files.files.insert(
        "index.html".to_string(),
        (sample_html.to_vec().into(), "text/html; charset=utf-8"),
    );
    let local_router = truenas_nvidia_driver_manager::web::router(app.ctx.clone(), files);

    let nav_headers = &[
        (
            header::HeaderName::from_static("sec-fetch-site"),
            "cross-site",
        ),
        (
            header::HeaderName::from_static("sec-fetch-mode"),
            "navigate",
        ),
        (
            header::HeaderName::from_static("sec-fetch-dest"),
            "document",
        ),
    ];

    // GET / with legitimate top-level document navigation headers succeeds (200 OK) with exact
    // content & CSP.
    let mut builder = Request::builder().method(Method::GET).uri("/");
    builder = builder.header(header::HOST, "nas.example:8199");
    for (name, value) in nav_headers {
        builder = builder.header(name.clone(), *value);
    }
    let request = builder.body(Body::empty()).unwrap();
    let response = local_router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let csp = response
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(csp.contains("default-src 'none'"));
    let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    assert_eq!(bytes.as_ref(), sample_html);

    // HEAD / with the same legitimate navigation headers succeeds.
    let mut builder = Request::builder().method(Method::HEAD).uri("/");
    builder = builder.header(header::HOST, "nas.example:8199");
    for (name, value) in nav_headers {
        builder = builder.header(name.clone(), *value);
    }
    let request = builder.body(Body::empty()).unwrap();
    let response = local_router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
}

#[tokio::test]
async fn the_write_origin_check_and_the_framing_header_are_the_remaining_boundaries() {
    let (app, _cookie) = setup().await;
    let page = static_router(&app);

    // 1. A cross-site-labelled iframe navigation reaches the page (200); the CSP is what refuses to
    // display it in a frame.
    let mut builder = Request::builder().method(Method::GET).uri("/");
    builder = builder.header(header::HOST, "nas.example:8199");
    for (name, value) in [
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-dest", "iframe"),
    ] {
        builder = builder.header(name, value);
    }
    let response = page
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let csp = response
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(csp.contains("default-src 'none'"));
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap_or_default();
    assert_eq!(bytes.as_ref(), SAMPLE_HTML);

    // 2. A cross-site-labelled subresource GET is served like any other read.
    let mut builder = Request::builder().method(Method::GET).uri("/");
    builder = builder.header(header::HOST, "nas.example:8199");
    for (name, value) in [
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-mode", "no-cors"),
        ("sec-fetch-dest", "script"),
    ] {
        builder = builder.header(name, value);
    }
    let response = page
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 3. A cross-site-labelled API GET keeps the endpoint auth (anonymous 401).
    let (status, _, body, _) = raw_request(
        &app,
        Method::GET,
        "/api/home",
        &[(
            header::HeaderName::from_static("sec-fetch-site"),
            "cross-site",
        )],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "auth.required");

    // 4. A non-API mutation without Origin is refused with the two-line text.
    let (status, _, _, text) = raw_request(&app, Method::POST, "/", &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(text.contains("请求来自其他网站"));

    // 5. An API mutation without Origin is refused with `auth.origin_mismatch`.
    let (status, _, body, _) = raw_request(&app, Method::POST, "/api/login", &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "auth.origin_mismatch");

    // 6. The label does not relax the write check: a matching authority passes even when the
    // browser labels the request cross-site.
    let (status, _, _, _) = raw_request(
        &app,
        Method::POST,
        "/api/login",
        &[
            (
                header::HeaderName::from_static("sec-fetch-site"),
                "cross-site",
            ),
            (header::ORIGIN, "http://nas.example:8199"),
            (header::CONTENT_TYPE, "application/json"),
        ],
    )
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn every_api_route_is_401_without_a_session_and_404_when_logged_in() {
    let (app, cookie) = setup().await;
    for (method, uri) in API_ROUTES {
        let method = Method::from_bytes(method.as_bytes()).unwrap();
        // Without a cookie (and with a valid Origin) the answer is 401.
        let (status, _, body, _) = raw_request(
            &app,
            method.clone(),
            uri,
            &[
                (header::ORIGIN, "http://nas.example:8199"),
                (header::CONTENT_TYPE, "application/json"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {body}");
        assert_eq!(body["error"], "auth.required", "{method} {uri}");
    }
    // The unknown API path is 401 while signed out, 404 once signed in.
    let (status, _, body, _) = raw_request(
        &app,
        Method::GET,
        "/api/nope",
        &[(header::ORIGIN, "http://nas.example:8199")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, _, body, _) = raw_request(
        &app,
        Method::GET,
        "/api/nope",
        &[
            (header::ORIGIN, "http://nas.example:8199"),
            (header::COOKIE, &format!("nvm_session={cookie}")),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn a_wrong_method_is_json_with_allow_and_csp() {
    let (app, cookie) = setup().await;
    let session = format!("nvm_session={cookie}");
    let (status, headers, body, text) = raw_request(
        &app,
        Method::DELETE,
        "/api/home",
        &[
            (header::ORIGIN, "http://nas.example:8199"),
            (header::COOKIE, &session),
        ],
    )
    .await;
    if status != StatusCode::METHOD_NOT_ALLOWED {
        panic!("status={status} text={text:?} headers={headers:?}");
    }
    assert_eq!(body["error"], "request.invalid");
    let allow = headers
        .get(header::ALLOW)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(allow.contains("GET"), "{allow}");
    assert!(headers.get(header::CONTENT_SECURITY_POLICY).is_some());
    // Static files only take GET/HEAD (with the origin guard satisfied).
    let (status, _, body, _) = raw_request(
        &app,
        Method::POST,
        "/",
        &[(header::ORIGIN, "http://nas.example:8199")],
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{body}");
    assert_eq!(body["error"], "request.invalid");
}

#[tokio::test]
async fn json_bodies_are_rejected_with_their_required_shapes() {
    let (app, _cookie) = setup().await;
    let cases: Vec<(&str, &str, StatusCode, &str)> = vec![
        (
            "no content type",
            "",
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "request.not_json",
        ),
        (
            "text/plain",
            "text/plain",
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "request.not_json",
        ),
        (
            "json-ish media type",
            "text/application/json-invalid",
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "request.not_json",
        ),
        (
            "broken json",
            "application/json",
            StatusCode::BAD_REQUEST,
            "request.invalid",
        ),
    ];
    for (label, content_type, want_status, want_key) in cases {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/api/login")
            .header(header::HOST, "nas.example:8199")
            .header(header::ORIGIN, "http://nas.example:8199");
        if !content_type.is_empty() {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        let body_bytes: Vec<u8> = if content_type == "application/json" {
            b"{not json".to_vec()
        } else {
            b"{}".to_vec()
        };
        let request = builder.body(Body::from(body_bytes)).unwrap();
        let response = app.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), want_status, "{label}");
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], want_key, "{label}");
    }
    for payload in [
        json!({"username": "a"}),
        json!({"username": "a", "password": "b", "extra": 1}),
        json!({"username": 4, "password": "b"}),
    ] {
        let (status, body) = post(&app, "/api/login", payload.clone(), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}: {body}");
        assert_eq!(body["error"], "request.invalid");
    }
    let big = vec![b' '; 3 * 1024 * 1024];
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/login")
        .header(header::HOST, "nas.example:8199")
        .header(header::ORIGIN, "http://nas.example:8199")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(big))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn path_and_query_parameters_are_checked_before_any_action() {
    let (app, cookie) = setup().await;
    let session = format!("nvm_session={cookie}");
    let origin = "http://nas.example:8199";
    for uri in [
        "/api/uploads/00000000000000000000000000000000/files?name=%ff.run",
        "/api/uploads/00000000000000000000000000000000/files?name=a%2Fb.run",
        "/api/uploads/00000000000000000000000000000000/files?name=",
        "/api/uploads/00000000000000000000000000000000/files",
        "/api/uploads/..%2Fetc/files?name=a.run",
    ] {
        let (status, _, body, _) = raw_request(
            &app,
            Method::PUT,
            uri,
            &[
                (header::ORIGIN, origin),
                (header::COOKIE, &session),
                (header::CONTENT_TYPE, "application/octet-stream"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
        assert_eq!(body["error"], "request.invalid", "{uri}");
    }
    for after in ["abc", "-1", "%2B1", "1.5"] {
        let uri = format!("/api/tasks/20260928T101530Z-3f9a0c12?after={after}");
        let (status, _, body, _) = raw_request(
            &app,
            Method::GET,
            &uri,
            &[(header::ORIGIN, origin), (header::COOKIE, &session)],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
        assert_eq!(body["error"], "request.invalid", "{uri}");
    }
    let (status, body) = post(
        &app,
        "/api/tasks",
        json!({"plan_id": "..%2Fetc", "button": "skip"}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "request.invalid");
    let (status, body) = post(
        &app,
        "/api/tasks",
        json!({"plan_id": "x".repeat(32), "button": "later"}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // Driver deletion/Keep update: malformed row ids never reach the filesystem.
    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": ["../../etc/passwd"]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "request.invalid");
    let (status, body) = put(
        &app,
        "/api/drivers/..%2Fetc/keep",
        json!({"keep": false}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "request.invalid");
}

#[tokio::test]
async fn the_task_detail_log_cursor_returns_tail_lines_and_a_reset() {
    let (app, cookie) = setup().await;
    let id = "20260928T101530Z-3f9a0c12";
    write_state(&app.host, id, Status::Failed, Some(Phase::Check));
    let log_path = format!("/var/lib/nvm/manager/state/{id}.log");
    let line = |key: &str| format!("{{\"t\":1,\"key\":\"{key}\",\"p\":{{}}}}\n");
    app.host.put_text(&log_path, &line("log.phase_enter"));
    let (status, body) = get(&app, &format!("/api/tasks/{id}"), Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "FAILED");
    assert_eq!(body["log"]["lines"].as_array().unwrap().len(), 1);
    let next = body["log"]["next"].as_u64().unwrap();
    assert!(next > 0);
    assert_eq!(body["log"]["reset"], false);
    // Nothing new: the cursor returns no lines.
    let (_, body) = get(
        &app,
        &format!("/api/tasks/{id}?after={next}"),
        Some(&cookie),
    )
    .await;
    assert_eq!(body["log"]["lines"].as_array().unwrap().len(), 0);
    // Append one line and read only the new one.
    let mut text = app.host.file_text(&log_path).unwrap();
    text.push_str(&line("log.finished"));
    app.host.put_text(&log_path, &text);
    let (_, body) = get(
        &app,
        &format!("/api/tasks/{id}?after={next}"),
        Some(&cookie),
    )
    .await;
    let lines = body["log"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1, "{body}");
    assert_eq!(lines[0]["key"], "log.finished");
    // A cursor past the end is a reset.
    let (_, body) = get(
        &app,
        &format!("/api/tasks/{id}?after=999999"),
        Some(&cookie),
    )
    .await;
    assert_eq!(body["log"]["reset"], true, "{body}");
    assert_eq!(body["log"]["lines"].as_array().unwrap().len(), 2);
    let (status, body) = get(&app, "/api/tasks/20260928T101530Z-ffffffff", Some(&cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn the_task_list_list_is_sorted_and_the_terminal_state_comes_from_the_file() {
    let (app, cookie) = setup().await;
    write_state(
        &app.host,
        "20260928T101530Z-3f9a0c11",
        Status::Failed,
        Some(Phase::Check),
    );
    {
        // A SUCCEEDED state carries a passing update check with both observations.
        use truenas_nvidia_driver_manager::task_state::{CheckRecord, Pass, ReadingJson};
        let mut state = write_state(
            &app.host,
            "20260928T101530Z-3f9a0c12",
            Status::Succeeded,
            Some(Phase::ReadLoadedVersionEnd),
        );
        state.checks.push(CheckRecord {
            pass: Pass::Update,
            since_before: ReadingJson::value(1),
            since_after: ReadingJson::value(2),
            refresh_ok: true,
            nvidia_smi: ReadingJson::value("580.178.04".to_string()),
            proc_version: ReadingJson::value("580.178.04".to_string()),
            expected: "580.178.04".to_string(),
            version_ok: true,
            passed: true,
        });
        state.loaded_version_end = Some(ReadingJson::value("580.178.04".to_string()));
        truenas_nvidia_driver_manager::task_state::write(
            &app.host,
            &truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR),
            &state,
        )
        .unwrap();
    }
    let (status, body) = get(&app, "/api/tasks", Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    let ids: Vec<String> = body["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["task_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        vec![
            "20260928T101530Z-3f9a0c12".to_string(),
            "20260928T101530Z-3f9a0c11".to_string()
        ]
    );
    assert_eq!(body["tasks"][0]["status"], "SUCCEEDED");
    assert!(body["active_task"].is_null());
    assert!(body["unit_query"].is_null());
}

#[tokio::test]
async fn every_response_carries_csp_and_no_referrer_policy() {
    let (app, cookie) = setup().await;
    let probes: Vec<(Method, &str, &str)> = vec![
        (Method::GET, "/", ""),
        (Method::GET, "/api/home", &cookie),
        (Method::GET, "/api/versions", &cookie),
        (Method::GET, "/api/nope", &cookie),
        (Method::POST, "/api/login", ""),
    ];
    for (method, uri, cookie) in probes {
        let session = format!("nvm_session={cookie}");
        let mut extra: Vec<(header::HeaderName, &str)> =
            vec![(header::ORIGIN, "http://nas.example:8199")];
        if !cookie.is_empty() {
            extra.push((header::COOKIE, session.as_str()));
        }
        let (_, headers, _, _) = raw_request(&app, method, uri, &extra).await;
        let csp = headers
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(
            csp,
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            "{uri}"
        );
        assert!(
            headers.get("referrer-policy").is_none(),
            "{uri} must not set Referrer-Policy"
        );
    }
}

/// Create an account with the password used by the body-limit sign-in control.
async fn account_for_body_limits() -> TestApp {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let app = app(host.clone(), settings(), healthy_startup(&host));
    let (status, _, _) = request(&app, Method::GET, "/api/login", None, None).await;
    assert_eq!(status, StatusCode::OK);
    setup_app(&app, "hunter12").await;
    app
}

fn json_body(bytes: &[u8]) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/api/login")
        .header(header::HOST, "nas.example:8199")
        .header(header::ORIGIN, "http://nas.example:8199")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .body(Body::from(bytes.to_vec()))
        .unwrap()
}

async fn read_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

#[tokio::test]
async fn an_anonymous_json_body_over_64_kib_is_refused_and_a_smaller_one_is_not() {
    let app = account_for_body_limits().await;
    let over = vec![b'x'; 64 * 1024 + 1];
    assert!(over.len() > 40 * 1024);
    let (status, body) =
        read_json(app.router.clone().oneshot(json_body(&over)).await.unwrap()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["error"], "request.too_large");

    let mut mid = b"{\"username\":\"".to_vec();
    mid.extend(std::iter::repeat_n(b'a', 50 * 1024));
    assert!(mid.len() > 40 * 1024 && mid.len() < 64 * 1024);
    let (status, body) =
        read_json(app.router.clone().oneshot(json_body(&mid)).await.unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "request.invalid");

    let (status, _) = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "hunter12"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// A body that never produces a frame.
struct PendingBody {
    polled: Arc<AtomicBool>,
}

impl HttpBody for PendingBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        self.polled.store(true, Ordering::SeqCst);
        Poll::Pending
    }
}

#[tokio::test(start_paused = true)]
async fn a_stalled_json_body_is_refused_after_the_deadline() {
    let app = account_for_body_limits().await;
    let polled = Arc::new(AtomicBool::new(false));
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/login")
        .header(header::HOST, "nas.example:8199")
        .header(header::ORIGIN, "http://nas.example:8199")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, "32")
        .body(Body::new(PendingBody {
            polled: polled.clone(),
        }))
        .unwrap();
    let router = app.router.clone();
    let task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
    for _ in 0..1_000 {
        if polled.load(Ordering::SeqCst) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(polled.load(Ordering::SeqCst), "the body read did not start");
    assert!(!task.is_finished());
    // The paused clock moves here. The runtime applies that move when this task waits, which is
    // what makes the body deadline fire.
    tokio::time::advance(JSON_BODY_DEADLINE + Duration::from_millis(1)).await;
    let response = task.await.unwrap();
    let (status, body) = read_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "request.invalid");
}

// ---- the result notice of a finished task ---------------------------------

const NOTICE_ID: &str = "20261005T120000Z-abcdef12";
const NOTICE_VERSION: &str = "580.178.04";

fn notice_paths() -> Paths {
    Paths::new(CONTAINER_DATA_DIR)
}

/// A successful update record. `reload_failed` names the module that did not come back, or `None`
/// for a clean success.
fn succeeded(host: &FakeHost, reload_failed: Option<&str>) -> TaskState {
    let mut state = task_state::new_state(
        NOTICE_ID,
        1,
        sample_plan(PlanSource::Official),
        Phase::Check,
    );
    state.status = Status::Succeeded;
    state.phase = Some(Phase::Check);
    state.finished_at = Some(2);
    state.loaded_version_start = Some(ReadingJson::value(NOTICE_VERSION.to_string()));
    state.loaded_version_end = Some(ReadingJson::value(NOTICE_VERSION.to_string()));
    state.reload_targets = Some(ReloadTargetsJson {
        basis: "unloaded_in_update".to_string(),
        modules: vec!["nvidia".to_string(), "nvidia_uvm".to_string()],
    });
    state.replace.push(ReplaceRecord {
        pass: Pass::Update,
        module_version_before: None,
        file_changed: None,
        module_correction: None,
        unloaded: vec!["nvidia_uvm".to_string()],
        unload_waits: Vec::new(),
        occupancy_waits: Vec::new(),
        reloaded: vec![
            ReloadedRecord {
                module: "nvidia".to_string(),
                ok: true,
                reason: None,
            },
            match reload_failed {
                Some(module) => ReloadedRecord {
                    module: module.to_string(),
                    ok: false,
                    reason: Some(ReasonJson::key("reason.exit_status", json!({"code": 1}))),
                },
                None => ReloadedRecord {
                    module: "nvidia_uvm".to_string(),
                    ok: true,
                    reason: None,
                },
            },
        ],
        refresh_exit: Some(json!({"code": 0})),
        ldconfig_exit: Some(json!({"code": 0})),
        readback_ok: Some(true),
        readonly_after: Some(ReadingJson::value("on".to_string())),
    });
    state.checks.push(CheckRecord {
        pass: Pass::Update,
        since_before: ReadingJson::value(1),
        since_after: ReadingJson::value(2),
        refresh_ok: true,
        nvidia_smi: ReadingJson::value(NOTICE_VERSION.to_string()),
        proc_version: ReadingJson::value(NOTICE_VERSION.to_string()),
        expected: NOTICE_VERSION.to_string(),
        version_ok: true,
        passed: true,
    });
    task_state::write(host, &notice_paths(), &state).unwrap();
    host.mkdir(&format!("{CONTAINER_DATA_DIR}/manager/tasks/{NOTICE_ID}"));
    state
}

/// A success whose effective pass did not reload every module is a warning notice naming them, not
/// a plain completion.
#[tokio::test]
async fn a_successful_update_names_the_modules_that_did_not_come_back() {
    let (app, cookie) = setup().await;
    succeeded(&app.host, Some("nvidia_uvm"));

    let (status, body) = get(&app, &format!("/api/tasks/{NOTICE_ID}"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "SUCCEEDED", "{body}");
    assert_eq!(body["notice"]["key"], "notice.update_complete", "{body}");
    assert_eq!(body["notice"]["variant"], "reload_warning", "{body}");
    assert_eq!(
        body["notice"]["params"]["version"], NOTICE_VERSION,
        "{body}"
    );
    assert_eq!(body["notice"]["params"]["modules"], "nvidia_uvm", "{body}");
}

#[tokio::test]
async fn a_clean_success_keeps_the_plain_completion_notice() {
    let (app, cookie) = setup().await;
    succeeded(&app.host, None);

    let (status, body) = get(&app, &format!("/api/tasks/{NOTICE_ID}"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["notice"]["key"], "notice.update_complete", "{body}");
    assert!(body["notice"]["variant"].is_null(), "{body}");
}

async fn release_request(app: &TestApp, cookie: &str) -> (StatusCode, serde_json::Value) {
    let (status, body, _) = request(
        app,
        Method::POST,
        &format!("/api/tasks/{RELEASE_X}/release"),
        None,
        Some(cookie),
    )
    .await;
    (status, body)
}

#[tokio::test]
async fn release_api_returns_detail_and_keeps_state_log_and_library_row() {
    let (app, cookie) = setup().await;
    release_scene(&app.host);
    let (status, before) = get(&app, &format!("/api/tasks/{RELEASE_X}"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(before["recovery_material_kept"], true, "{before}");
    let (status, body) = release_request(&app, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["task_id"], RELEASE_X, "{body}");
    assert_eq!(body["released_by"], RELEASE_L, "{body}");
    assert_eq!(body["recovery_material_kept"], false, "{body}");
    let paths = Paths::new(CONTAINER_DATA_DIR);
    let state = release_state(&app.host, RELEASE_X);
    assert_eq!(
        body["released_at"],
        serde_json::to_value(&state).unwrap()["released_at"],
        "{body}"
    );
    assert!(app.host.exists(paths.state(RELEASE_X).to_str().unwrap()));
    assert!(app
        .host
        .exists(paths.state_log(RELEASE_X).to_str().unwrap()));
    assert!(app.host.exists(
        paths
            .row_dir(&format!("{RELEASE_L}-build"))
            .to_str()
            .unwrap()
    ));
    assert!(!app.host.exists(paths.task_dir(RELEASE_X).to_str().unwrap()));
    let (status, again) = release_request(&app, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["released_by"], RELEASE_L, "{again}");
}

#[tokio::test]
async fn release_api_refuses_missing_proof_and_explains_why() {
    for (case, expected) in [
        ("newest", "release.no_later_success"),
        ("failed", "release.no_later_success"),
        ("interrupted", "release.no_later_success"),
        ("no_replace", "release.no_later_success"),
        ("missing_row", "release.driver_mismatch"),
        ("row_hash", "release.driver_mismatch"),
        ("canonical_hash", "release.driver_mismatch"),
        ("kernel", "release.driver_mismatch"),
        ("smi", "release.driver_mismatch"),
        ("mount", "release.host_residue"),
        ("readonly", "release.host_residue"),
        ("recovery", "release.recovery_pending"),
        ("unreadable", "release.recovery_pending"),
        ("invalid", "release.record_unreadable"),
        ("rebuilt", "release.record_unreadable"),
        ("target_running", "release.task_running"),
        ("running", "release.task_running"),
        ("unknown", "release.state_unknown"),
        ("locked", "release.lock_busy"),
        ("write", "release.write_failed"),
        ("cleanup", "release.cleanup_failed"),
    ] {
        let host = default_host();
        prime_canonical(&host, "580.173.02");
        let fail = Arc::new(AtomicBool::new(case == "cleanup"));
        let app = app_with_host(
            Arc::new(Wrap(ReleaseCleanupFailure {
                host: host.clone(),
                fail,
            })),
            host,
        );
        let cookie = setup_app(&app, "longpass99").await;
        release_scene(&app.host);
        let paths = Paths::new(CONTAINER_DATA_DIR);
        let mut l = release_state(&app.host, RELEASE_L);
        let _lock = if case == "locked" {
            Some(
                truenas_nvidia_driver_manager::admission::try_execution_lock(&app.host)
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        match case {
            "newest" => { l.created_at = 0; task_state::write(&app.host, &paths, &l).unwrap(); },
            "failed" | "interrupted" | "no_replace" => {
                if case == "failed" { l.status = Status::Failed; }
                else if case == "interrupted" { l.status = Status::Interrupted; }
                else { l.replace.clear(); }
                task_state::write(&app.host, &paths, &l).unwrap();
            },
            "missing_row" => app.host.remove(paths.row_dir(&format!("{RELEASE_L}-build")).to_str().unwrap()),
            "row_hash" => app.host.put_text(paths.row_dir(&format!("{RELEASE_L}-build")).join("nvidia.raw").to_str().unwrap(), "changed"),
            "canonical_hash" => app.host.put_text(CANONICAL_PATH, "changed"),
            "kernel" => app.host.put_text("/proc/driver/nvidia/version", "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  580.173.02  Release Build\n"),
            "smi" => app.host.preset("nvidia-smi", &[], FakeOutput::stdout("580.173.02\n")),
            "mount" => { app.host.add_mount(RELEASE_DATASET, RW_MOUNT, "zfs", "rw"); app.host.preset("umount", &[RW_MOUNT], FakeOutput::code(1)); },
            "readonly" => app.host.preset("zfs", &["get", "-H", "-o", "value", "readonly", RELEASE_DATASET], FakeOutput::stdout("off\n")),
            "recovery" => app.host.put_text(paths.state_dir().join(format!(".{RELEASE_X}.json.recovery-0123456789abcdef")).to_str().unwrap(), "protected"),
            "unreadable" => app.host.put_text(paths.state_unreadable(RELEASE_X).to_str().unwrap(), "protected"),
            "running" => app.host.set_unit("20261007T101530Z-abcdef78", "active", "running", "success", 0),
            "unknown" => app.host.preset("systemctl", &["list-units"], FakeOutput::code(1)),
            "write" => app.host.fail_rename_to_suffix(&format!("{RELEASE_X}.json")),
            "rebuilt" => {
                let mut x = release_state(&app.host, RELEASE_X);
                x.state_note = Some(task_state::StateNote { state_read_error: "missing".into(), kept_as: None });
                task_state::write(&app.host, &paths, &x).unwrap();
            },
            "target_running" => {
                let mut x = release_state(&app.host, RELEASE_X);
                x.status = Status::Running;
                task_state::write(&app.host, &paths, &x).unwrap();
            },
            "invalid" => app.host.put_text(paths.state(RELEASE_X).to_str().unwrap(), "invalid"),
            "cleanup" => {},
            "locked" => {},
            _ => unreachable!(),
        }
        let (status, body) = release_request(&app, &cookie).await;
        assert!(!status.is_success(), "case {case}: {status}: {body}");
        assert_eq!(body["error"], expected, "case {case}: {body}");
        assert!(
            app.host.exists(paths.task_dir(RELEASE_X).to_str().unwrap()),
            "case {case}"
        );
        if case != "cleanup" && case != "invalid" {
            let state = serde_json::to_value(release_state(&app.host, RELEASE_X)).unwrap();
            assert!(state["released_by"].is_null(), "case {case}: {state}");
        }
    }
}

#[tokio::test]
async fn release_api_enforces_origin_session_and_request_token_before_mutation() {
    let (app, cookie) = setup().await;
    release_scene(&app.host);
    let uri = format!("/api/tasks/{RELEASE_X}/release");
    let session = format!("nvm_session={cookie}");
    for (origin, token, expected) in [
        (None, None, "auth.origin_mismatch"),
        (
            Some("http://elsewhere.example:8199"),
            None,
            "auth.origin_mismatch",
        ),
        (Some("http://nas.example:8199"), Some(""), "auth.required"),
        (
            Some("http://nas.example:8199"),
            Some("incorrect"),
            "auth.required",
        ),
    ] {
        let mut headers = vec![(header::COOKIE, session.as_str())];
        if let Some(origin) = origin {
            headers.push((header::ORIGIN, origin));
        }
        if let Some(token) = token {
            headers.push((header::HeaderName::from_static("x-nvm-token"), token));
        }
        let (status, _, body, _) = raw_request(&app, Method::POST, &uri, &headers).await;
        assert_eq!(
            status,
            if expected == "auth.required" {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::FORBIDDEN
            },
            "{body}"
        );
        assert_eq!(body["error"], expected, "{body}");
        assert!(app.host.exists(
            Paths::new(CONTAINER_DATA_DIR)
                .task_dir(RELEASE_X)
                .to_str()
                .unwrap()
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn release_api_serializes_a_concurrent_task_submission() {
    let (app, cookie) = setup().await;
    release_scene(&app.host);
    let paths = Paths::new(CONTAINER_DATA_DIR);
    let x = release_state(&app.host, RELEASE_X);
    app.host.remove(paths.state(RELEASE_X).to_str().unwrap());
    app.host.remove(paths.task_dir(RELEASE_X).to_str().unwrap());
    prime_kernel_headers(&app.host, KERNEL);
    app.host.set_curl(
        CATALOG_URL,
        200,
        format!("<a href=\"{RELEASE_VERSION}/\">{RELEASE_VERSION}</a>\n").as_bytes(),
    );
    let (status, plan) = post_plan(
        &app,
        &cookie,
        json!({"kind":"official", "version":RELEASE_VERSION, "gpus":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["result"], "ready", "{plan}");
    task_state::write(&app.host, &paths, &x).unwrap();
    app.host.mkdir(paths.task_dir(RELEASE_X).to_str().unwrap());
    official_downloads(&app.host, RELEASE_VERSION);

    let gate = app.host.set_gate_output(
        "nvidia-smi",
        &[],
        FakeOutput::stdout(&format!("{RELEASE_VERSION}\n")),
    );
    let _release_gate = ReleaseGate(gate.clone());
    let release_app = app.clone();
    let release_cookie = cookie.clone();
    let releasing =
        tokio::spawn(async move { release_request(&release_app, &release_cookie).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !gate.blocked() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("release reached its live proof");
    assert!(app
        .host
        .lock_held(&format!("{LOCK_DIR}/{EXECUTION_LOCK_NAME}")));
    let submit_app = app.clone();
    let submitting =
        tokio::spawn(
            async move { post_task(&submit_app, &cookie, &plan["plan_id"], "skip").await },
        );
    tokio::task::yield_now().await;
    assert!(
        !submitting.is_finished(),
        "a submit waits for the release's process lock"
    );
    assert!(app.host.unit_ids().is_empty());
    gate.release();
    let (status, body) = releasing.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = submitting.await.unwrap();
    assert!(status.is_success(), "{status}: {body}");
    assert!(!app.host.unit_ids().is_empty());
    app.host.wait_for_units(Duration::from_secs(5));
    assert!(!app.host.exists(
        Paths::new(CONTAINER_DATA_DIR)
            .task_dir(RELEASE_X)
            .to_str()
            .unwrap()
    ));
}

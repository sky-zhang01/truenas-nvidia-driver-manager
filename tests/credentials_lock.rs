//! The credentials lock covers credential state only. login status reads the extension link for its
//! own response; that read is a host fact, not credential state, so it must not run under the lock:
//! a slow host read there would delay a concurrent login or password change, which need the same
//! lock. The test uses the fake host's path gate as a deterministic barrier: the next `lstat` of
//! the link blocks, login status stops inside that read, and password change must still answer
//! while login status is blocked.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::LINK_PATH;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocked_link_read_does_not_delay_a_password_change() {
    let host = default_host();
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    // A real sign-in first: password change needs a session cookie, and the initial credentials
    // must be in place before the barrier goes up.
    let cookie = login(&app, "admin", "hunter12").await;

    // The host stalls the next `lstat` of the extension link, the way a busy runner stalls an I/O
    // inside login status. The fake host records the view-relative path, which is the host path
    // itself.
    let gate = host.set_path_gate(LINK_PATH);

    let _release_gate = ReleaseGate(gate.clone());
    let login_status_request = tokio::spawn({
        let app = app.clone();
        async move { request(&app, Method::GET, "/api/login", None, None).await.0 }
    });
    let wait_until = Instant::now() + Duration::from_secs(30);
    while !gate.blocked() {
        assert!(
            Instant::now() < wait_until,
            "login status never reached the extension-link read"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Password change must not wait for that read: the lock covers the credentials, not the
    // extension link. The gate is released before the assertion so a failure cannot leave the
    // blocked reader (and the runtime shutdown) hanging.
    let changed = tokio::time::timeout(
        Duration::from_secs(2),
        request(
            &app,
            Method::POST,
            "/api/password",
            Some(json!({"new_password": "changed-password-1"})),
            Some(&cookie),
        ),
    )
    .await;
    gate.release();
    let delayed = changed.is_err();
    assert!(
        !delayed,
        "the password change waited for the blocked extension-link read"
    );
    let (status, body, _) = changed.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");

    let status = login_status_request.await.expect("login status task");
    assert_eq!(status, StatusCode::OK);
}

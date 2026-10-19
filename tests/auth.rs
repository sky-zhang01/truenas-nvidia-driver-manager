//! Authentication, sessions, request origin and password changes.

mod common;

use axum::http::{header, Method, StatusCode};
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::*;

async fn setup(password: &str) -> TestApp {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let app = app(host.clone(), settings(), healthy_startup(&host));
    setup_app(&app, password).await;
    let (status, _body, _) = request(&app, Method::GET, "/api/login", None, None).await;
    assert_eq!(status, StatusCode::OK);
    app
}

#[tokio::test]
async fn unauthenticated_api_requests_are_401() {
    let app = setup("hunter12").await;
    for uri in [
        "/api/home",
        "/api/versions",
        "/api/tasks",
        "/api/drivers",
        "/api/uploads/current",
        "/api/unknown",
    ] {
        let (status, body) = get(&app, uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(body["error"], "auth.required", "{uri}");
    }
}

#[tokio::test]
async fn login_page_is_public_and_reports_state() {
    let app = setup("hunter12").await;
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["logged_in"], false);
    assert_eq!(body["credentials"], "ok");
    assert_eq!(body["show_reset_help"], false);
    assert!(body["setup"]["problems"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn third_failure_shows_reset_help() {
    let app = setup("hunter12").await;
    for i in 1..=3 {
        let (status, body) = post(
            &app,
            "/api/login",
            json!({"username": "admin", "password": "wrong"}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "login.failed");
        assert_eq!(body["params"]["show_reset_help"], i >= 3, "attempt {i}");
    }
}

#[tokio::test]
async fn rate_limit_blocks_even_the_right_password_then_refills() {
    let app = setup("hunter12").await;
    for _ in 0..5 {
        let _ = post(
            &app,
            "/api/login",
            json!({"username": "admin", "password": "wrong"}),
            None,
        )
        .await;
    }
    let (status, body) = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "hunter12"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "login.rate_limited");
    assert!(body["params"]["retry_after_s"].as_u64().unwrap() <= 30);
    app.host.advance(std::time::Duration::from_secs(31));
    let (status, _) = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "hunter12"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn password_change_invalidates_other_sessions() {
    let app = setup("hunter12").await;
    let first = login(&app, "admin", "hunter12").await;
    let second = login(&app, "admin", "hunter12").await;
    let (status, _) = post(
        &app,
        "/api/password",
        json!({"new_password": "newpass99"}),
        Some(&first),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(&app, "/api/home", Some(&second)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A revoked session cannot change the password.
    let (status, _, _) = request(
        &app,
        Method::POST,
        "/api/password",
        Some(json!({"new_password": "anotherpass"})),
        Some(&second),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn short_password_is_rejected_and_old_one_still_works() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    let (status, body) = post(
        &app,
        "/api/password",
        json!({"new_password": "short"}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "password.rule.too_short");
    let (status, _) = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "hunter12"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn deleting_credentials_requires_setup_and_revokes_sessions() {
    let app = setup("hunter12").await;
    let old_cookie = login(&app, "admin", "hunter12").await;
    app.host
        .remove("/var/lib/nvm/manager/secrets/web-credentials.json");
    let code = app
        .ctx
        .auth
        .ensure_setup_code(&*app.ctx.host, &app.ctx.paths, &app.ctx.settings, "reset")
        .unwrap()
        .code;
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credentials"], "setup_required");
    // The old cookie no longer verifies.
    let (status, _) = get(&app, "/api/home", Some(&old_cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let cookie = setup_with_code(&app, &code, "hunter12").await;
    let (status, body) = get(&app, "/api/login", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("must_change_password").is_none());
}

#[tokio::test]
async fn corrupted_credentials_keep_the_file() {
    let app = setup("hunter12").await;
    app.host.put_text(
        "/var/lib/nvm/manager/secrets/web-credentials.json",
        "not json",
    );
    let before = app
        .host
        .file("/var/lib/nvm/manager/secrets/web-credentials.json")
        .unwrap();
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credentials"]["unreadable"], "not_json");
    assert_eq!(body["show_reset_help"], true);
    let after = app
        .host
        .file("/var/lib/nvm/manager/secrets/web-credentials.json")
        .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn cookie_attributes_and_expiry() {
    let app = setup("hunter12").await;
    let (status, _, headers) = request(
        &app,
        Method::POST,
        "/api/login",
        Some(json!({"username": "admin", "password": "hunter12"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Max-Age=43200"));
    assert!(!cookie.contains("Secure"));
    let value = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("nvm_session=");
    let (status, _) = get(&app, "/api/home", Some(value)).await;
    assert_eq!(status, StatusCode::OK);
    app.host
        .advance(std::time::Duration::from_secs(12 * 3600 + 1));
    let (status, body) = get(&app, "/api/home", Some(value)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth.required");
}

#[tokio::test]
async fn an_anonymous_login_read_does_not_reconcile() {
    let app = setup("hunter12").await;
    let before = app.host.commands().len();
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["logged_in"], false);
    assert_eq!(body["credentials"], "ok");
    assert_eq!(body["show_reset_help"], false);
    assert!(body["setup"]["problems"].as_array().unwrap().is_empty());
    // Anonymous login-status reads must not query task units.
    let new_commands = &app.host.commands()[before..];
    assert!(
        new_commands
            .iter()
            .all(|c| !(c.program == "systemctl" && c.args.iter().any(|a| a.contains("nvm-task-")))),
        "anonymous login status reconciled: {new_commands:?}"
    );
}

#[tokio::test]
async fn a_valid_session_login_read_still_reconciles() {
    let app = setup("hunter12").await;
    // A signed-in session still reconciles task state.
    let cookie = login(&app, "admin", "hunter12").await;
    let id = "20260928T101530Z-3f9a0c12";
    write_state(
        &app.host,
        id,
        truenas_nvidia_driver_manager::task_state::Status::Running,
        Some(truenas_nvidia_driver_manager::task_state::Phase::Write),
    );
    let before = app.host.commands().len();
    let (status, body) = get(&app, "/api/login", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["logged_in"], true);
    assert!(body.get("must_change_password").is_none());
    assert!(body["update_blocked"].is_array());
    let new_commands = &app.host.commands()[before..];
    assert!(
        new_commands
            .iter()
            .any(|c| c.program == "systemctl" && c.args.iter().any(|a| a.contains("nvm-task-"))),
        "a valid session did not reconcile: {new_commands:?}"
    );
    // The reconcile result is the INTERRUPTED state a later read sees.
    let state = app
        .host
        .file_text("/var/lib/nvm/manager/state/20260928T101530Z-3f9a0c12.json")
        .unwrap_or_default();
    assert!(state.contains("INTERRUPTED"), "{state}");
}

#[tokio::test]
async fn password_publish_never_moves_the_target_away() {
    // The publish must switch the target with a single rename from the temporary name. A reader of
    // the target may never see it absent, and a process killed at any point leaves the old or the
    // new file in place.
    let app = setup("hunter12").await;
    let before = app
        .host
        .file("/var/lib/nvm/manager/secrets/web-credentials.json")
        .unwrap();
    let state = truenas_nvidia_driver_manager::auth::AuthState::new();
    state
        .change_password(
            &app.host,
            &truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR),
            "changed123",
        )
        .unwrap();
    let target = FakeHost::translate("/var/lib/nvm/manager/secrets/web-credentials.json");
    let ops = app.host.ops();
    let mut swap = None;
    for (i, op) in ops.iter().enumerate() {
        if op.kind != "rename" {
            continue;
        }
        let mut parts = op.path.split(" -> ");
        let from = parts.next().unwrap_or("");
        let to = parts.next().unwrap_or("");
        assert_ne!(from, target, "the target was renamed away: {}", op.path);
        if to == target {
            assert!(
                from.contains(".web-credentials.json.tmp-"),
                "the target was replaced from {from}"
            );
            swap = Some(i);
        }
    }
    let swap = swap.expect("the target is replaced with a rename");
    // The old bytes are copied to a hidden recovery name before the swap, so an interruption after
    // the copy still leaves the previous file readable.
    let recovery_copies = ops[..swap]
        .iter()
        .filter(|o| o.kind == "create_new" && o.path.contains(".web-credentials.json.tmp-"))
        .count();
    assert!(
        recovery_copies >= 2,
        "expected the new file and the recovery copy before the swap: {ops:?}"
    );
    let after = app
        .host
        .file("/var/lib/nvm/manager/secrets/web-credentials.json")
        .unwrap();
    assert_ne!(after, before);
}

#[tokio::test]
async fn a_directory_sync_failure_keeps_the_old_credentials_file() {
    let app = setup("hunter12").await;
    app.host.fail_fsync_dir("/var/lib/nvm/manager/secrets");
    let before = app
        .host
        .file("/var/lib/nvm/manager/secrets/web-credentials.json")
        .unwrap();
    let state = truenas_nvidia_driver_manager::auth::AuthState::new();
    let result = state.change_password(
        &app.host,
        &truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR),
        "changed123",
    );
    assert_eq!(
        result,
        Err(
            truenas_nvidia_driver_manager::auth::PasswordChangeError::Unsaved(
                truenas_nvidia_driver_manager::auth::UnsavedReason::Directory
            )
        )
    );
    let after = app
        .host
        .file("/var/lib/nvm/manager/secrets/web-credentials.json")
        .unwrap();
    assert_eq!(after, before, "the old credentials file was replaced");
    // The failed publish never renamed the target away either.
    let target = FakeHost::translate("/var/lib/nvm/manager/secrets/web-credentials.json");
    for op in app.host.ops().iter().filter(|o| o.kind == "rename") {
        let from = op.path.split(" -> ").next().unwrap_or("");
        assert_ne!(from, target, "the target was renamed away: {}", op.path);
    }
}

#[tokio::test]
async fn write_failure_keeps_the_old_credentials_file() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    let before = app
        .host
        .file("/var/lib/nvm/manager/secrets/web-credentials.json")
        .unwrap();
    app.host.fail_write("/var/lib/nvm/manager/secrets/");
    let (status, body) = post(
        &app,
        "/api/password",
        json!({"new_password": "newpass99"}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "password.unsaved");
    let after = app
        .host
        .file("/var/lib/nvm/manager/secrets/web-credentials.json")
        .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn logout_ends_only_this_session() {
    let app = setup("hunter12").await;
    let first = login(&app, "admin", "hunter12").await;
    app.host.advance(std::time::Duration::from_millis(1));
    let second = login(&app, "admin", "hunter12").await;
    assert_ne!(first, second);
    let (status, _) = post(&app, "/api/logout", json!({}), Some(&first)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(&app, "/api/home", Some(&first)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = get(&app, "/api/home", Some(&second)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn unauthenticated_requests_to_the_unknown_api_are_401_then_404() {
    let app = setup("hunter12").await;
    let (status, _) = get(&app, "/api/nope", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let cookie = login(&app, "admin", "hunter12").await;
    let (status, body) = get(&app, "/api/nope", Some(&cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn setup_session_can_mutate_immediately() {
    let host = default_host();
    let app = app(host.clone(), settings(), healthy_startup(&host));
    let cookie = setup_app(&app, "hunter12").await;
    let (status, body) = post(&app, "/api/uploads", json!({}), Some(&cookie)).await;
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
    assert_ne!(body["error"], "auth.password_change_required");
    assert_eq!(
        post(&app, "/api/logout", json!({}), Some(&cookie)).await.0,
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_change_waits_for_the_credentials_lock() {
    // The password change must not interleave with account creation or another password change.
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    let guard = app.ctx.auth.credentials_lock.lock().await;
    let pending_app = app.clone();
    let pending = tokio::spawn(async move {
        post(
            &pending_app,
            "/api/password",
            json!({"new_password": "anotherpass99"}),
            Some(&cookie),
        )
        .await
        .0
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !pending.is_finished(),
        "password change completed while the credentials lock was held"
    );
    drop(guard);
    assert_eq!(pending.await.unwrap(), StatusCode::OK);
}

#[tokio::test]
async fn an_existing_credentials_file_keeps_its_username() {
    // A second web process with a different configured username must not rebuild the account; the
    // stored username and changed password keep working and the new one is refused.
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let app1 = app(host.clone(), settings(), healthy_startup(&host));
    let _ = get(&app1, "/api/login", None).await;
    let _ = setup_app(&app1, "longpass99").await;

    let mut second = settings();
    second.username = Some("root".into());
    let app2 = app(host.clone(), second, healthy_startup(&host));
    let _ = get(&app2, "/api/login", None).await;
    let _ = login(&app2, "admin", "longpass99").await;
    let (status, body, _) = request(
        &app2,
        axum::http::Method::POST,
        "/api/login",
        Some(serde_json::json!({"username": "root", "password": "longpass99"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "login.failed");
}

#[tokio::test]
async fn a_failed_password_save_keeps_the_old_file_and_the_issued_cookie() {
    // Every failure of the write steps leaves the credentials file and the already-issued cookies
    // as they were.
    type Injection = fn(&FakeHost);
    let cases: [(&str, Injection); 3] = [
        ("temp write", |h| {
            h.fail_write("/var/lib/nvm/manager/secrets/.web-credentials.json.tmp-")
        }),
        ("temp fsync", |h| {
            h.fail_sync_file("/var/lib/nvm/manager/secrets/.web-credentials.json.tmp-")
        }),
        ("rename", |h| {
            h.fail_rename_to_suffix("web-credentials.json")
        }),
    ];
    for (label, inject) in cases {
        let app = setup("hunter12").await;
        let cookie = login(&app, "admin", "hunter12").await;
        let path = "/var/lib/nvm/manager/secrets/web-credentials.json";
        let before = app.host.file(path).unwrap();
        inject(&app.host);
        let (status, body, _) = request(
            &app,
            Method::POST,
            "/api/password",
            Some(json!({"new_password": "newpass99"})),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{label}: {body}");
        assert_eq!(body["error"], "password.unsaved", "{label}");
        assert_eq!(
            app.host.file(path).unwrap(),
            before,
            "{label}: file changed"
        );
        let (status, _) = get(&app, "/api/home", Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "{label}: the old cookie died");
        let _ = login(&app, "admin", "hunter12").await;
        let (status, body, _) = request(
            &app,
            Method::POST,
            "/api/login",
            Some(json!({"username": "admin", "password": "newpass99"})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{label}: {body}");
    }
}

#[tokio::test]
async fn a_recovery_copy_that_cannot_be_deleted_never_rolls_back_the_new_password() {
    // The successful publish path retires the protected copy. A refused deletion must not leave it
    // under the recovery name: a later startup cleanup would otherwise put the old credentials back
    // over the committed password.
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    app.host
        .fail_remove("/var/lib/nvm/manager/secrets/.web-credentials.json.recovery-");
    let (status, body, headers) = request(
        &app,
        Method::POST,
        "/api/password",
        Some(json!({"new_password": "newpass99"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim_start_matches("nvm_session=").to_string())
        .expect("the new session cookie");
    app.host.clear_failures();
    // The next cleanup (startup does the same) must not roll the file back.
    let paths = truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR);
    truenas_nvidia_driver_manager::store::clean_leftovers(&app.host, &paths).unwrap();
    let (status, _) = get(&app, "/api/home", Some(&new_cookie)).await;
    assert_eq!(status, StatusCode::OK, "the new session died");
    let _ = login(&app, "admin", "newpass99").await;
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/login",
        Some(json!({"username": "admin", "password": "hunter12"})),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the old password came back: {body}"
    );
}

#[tokio::test]
async fn a_copy_that_can_neither_be_deleted_nor_renamed_is_disarmed_and_never_rolled_back() {
    // The protected copy can be neither unlinked nor renamed out of the recovery namespace, but it
    // can still be overwritten. The committed value must win: the material is disarmed with the
    // committed bytes, so a later flush can never put the old password back.
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    let path = "/var/lib/nvm/manager/secrets/web-credentials.json";
    app.host
        .fail_remove("/var/lib/nvm/manager/secrets/.web-credentials.json.recovery-");
    app.host
        .fail_rename_contains("/.web-credentials.json.recovery-");
    let (status, body, headers) = request(
        &app,
        Method::POST,
        "/api/password",
        Some(json!({"new_password": "newpass99"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim_start_matches("nvm_session=").to_string())
        .expect("the new session cookie");
    app.host.clear_failures();
    // The next cleanup (startup does the same) must not roll the file back.
    let paths = truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR);
    truenas_nvidia_driver_manager::store::clean_leftovers(&app.host, &paths).unwrap();
    let (status, _) = get(&app, "/api/home", Some(&new_cookie)).await;
    assert_eq!(status, StatusCode::OK, "the new session died");
    let _ = login(&app, "admin", "newpass99").await;
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/login",
        Some(json!({"username": "admin", "password": "hunter12"})),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the old password came back: {body}"
    );
    assert_ne!(app.host.file(path).unwrap(), b"");
}

#[tokio::test]
async fn a_failed_save_never_exposes_the_new_password_or_kills_the_old_cookie() {
    // The directory sync fails after the publish rename, and every rollback rename is refused: the
    // target may already hold the new bytes, but the protected copy holds the old ones. While it
    // exists, those bytes are the value of record, so the reported failure leaves the old password
    // working and the issued cookie valid.
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    app.host.fail_fsync_dir("/var/lib/nvm/manager/secrets");
    // The publish rename (the first one to the target) is allowed; the rollback renames are not.
    app.host
        .fail_rename_to_suffix_after("web-credentials.json", 1);
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/password",
        Some(json!({"new_password": "newpass99"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "password.unsaved");
    let (status, _) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "the issued cookie must stay valid");
    let _ = login(&app, "admin", "hunter12").await;
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/login",
        Some(json!({"username": "admin", "password": "newpass99"})),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the failed save must not expose the new password: {body}"
    );
    app.host.clear_failures();
    // Once the filesystem allows writes again, the next cleanup restores the old file to the
    // target.
    let paths = truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR);
    truenas_nvidia_driver_manager::store::clean_leftovers(&app.host, &paths).unwrap();
    let (status, _) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the cookie must survive the restore"
    );
}

/// The fixed credentials file is read on every request, and a leftover recovery copy never hides
/// its own read/classification failure.
const CP: &str = "/var/lib/nvm/manager/secrets/web-credentials.json";

/// Establish an unconfirmed publish rollback: the directory sync after the rename fails and every
/// rollback rename is refused, so the previous bytes stay under the protected recovery name.
async fn pending_recovery(app: &TestApp, cookie: &str) {
    app.host.fail_fsync_dir("/var/lib/nvm/manager/secrets");
    app.host
        .fail_rename_to_suffix_after("web-credentials.json", 1);
    let (status, body, _) = request(
        app,
        Method::POST,
        "/api/password",
        Some(json!({"new_password": "newpass99"})),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "password.unsaved");
    app.host.clear_failures();
    let dir = std::path::Path::new(CP).parent().unwrap();
    assert!(
        app.host
            .children(dir.to_str().unwrap())
            .iter()
            .any(|n| n.contains(".recovery-")),
        "the failed save left no recovery copy"
    );
}

#[tokio::test]
async fn a_pending_recovery_never_hides_a_fixed_credentials_failure() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    pending_recovery(&app, &cookie).await;

    // EACCES on the fixed file: credentials failure file_unreadable and a 401 on the old cookie,
    // not `credentials: ok` from the recovery copy.
    app.host.fail_read(CP);
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["credentials"]["unreadable"], "file_unreadable",
        "{body}"
    );
    assert_eq!(
        get(&app, "/api/home", Some(&cookie)).await.0,
        StatusCode::UNAUTHORIZED,
        "a fixed-file failure must invalidate protected requests"
    );
    app.host.clear_failures();

    app.host.put_text(CP, "{");
    let (_, body) = get(&app, "/api/login", None).await;
    assert_eq!(body["credentials"]["unreadable"], "not_json", "{body}");
    assert_eq!(
        get(&app, "/api/home", Some(&cookie)).await.0,
        StatusCode::UNAUTHORIZED
    );

    app.host.put_text(CP, "{}");
    let (_, body) = get(&app, "/api/login", None).await;
    assert_eq!(body["credentials"]["unreadable"], "field_invalid", "{body}");
    assert_eq!(
        get(&app, "/api/home", Some(&cookie)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_deleted_fixed_file_is_rebuilt_and_revokes_the_old_cookie() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    pending_recovery(&app, &cookie).await;
    app.host.remove(CP);
    assert_eq!(
        get(&app, "/api/home", Some(&cookie)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let code = app
        .ctx
        .auth
        .ensure_setup_code(&*app.ctx.host, &app.ctx.paths, &app.ctx.settings, "reset")
        .unwrap()
        .code;
    let (_, body) = get(&app, "/api/login", None).await;
    assert_eq!(body["credentials"], "setup_required", "{body}");
    assert_eq!(
        get(&app, "/api/home", Some(&cookie)).await.0,
        StatusCode::UNAUTHORIZED,
        "the rebuilt account must not accept the pre-deletion cookie"
    );
    let fresh = setup_with_code(&app, &code, "hunter12").await;
    let (_, body) = get(&app, "/api/login", Some(&fresh)).await;
    assert!(body.get("must_change_password").is_none());
}

#[tokio::test]
async fn a_disarmed_recovery_never_hides_a_fixed_credentials_failure() {
    // Password change publishes successfully, but the protected copy can be neither unlinked nor
    // renamed out of the recovery namespace, so it is disarmed in place with the committed bytes. A
    // later fixed-file EACCES is still reported as `file_unreadable` and 401s the issued cookie.
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    app.host
        .fail_remove("/var/lib/nvm/manager/secrets/.web-credentials.json.recovery-");
    app.host
        .fail_rename_contains("/.web-credentials.json.recovery-");
    let (status, body, headers) = request(
        &app,
        Method::POST,
        "/api/password",
        Some(json!({"new_password": "newpass99"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fresh = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim_start_matches("nvm_session=").to_string())
        .expect("the new session cookie");
    app.host.clear_failures();
    app.host.fail_read(CP);
    let (_, body) = get(&app, "/api/login", None).await;
    assert_eq!(
        body["credentials"]["unreadable"], "file_unreadable",
        "{body}"
    );
    assert_eq!(
        get(&app, "/api/home", Some(&fresh)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_non_utf8_configured_username_never_creates_the_default_account() {
    // The configured name is recorded before the UTF-8 conversion, so an unusable value is
    // `username_invalid`, never the `admin` default.
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use truenas_nvidia_driver_manager::settings;
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let s = settings::load(
        vec![(
            OsString::from(settings::VAR_USERNAME),
            OsString::from_vec(vec![0xff]),
        )]
        .into_iter(),
    );
    assert_eq!(s.username, None);
    let app = app(host.clone(), s, healthy_startup(&host));
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credentials"], "username_invalid", "{body}");
    assert!(
        host.file(CP).is_none(),
        "an invalid explicitly configured name must not create admin"
    );
}

const LOGGED_OUT: &str = "/var/lib/nvm/manager/secrets/logged-out.json";

fn logged_out_mode(host: &FakeHost) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let real = host
        .sandbox_root()
        .join(FakeHost::translate(LOGGED_OUT).trim_start_matches('/'));
    std::fs::metadata(&real)
        .unwrap_or_else(|e| panic!("missing {LOGGED_OUT} at {}: {e}", real.display()))
        .permissions()
        .mode()
        & 0o777
}

/// A `/api/home` read whose token header is only the one passed in.
async fn home_with_token(
    app: &TestApp,
    cookie: &str,
    token: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = axum::http::Request::builder()
        .method(Method::GET)
        .uri("/api/home")
        .header(header::HOST, "nas.example:8199")
        .header(header::ORIGIN, "http://nas.example:8199")
        .header(header::COOKIE, format!("nvm_session={cookie}"));
    if let Some(token) = token {
        builder = builder.header("x-nvm-token", token);
    }
    let response = tower::ServiceExt::oneshot(
        app.router.clone(),
        builder.body(axum::body::Body::empty()).unwrap(),
    )
    .await
    .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

#[tokio::test]
async fn a_missing_or_wrong_token_is_401_and_the_issued_token_is_200() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    let token = token_for(&cookie).expect("login remembered the request token");
    assert_eq!(token.len(), 64);

    let (status, body) = home_with_token(&app, &cookie, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "auth.required");

    let (status, body) = home_with_token(&app, &cookie, Some(&"0".repeat(64))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "auth.required");

    let (status, body, _) = request(&app, Method::GET, "/api/home", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = home_with_token(&app, "1.abcd", Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, _) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    let response = tower::ServiceExt::oneshot(
        app.router.clone(),
        axum::http::Request::builder()
            .method(Method::GET)
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn two_logins_in_the_same_millisecond_are_different_sessions() {
    let app = setup("hunter12").await;
    let first = login(&app, "admin", "hunter12").await;
    let second = login(&app, "admin", "hunter12").await;
    assert_eq!(
        first.split('.').next(),
        second.split('.').next(),
        "the fake clock moved between the two logins"
    );
    assert_ne!(first, second);
    assert_ne!(token_for(&first), token_for(&second));

    let (status, _) = post(&app, "/api/logout", json!({}), Some(&first)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(&app, "/api/home", Some(&first)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = get(&app, "/api/home", Some(&second)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_rebuilt_auth_state_still_rejects_a_logged_out_session() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    let (status, _) = post(&app, "/api/logout", json!({}), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(logged_out_mode(&app.host), 0o600);
    let mac = cookie.split('.').nth(2).unwrap();
    let stored = String::from_utf8(app.host.file(LOGGED_OUT).unwrap()).unwrap();
    assert!(stored.contains(mac), "{stored}");

    let host = app.host.clone();
    let restarted = common::app(host.clone(), settings(), healthy_startup(&host));
    restarted
        .ctx
        .auth
        .load_logged_out(restarted.ctx.host.as_ref(), &restarted.ctx.paths);
    let (status, body) = get(&restarted, "/api/home", Some(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    app.host.put_text(
        LOGGED_OUT,
        &format!(r#"{{"entries":[{{"mac":"{mac}","expiry":0}}]}}"#),
    );
    restarted
        .ctx
        .auth
        .load_logged_out(restarted.ctx.host.as_ref(), &restarted.ctx.paths);
    let (status, body) = get(&restarted, "/api/home", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    app.host.put_text(LOGGED_OUT, "not json");
    restarted
        .ctx
        .auth
        .load_logged_out(restarted.ctx.host.as_ref(), &restarted.ctx.paths);
    let (status, body) = get(&restarted, "/api/home", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_revocation_write_failure_is_an_error_and_still_revokes() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    app.host.fail_rename_to_suffix("logged-out.json");
    let (status, body, headers) = request(
        &app,
        Method::POST,
        "/api/logout",
        Some(json!({})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "logout.unsaved");
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn a_voluntary_password_change_can_repeat_the_current_password() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    let (status, body) = post(
        &app,
        "/api/password",
        json!({"new_password":"hunter12"}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        get(&app, "/api/home", Some(&cookie)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let next = login(&app, "admin", "hunter12").await;
    assert_eq!(
        post(
            &app,
            "/api/password",
            json!({"new_password":"newpass99"}),
            Some(&next)
        )
        .await
        .0,
        StatusCode::OK
    );
}

const QUEUED_TASK_ID: &str = "20260928T101530Z-3f9a0c12";

fn queued_task_path() -> String {
    format!("/var/lib/nvm/manager/state/{QUEUED_TASK_ID}.json")
}

/// A RUNNING record with no live unit. A reconcile that still runs rewrites it.
fn snapshot_running_task(app: &TestApp) -> Vec<u8> {
    write_state(
        &app.host,
        QUEUED_TASK_ID,
        truenas_nvidia_driver_manager::task_state::Status::Running,
        Some(truenas_nvidia_driver_manager::task_state::Phase::Write),
    );
    let bytes = app.host.file(&queued_task_path()).expect("state file");
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap_or("")
            .contains("RUNNING"),
        "fixture is not RUNNING"
    );
    bytes
}

fn spawn_task_list(
    app: &TestApp,
    cookie: &str,
) -> tokio::task::JoinHandle<axum::response::Response> {
    let token = token_for(cookie).expect("issued token");
    let router = app.router.clone();
    let cookie = cookie.to_string();
    tokio::spawn(async move {
        let request = axum::http::Request::builder()
            .method(Method::GET)
            .uri("/api/tasks")
            .header(header::HOST, "nas.example:8199")
            .header(header::ORIGIN, "http://nas.example:8199")
            .header(header::COOKIE, format!("nvm_session={cookie}"))
            .header("x-nvm-token", token)
            .body(axum::body::Body::empty())
            .unwrap();
        tower::ServiceExt::oneshot(router, request).await.unwrap()
    })
}

async fn assert_task_list_rejected(
    pending: tokio::task::JoinHandle<axum::response::Response>,
    before: &[u8],
    app: &TestApp,
) {
    let response = pending.await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "auth.required");
    assert_eq!(app.host.file(&queued_task_path()).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_task_list_does_nothing_after_logout() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    let before = snapshot_running_task(&app);
    // The list is accepted, then waits inside reconcile. Logout revokes that session before the
    // lock is released.
    let guard = app.ctx.process_lock.lock().await;
    let pending = spawn_task_list(&app, &cookie);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!pending.is_finished(), "the task list did not wait");
    let (status, body) = post(&app, "/api/logout", json!({}), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    drop(guard);
    assert_task_list_rejected(pending, &before, &app).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_task_list_does_nothing_after_password_change() {
    let app = setup("hunter12").await;
    let cookie = login(&app, "admin", "hunter12").await;
    let before = snapshot_running_task(&app);
    let guard = app.ctx.process_lock.lock().await;
    let pending = spawn_task_list(&app, &cookie);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!pending.is_finished(), "the task list did not wait");
    let (status, body) = post(
        &app,
        "/api/password",
        json!({"new_password": "third-pass-99"}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    drop(guard);
    assert_task_list_rejected(pending, &before, &app).await;
}

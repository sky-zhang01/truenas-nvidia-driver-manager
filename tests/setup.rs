//! Bootstrap security contracts against the production HTTP router.
mod common;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::*;
use serde_json::json;
use tower::ServiceExt;
use truenas_nvidia_driver_manager::host::Host;
use truenas_nvidia_driver_manager::settings::{self, SettingProblem};

const CREDENTIALS: &str = "/var/lib/nvm/manager/secrets/web-credentials.json";
const PASSWORD: &str = "a sentence for setup";

fn fresh() -> TestApp {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    app(host.clone(), common::settings(), healthy_startup(&host))
}

fn code(app: &TestApp) -> String {
    app.setup_code.lock().unwrap().clone().unwrap()
}

async fn submit(app: &TestApp, value: &str, password: &str) -> (StatusCode, serde_json::Value) {
    post(
        app,
        "/api/setup",
        json!({"code": value, "new_password": password}),
        None,
    )
    .await
}

#[tokio::test]
async fn setup_creates_private_credentials_and_an_authenticated_session() {
    let app = fresh();
    let value = code(&app);
    let (status, body, headers) = request(
        &app,
        Method::POST,
        "/api/setup",
        Some(json!({"code": value, "new_password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["token"].as_str().is_some_and(|s| !s.is_empty()));
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    for flag in ["Path=/", "Max-Age=43200", "HttpOnly", "SameSite=Strict"] {
        assert!(cookie.contains(flag), "{cookie}");
    }
    let session = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("nvm_session=");
    assert_eq!(
        get(&app, "/api/home", Some(session)).await.0,
        StatusCode::OK
    );
    let text = app.host.file_text(CREDENTIALS).unwrap();
    assert!(!text.contains(&value));
    assert!(!text.contains(PASSWORD));
    assert!(!text.contains("must_change_password"));
    let path = app
        .host
        .sandbox_root()
        .join(FakeHost::translate(CREDENTIALS).trim_start_matches('/'));
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!body.to_string().contains(&value));
}

#[tokio::test]
async fn anonymous_login_status_never_discloses_host_credentials_path() {
    let app = fresh();
    let assert_private = |body: &serde_json::Value| {
        assert!(body.get("credentials_reset_path").is_none(), "{body}");
        let text = body.to_string();
        assert!(!text.contains(HOST_DATA_DIR), "{body}");
        assert!(
            !text.contains("manager/secrets/web-credentials.json"),
            "{body}"
        );
    };
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credentials"], "setup_required");
    assert_private(&body);

    assert_eq!(submit(&app, &code(&app), PASSWORD).await.0, StatusCode::OK);
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credentials"], "ok");
    assert_private(&body);

    app.host.put_text(CREDENTIALS, "{");
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credentials"]["unreadable"], "not_json");
    assert_private(&body);
}

#[tokio::test]
async fn consumed_setup_code_cannot_overwrite_credentials() {
    let app = fresh();
    let value = code(&app);
    assert_eq!(submit(&app, &value, PASSWORD).await.0, StatusCode::OK);
    let before = app.host.file(CREDENTIALS).unwrap();
    let (status, body) = submit(&app, &value, "different password").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "setup.not_available");
    assert_eq!(app.host.file(CREDENTIALS).unwrap(), before);
}

#[tokio::test]
async fn wrong_setup_codes_spend_shared_quota_without_rotating() {
    let app = fresh();
    let value = code(&app);
    for attempt in 1..=5 {
        let (status, body) = submit(&app, "wrong", PASSWORD).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "setup.code_invalid");
        assert_eq!(body["params"]["show_help"], attempt >= 3);
        assert_eq!(body["params"]["code_renewed"], false);
    }
    assert_eq!(
        submit(&app, &value, PASSWORD).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    app.host.advance(Duration::from_secs(30));
    assert_eq!(submit(&app, &value, PASSWORD).await.0, StatusCode::OK);
    assert_eq!(
        post(
            &app,
            "/api/login",
            json!({"username":"admin","password":"wrong"}),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    for (uri, body) in [
        ("/api/login", json!({"username":"admin","password":"wrong"})),
        (
            "/api/ssh/login",
            json!({"challenge_id":"not-a-challenge","signature":"not-a-signature"}),
        ),
    ] {
        assert_eq!(
            post(&app, uri, body, None).await.0,
            StatusCode::TOO_MANY_REQUESTS,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn weak_password_keeps_code_and_does_not_spend_quota() {
    let app = fresh();
    let value = code(&app);
    for _ in 0..7 {
        let (status, body) = submit(&app, &value, "short").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "password.rule");
    }
    assert_eq!(submit(&app, &value, PASSWORD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn setup_code_works_just_before_expiry() {
    let app = fresh();
    app.host.advance(Duration::from_secs(24 * 60 * 60 - 1));
    assert_eq!(submit(&app, &code(&app), PASSWORD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn expired_post_without_status_get_renews_and_rejects_the_submission() {
    let app = fresh();
    let value = code(&app);
    app.host.advance(Duration::from_secs(24 * 60 * 60));
    let (status, body) = submit(&app, &value, PASSWORD).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "setup.code_invalid");
    assert_eq!(body["params"]["code_renewed"], true);
    assert!(app
        .ctx
        .auth
        .ensure_setup_code(&app.host, &app.ctx.paths, &app.ctx.settings, "reset")
        .is_none());
    assert!(app.host.file(CREDENTIALS).is_none());
    assert_eq!(
        submit(&app, &value, PASSWORD).await.1["params"]["code_renewed"],
        false
    );
}

#[tokio::test]
async fn ensure_is_idempotent_until_expiry_and_status_never_returns_the_code() {
    let app = fresh();
    let value = code(&app);
    for _ in 0..10 {
        assert!(app
            .ctx
            .auth
            .ensure_setup_code(&app.host, &app.ctx.paths, &app.ctx.settings, "reset")
            .is_none());
        let (status, body) = get(&app, "/api/login", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["credentials"], "setup_required");
        assert_eq!(body["setup"]["username"], "admin");
        assert_eq!(body["setup"]["valid_hours"], 24);
        assert!(!body.to_string().contains(&value));
        assert!(app.host.file(CREDENTIALS).is_none());
    }
    app.host.advance(Duration::from_secs(24 * 60 * 60));
    let issued = app
        .ctx
        .auth
        .ensure_setup_code(&app.host, &app.ctx.paths, &app.ctx.settings, "reset")
        .unwrap();
    assert_eq!(issued.reason, "expired");
    assert_ne!(issued.code, value);
    assert_eq!(submit(&app, &issued.code, PASSWORD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn setup_rejects_missing_or_mismatching_origin() {
    let app = fresh();
    for origin in [None, Some("http://attacker.example:8199")] {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/api/setup")
            .header(header::HOST, "nas.example:8199")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        let response = app
            .router
            .clone()
            .oneshot(
                builder
                    .body(Body::from(
                        json!({"code":code(&app),"new_password":PASSWORD}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(app.host.file(CREDENTIALS).is_none());
    }
    assert_eq!(submit(&app, &code(&app), PASSWORD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn setup_body_is_strict_and_invalid_requests_do_not_consume_code() {
    let app = fresh();
    for body in [
        json!({"code":code(&app),"new_password":PASSWORD,"username":"someone"}),
        json!({"code":42,"new_password":PASSWORD}),
        json!({"code":code(&app)}),
        json!([]),
    ] {
        let (status, response) = post(&app, "/api/setup", body, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["error"], "request.invalid");
    }
    let (status, body) = request_raw(
        &app,
        Method::POST,
        "/api/setup",
        b"{".to_vec(),
        "application/json",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "request.invalid");
    assert_eq!(submit(&app, &code(&app), PASSWORD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn restart_invalidates_pending_code_and_new_auth_accepts_only_its_own() {
    let preceding = fresh();
    let value = code(&preceding);
    let restarted = app(
        preceding.host.clone(),
        common::settings(),
        healthy_startup(&preceding.host),
    );
    assert_ne!(code(&restarted), value);
    assert_eq!(
        submit(&restarted, &value, PASSWORD).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        submit(&restarted, &code(&restarted), PASSWORD).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn concurrent_setup_succeeds_exactly_once() {
    let app = fresh();
    let value = code(&app);
    let (first, second) = tokio::join!(
        submit(&app, &value, PASSWORD),
        submit(&app, &value, "another strong password")
    );
    let mut statuses = [first.0.as_u16(), second.0.as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
    assert!(matches!(
        app.ctx
            .auth
            .load_state(&app.host, &app.ctx.paths, &app.ctx.settings),
        truenas_nvidia_driver_manager::auth::CredentialState::Ok
    ));
}

#[tokio::test]
async fn missing_account_login_paths_require_setup_without_quota_or_challenges() {
    let app = fresh();
    for _ in 0..7 {
        for (uri, body) in [
            ("/api/login", json!({"username":"admin","password":"wrong"})),
            ("/api/ssh/challenge", json!({})),
            (
                "/api/ssh/login",
                json!({"challenge_id":"missing","signature":"invalid"}),
            ),
        ] {
            let (status, body) = post(&app, uri, body, None).await;
            assert_eq!(status, StatusCode::CONFLICT, "{uri}: {body}");
            assert_eq!(body["error"], "login.setup_required");
        }
    }
    assert!(app.ctx.auth.limiter.lock().await.is_none());
    assert_eq!(
        app.ctx.auth.ssh_challenges.outstanding(app.host.wall_ms()),
        0
    );
    assert_eq!(submit(&app, &code(&app), PASSWORD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn reset_revokes_sessions_and_ssh_keys_without_removing_tasks_or_drivers() {
    let app = fresh();
    let cookie = setup_app(&app, PASSWORD).await;
    let seed = [5u8; 32];
    let key = ssh_key::private::Ed25519Keypair::from_seed(&seed);
    let private =
        ssh_key::PrivateKey::new(ssh_key::private::KeypairData::Ed25519(key), "setup-test")
            .unwrap();
    let public = private.public_key().to_openssh().unwrap();
    let (status, body) = post(
        &app,
        "/api/ssh/keys",
        json!({"public_key":public,"comment":"setup test"}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let cookie = login(&app, "admin", PASSWORD).await;
    assert_eq!(app.ctx.auth.ssh_key_count(&app.host, &app.ctx.paths), 1);
    app.host
        .put_text("/var/lib/nvm/manager/state/preserved.log", "keep task");
    app.host
        .put_text("/var/lib/nvm/manager/drivers/preserved.raw", "keep driver");
    let preceding: serde_json::Value =
        serde_json::from_slice(&app.host.file(CREDENTIALS).unwrap()).unwrap();
    app.host.remove(CREDENTIALS);
    let issued = app
        .ctx
        .auth
        .ensure_setup_code(&app.host, &app.ctx.paths, &app.ctx.settings, "reset")
        .unwrap();
    assert_eq!(issued.reason, "reset");
    let (status, body) = get(&app, "/api/login", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["logged_in"], false);
    assert_eq!(body["credentials"], "setup_required");
    assert_eq!(
        get(&app, "/api/home", Some(&cookie)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(app.ctx.auth.ssh_key_count(&app.host, &app.ctx.paths), 0);
    assert_eq!(
        app.host
            .file_text("/var/lib/nvm/manager/state/preserved.log")
            .as_deref(),
        Some("keep task")
    );
    assert_eq!(
        app.host
            .file_text("/var/lib/nvm/manager/drivers/preserved.raw")
            .as_deref(),
        Some("keep driver")
    );
    assert_eq!(
        submit(&app, &issued.code, "replacement account password")
            .await
            .0,
        StatusCode::OK
    );
    let current: serde_json::Value =
        serde_json::from_slice(&app.host.file(CREDENTIALS).unwrap()).unwrap();
    assert_ne!(current["session_key"], preceding["session_key"]);
    assert_eq!(current["ssh_keys"], json!([]));
    assert_eq!(
        get(&app, "/api/home", Some(&cookie)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &app,
            "/api/login",
            json!({"username":"admin","password":PASSWORD}),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[test]
fn removed_password_variable_is_unknown_without_exposing_its_non_utf8_value() {
    let value = OsString::from_vec(b"secret-value-\xff".to_vec());
    let settings =
        settings::load([(OsString::from("NVM_INITIAL_ADMIN_PASSWORD"), value)].into_iter());
    assert_eq!(
        settings.problems,
        vec![SettingProblem::Unknown {
            variable: "NVM_INITIAL_ADMIN_PASSWORD".into()
        }]
    );
    assert!(!settings.usable());
    assert!(!format!("{settings:?}").contains("secret-value"));
}

#[tokio::test]
async fn successful_setup_consumes_digest_before_account_reset() {
    let app = fresh();
    let preceding = code(&app);
    assert_eq!(submit(&app, &preceding, PASSWORD).await.0, StatusCode::OK);
    app.host.remove(CREDENTIALS);
    let issued = app
        .ctx
        .auth
        .ensure_setup_code(&app.host, &app.ctx.paths, &app.ctx.settings, "reset")
        .expect("consumed code must allow a fresh reset code");
    assert_eq!(issued.reason, "reset");
    assert_ne!(issued.code, preceding);
    assert_eq!(
        submit(&app, &preceding, PASSWORD).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(submit(&app, &issued.code, PASSWORD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn setup_waits_for_the_credentials_lock() {
    let app = fresh();
    let guard = app.ctx.auth.credentials_lock.lock().await;
    let other = app.clone();
    let value = code(&app);
    let pending = tokio::spawn(async move { submit(&other, &value, PASSWORD).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !pending.is_finished(),
        "setup must wait for the credentials lock"
    );
    assert!(app.host.file(CREDENTIALS).is_none());
    drop(guard);
    assert_eq!(pending.await.unwrap().0, StatusCode::OK);
}

#[tokio::test]
async fn expiry_while_waiting_for_the_shared_limiter_renews_in_the_same_post() {
    let app = fresh();
    let value = code(&app);
    let guard = app.ctx.auth.limiter.lock().await;
    let pending_app = app.clone();
    let pending = tokio::spawn(async move { submit(&pending_app, &value, PASSWORD).await });
    // The credentials lock proves the handler has entered setup before time advances.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if app.ctx.auth.credentials_lock.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    app.host.advance(Duration::from_secs(24 * 60 * 60));
    drop(guard);
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["params"]["code_renewed"], true);
    assert!(app
        .ctx
        .auth
        .ensure_setup_code(&app.host, &app.ctx.paths, &app.ctx.settings, "reset")
        .is_none());
}

#[tokio::test]
async fn failed_setup_sync_and_unlink_leave_no_usable_account_and_allow_retry() {
    let app = fresh();
    let value = code(&app);
    app.host
        .fail_sync_file("/var/lib/nvm/manager/secrets/.web-credentials.json.tmp-");
    app.host.fail_remove(CREDENTIALS);
    let (status, body) = submit(&app, &value, PASSWORD).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "login.credentials_unsaved");
    assert!(app.host.file(CREDENTIALS).is_none());
    app.host.clear_failures();
    assert_eq!(submit(&app, &value, PASSWORD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn failed_setup_directory_sync_keeps_the_complete_published_account() {
    let app = fresh();
    let value = code(&app);
    app.host.fail_fsync_dir("/var/lib/nvm/manager/secrets");
    let (status, body) = submit(&app, &value, PASSWORD).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "login.credentials_unsaved");
    let saved = app.host.file(CREDENTIALS).unwrap();
    assert!(app
        .ctx
        .auth
        .load_state(&app.host, &app.ctx.paths, &app.ctx.settings)
        .is_ok());
    assert!(body.get("token").is_none());
    app.host.clear_failures();
    assert_eq!(submit(&app, &value, PASSWORD).await.0, StatusCode::CONFLICT);
    assert_eq!(app.host.file(CREDENTIALS).unwrap(), saved);
}

#[tokio::test]
async fn published_setup_failure_consumes_the_code_before_account_reset() {
    let app = fresh();
    let value = code(&app);
    app.host.fail_fsync_dir("/var/lib/nvm/manager/secrets");
    assert_eq!(
        submit(&app, &value, PASSWORD).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    app.host.clear_failures();
    app.host.remove(CREDENTIALS);
    let issued = app
        .ctx
        .auth
        .ensure_setup_code(&app.host, &app.ctx.paths, &app.ctx.settings, "reset")
        .expect("a published account must consume its setup code even if directory sync fails");
    assert_ne!(issued.code, value);
    assert_eq!(issued.reason, "reset");
    assert_eq!(
        submit(&app, &value, PASSWORD).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(submit(&app, &issued.code, PASSWORD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn published_account_survives_sync_failure_and_restart_without_rollback() {
    let app = fresh();
    app.host.fail_fsync_dir("/var/lib/nvm/manager/secrets");
    app.host
        .fail_rename_to_suffix_after("web-credentials.json", 1);
    app.host.fail_remove(CREDENTIALS);
    assert_eq!(
        submit(&app, &code(&app), PASSWORD).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let raw: serde_json::Value =
        serde_json::from_slice(&app.host.file(CREDENTIALS).unwrap()).unwrap();
    assert_eq!(raw["username"], "admin");
    assert!(app
        .ctx
        .auth
        .load_state(&app.host, &app.ctx.paths, &app.ctx.settings)
        .is_ok());
    let restarted = common::app(
        app.host.clone(),
        common::settings(),
        healthy_startup(&app.host),
    );
    assert!(restarted.setup_code.lock().unwrap().is_none());
    let (status, body) = post(
        &restarted,
        "/api/login",
        json!({"username":"admin", "password":PASSWORD}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["token"].as_str().is_some());
}

#[tokio::test]
async fn additional_credential_fields_do_not_trigger_automatic_rewrites() {
    for value in [true, false] {
        let app = fresh();
        let credentials = json!({
            "username":"admin", "password_hash":truenas_nvidia_driver_manager::auth::hash_password(PASSWORD).unwrap(),
            "session_key":"a".repeat(64), "ssh_keys":[], "must_change_password": value,
        }).to_string();
        app.host.put_text(CREDENTIALS, &credentials);
        let (_, body) = get(&app, "/api/login", None).await;
        assert_eq!(body["credentials"], "ok");
        assert!(body.get("must_change_password").is_none());
        assert_eq!(
            app.host.file_text(CREDENTIALS).as_deref(),
            Some(credentials.as_str())
        );
        let cookie = login(&app, "admin", PASSWORD).await;
        assert_eq!(
            post(
                &app,
                "/api/password",
                json!({"new_password":"a new sentence for setup"}),
                Some(&cookie)
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(!app
            .host
            .file_text(CREDENTIALS)
            .unwrap()
            .contains("must_change_password"));
    }
}

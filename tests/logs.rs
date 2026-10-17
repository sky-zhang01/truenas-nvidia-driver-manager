//! `docker logs` lines and secret hygiene.

mod common;

use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::Event;
use truenas_nvidia_driver_manager::log;

const APPROVED: [(Event, &str); 38] = [
    (Event::Starting, "starting"),
    (Event::SettingUnknown, "setting_unknown"),
    (Event::SettingInvalid, "setting_invalid"),
    (Event::PortFallback, "port_fallback"),
    (Event::DataDirsFailed, "data_dirs_failed"),
    (Event::StartupCheckFailed, "startup_check_failed"),
    (Event::StartupChecksPassed, "startup_checks_passed"),
    (Event::CredentialsCreated, "credentials_created"),
    (
        Event::CredentialsInitialPasswordUnusable,
        "credentials_initial_password_unusable",
    ),
    (Event::CredentialsUnreadable, "credentials_unreadable"),
    (Event::CredentialsUnsaved, "credentials_unsaved"),
    (Event::Listening, "listening"),
    (Event::ListenFailed, "listen_failed"),
    (Event::LoginSucceeded, "login_succeeded"),
    (Event::LoginFailed, "login_failed"),
    (Event::LoginRateLimited, "login_rate_limited"),
    (Event::Logout, "logout"),
    (Event::PasswordChanged, "password_changed"),
    (Event::CatalogQueryFailed, "catalog_query_failed"),
    (Event::KernelHeadersUnreadable, "kernel_headers_unreadable"),
    (Event::GpusUnreadable, "gpus_unreadable"),
    (Event::RunnerCopyFailed, "runner_copy_failed"),
    (Event::TaskSubmitted, "task_submitted"),
    (Event::TaskSubmitFailed, "task_submit_failed"),
    (Event::TaskInterrupted, "task_interrupted"),
    (Event::TaskReleased, "task_released"),
    (Event::StateFileKept, "state_file_kept"),
    (Event::TaskCleanup, "task_cleanup"),
    (Event::HostReset, "host_reset"),
    (Event::TaskCleanupStepFailed, "task_cleanup_step_failed"),
    (Event::UnitQueryFailed, "unit_query_failed"),
    (Event::UploadRejected, "upload_rejected"),
    (Event::DriversDeleted, "drivers_deleted"),
    (Event::SshLoginSucceeded, "ssh_login_succeeded"),
    (Event::SshLoginFailed, "ssh_login_failed"),
    (Event::AdmissionBlocked, "admission_blocked"),
    (Event::SshKeyAdded, "ssh_key_added"),
    (Event::SshKeyRemoved, "ssh_key_removed"),
];

#[test]
fn event_wire_names_match_the_required_table() {
    for &(event, name) in &APPROVED {
        match event {
            Event::Starting
            | Event::SettingUnknown
            | Event::SettingInvalid
            | Event::PortFallback
            | Event::DataDirsFailed
            | Event::StartupCheckFailed
            | Event::StartupChecksPassed
            | Event::CredentialsCreated
            | Event::CredentialsInitialPasswordUnusable
            | Event::CredentialsUnreadable
            | Event::CredentialsUnsaved
            | Event::Listening
            | Event::ListenFailed
            | Event::LoginSucceeded
            | Event::LoginFailed
            | Event::LoginRateLimited
            | Event::Logout
            | Event::PasswordChanged
            | Event::CatalogQueryFailed
            | Event::KernelHeadersUnreadable
            | Event::GpusUnreadable
            | Event::RunnerCopyFailed
            | Event::TaskSubmitted
            | Event::TaskSubmitFailed
            | Event::TaskInterrupted
            | Event::TaskReleased
            | Event::StateFileKept
            | Event::TaskCleanup
            | Event::HostReset
            | Event::TaskCleanupStepFailed
            | Event::UnitQueryFailed
            | Event::UploadRejected
            | Event::DriversDeleted
            | Event::SshLoginSucceeded
            | Event::SshLoginFailed
            | Event::AdmissionBlocked
            | Event::SshKeyAdded
            | Event::SshKeyRemoved => {}
        }
        assert_eq!(event.as_str(), name);
    }
}

#[tokio::test]
async fn log_lines_are_ascii_json_of_known_events_without_secrets() {
    let buf = SharedBuf::default();
    log::set_sink(Box::new(buf.clone()));

    let host = default_host();
    prime_canonical(&host, "580.173.02");
    host.set_curl(
        truenas_nvidia_driver_manager::codes::CATALOG_URL,
        200,
        b"<a href=\"580.178.04/\">580.178.04</a>\n",
    );
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let _ = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "definitelywrong"}),
        None,
    )
    .await;
    let cookie = login_changed(&app, "hunter12", "longpass99").await;
    // A successful version-index query must not write the failure event.
    let (versions_status, _) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(versions_status, axum::http::StatusCode::OK);
    let (_, group) = post(&app, "/api/uploads", json!({}), Some(&cookie)).await;
    let group = group["group_id"].as_str().unwrap().to_string();
    let _ = request_raw(
        &app,
        axum::http::Method::PUT,
        &format!("/api/uploads/{group}/files?name=x.txt"),
        b"hello".to_vec(),
        "application/octet-stream",
        Some(&cookie),
    )
    .await;
    let _ = post(&app, "/api/logout", json!({}), Some(&cookie)).await;

    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!text.is_empty());
    assert!(text.is_ascii(), "non-ASCII log output: {text}");
    for line in text.lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!("log line is not JSON ({e}): {line}");
        });
        let level = value["level"].as_str().unwrap();
        assert!(matches!(level, "INFO" | "ERROR"), "{line}");
        let event = value["event"].as_str().unwrap();
        assert!(
            APPROVED.iter().any(|(_, name)| *name == event),
            "unknown event {event}: {line}"
        );
        assert!(value["msg"].as_str().is_some(), "{line}");
        assert!(value["ts_ms"].is_number(), "{line}");
    }
    assert!(
        !text.contains("catalog_query_failed"),
        "a successful catalog query wrote a failure event: {text}"
    );
    let failed = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v["event"] == "login_failed")
        .expect("a login_failed line");
    assert_eq!(failed["consecutive_failures"], 1, "{failed}");
    assert!(!text.contains("hunter12"));
    assert!(!text.contains("longpass99"));
    assert!(!text.contains("definitelywrong"));
    let credentials = app
        .host
        .file_text("/var/lib/nvm/manager/secrets/web-credentials.json")
        .unwrap();
    assert!(!credentials.contains("hunter12"));
    assert!(!credentials.contains("longpass99"));
    assert!(credentials.contains("$argon2id$"));
    app.host.assert_red_lines();
}

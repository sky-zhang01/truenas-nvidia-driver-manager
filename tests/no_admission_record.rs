//! Existing task data and unrelated admission records do not prevent a driver update.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::auth::{AuthState, CredentialState};
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{self, Status, TaskState};

/// A finished task makes this an existing data directory.
const OLD_ID: &str = "20260928T101530Z-3f9a0c11";
const ROW_ID: &str = "20260928T101530Z-3f9a0c12-build";
const OLD_BYTES: &[u8] = b"stock driver image";
const NEW_BYTES: &[u8] = b"new driver image";
/// An unrelated control record must not affect task execution.
const CONTROL_DIR: &str = "/var/lib/nvm/manager/control";
const RECORD_PATH: &str = "/var/lib/nvm/manager/control/admission.json";
const PAUSED_ADMISSION_RECORD: &str = r#"{"state":"paused","owner_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","target":"deployment","updated_at_ms":1}"#;
/// Malformed unrelated records must remain untouched.
const CORRUPT_RECORD: &str = "{not json";

fn paths() -> Paths {
    Paths::new(CONTAINER_DATA_DIR)
}

/// The host scene a whole update needs (the same shape the runner-flow suite uses): one idle
/// `nvidia` module, the ZFS `/usr` mount and the canonical image the local row replaces.
fn scene() -> FakeHost {
    let host = FakeHost::new();
    // One idle module: the update unloads and reloads it (a busy stack cannot be unloaded, which is
    // a different scenario).
    host.load_module("nvidia", 0, &[]);
    host.set_driver_version("580.173.02");
    host.remove_mount("/usr");
    host.add_mount("tank/ROOT/default/usr", "/usr", "zfs", "ro");
    host.put_file(CANONICAL_PATH, OLD_BYTES.to_vec());
    host.set_listing(CANONICAL_PATH, &canonical_listing(KERNEL, "580.173.02"));
    host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH);
    host.put_text("/data/manifest.json", r#"{"version":"25.10.7"}"#);
    let row = paths().row_dir(ROW_ID);
    host.put_file(row.join("nvidia.raw").to_str().unwrap(), NEW_BYTES.to_vec());
    host.set_listing(
        row.join("nvidia.raw").to_str().unwrap(),
        &canonical_listing(KERNEL, "580.178.04"),
    );
    host.put_text(
        row.join("record.json").to_str().unwrap(),
        &json!({
            "kind": "official",
            "version": "580.178.04",
            "sha256": sha256_hex(NEW_BYTES),
            "size": NEW_BYTES.len(),
            "target_kernel": KERNEL,
            "created_at": 1_790_000_000_000u64,
            "fingerprint": null,
            "original_of": null,
            "keep": null,
        })
        .to_string(),
    );
    host
}

/// An existing data directory with credentials and a finished task, without an admission record.
fn existing_data_dir(host: &FakeHost) -> truenas_nvidia_driver_manager::settings::Settings {
    let settings = settings("hunter12");
    let state = AuthState::new().load_or_create(host, &paths(), &settings);
    assert!(
        matches!(state, CredentialState::Ok { .. }),
        "the fixture must have usable credentials: {state:?}"
    );
    write_state(host, OLD_ID, Status::Succeeded, None);
    settings
}

/// Submit one local update through the web API and let the real runner unit finish. Returns the
/// task id and its persisted record.
async fn complete_one_update(app: &TestApp, cookie: &str) -> (String, TaskState) {
    let (status, plan) = post_plan(app, cookie, json!({"kind": "local", "row_id": ROW_ID})).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["result"], "ready", "{plan}");
    let (status, task) = post_task(app, cookie, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id = task["task_id"].as_str().expect("a task id").to_string();
    app.host.wait_for_units(std::time::Duration::from_secs(10));
    let state = match task_state::read(&app.host, &paths(), &id) {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!("the task record is unreadable: {other:?}"),
    };
    (id, state)
}

fn assert_one_completed_update(app: &TestApp, id: &str, state: &TaskState) {
    assert_eq!(state.status, Status::Succeeded, "{state:?}");
    assert_eq!(state.checks.len(), 1, "{state:?}");
    assert!(state.checks[0].passed, "{state:?}");
    assert_eq!(
        app.host.file(CANONICAL_PATH).as_deref(),
        Some(NEW_BYTES),
        "the canonical image must carry the update"
    );
    assert!(
        app.host
            .file_text(&paths().state(id).to_string_lossy())
            .is_some(),
        "the task record stays readable"
    );
}

/// The update must not create an admission record.
fn assert_no_admission_artifacts(app: &TestApp) {
    assert!(
        !app.host.exists(RECORD_PATH),
        "no admission record may be created"
    );
    assert!(
        !app.host.exists(CONTROL_DIR),
        "no admission record directory may be created"
    );
}

#[tokio::test]
async fn an_existing_data_dir_without_a_record_updates_normally() {
    let host = scene();
    let settings = existing_data_dir(&host);
    let app = app(host.clone(), settings, healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "longpass99").await;

    let (status, login) = get(&app, "/api/login", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{login}");
    let blocked = login["update_blocked"]
        .as_array()
        .expect("update_blocked is a list");
    assert!(
        !blocked.iter().any(|item| item["key"]
            .as_str()
            .is_some_and(|key| key.starts_with("admission"))),
        "no admission item may be reported: {blocked:?}"
    );

    let (id, state) = complete_one_update(&app, &cookie).await;
    assert_one_completed_update(&app, &id, &state);
    assert_no_admission_artifacts(&app);
}

#[tokio::test]
async fn a_leftover_paused_record_is_ignored_and_kept() {
    let host = scene();
    let settings = existing_data_dir(&host);
    host.put_text(RECORD_PATH, PAUSED_ADMISSION_RECORD);
    let app = app(host.clone(), settings, healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "longpass99").await;

    let (id, state) = complete_one_update(&app, &cookie).await;
    assert_one_completed_update(&app, &id, &state);
    assert_eq!(
        app.host.file_text(RECORD_PATH).as_deref(),
        Some(PAUSED_ADMISSION_RECORD),
        "the leftover record is neither read, rewritten nor deleted"
    );
}

#[tokio::test]
async fn a_corrupt_leftover_record_is_never_read() {
    let host = scene();
    let settings = existing_data_dir(&host);
    // Malformed unrelated material must neither block the update nor be overwritten.
    host.put_text(RECORD_PATH, CORRUPT_RECORD);
    let app = app(host.clone(), settings, healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "longpass99").await;

    let (id, state) = complete_one_update(&app, &cookie).await;
    assert_one_completed_update(&app, &id, &state);
    assert_eq!(
        app.host.file_text(RECORD_PATH).as_deref(),
        Some(CORRUPT_RECORD),
        "the corrupt bytes are neither read, rewritten nor deleted"
    );
}

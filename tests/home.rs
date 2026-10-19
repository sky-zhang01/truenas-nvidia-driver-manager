//! The home readings.

mod common;

use common::*;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::startup::StartupProblem;
use truenas_nvidia_driver_manager::task_state::{Phase, Status};

async fn setup() -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    signed_in_app(host).await
}

#[tokio::test]
async fn a_non_utf8_listing_line_makes_the_canonical_facts_unknown() {
    // Any line whose raw bytes are not valid UTF-8 makes the whole listing unreadable, never a
    // partial success.
    let (app, cookie) = setup().await;
    let mut bytes = canonical_listing(KERNEL, "580.173.02").into_bytes();
    bytes.extend_from_slice(b"-rw-r--r-- root/root 1 2026-09-28 10:15 squashfs-root/usr/bad-");
    bytes.push(0xff);
    bytes.push(b'\n');
    app.host.preset(
        "unsquashfs",
        &["-lls"],
        FakeOutput {
            stdout: bytes,
            ..FakeOutput::ok()
        },
    );
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["canonical"]["version"]["unknown"]["key"], REASON_FORMAT_UNRECOGNIZED,
        "{body}"
    );
}

#[tokio::test]
async fn all_six_readings_are_present() {
    let (app, cookie) = setup().await;
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, 200);
    assert_eq!(body["loaded_version"]["value"], "580.173.02");
    assert_eq!(body["canonical"]["version"]["value"], "580.173.02");
    assert_eq!(
        body["canonical"]["sha256"]["value"],
        sha256_hex(b"driver image")
    );
    assert_eq!(body["truenas_version"]["value"], TRUENAS_VERSION);
    assert_eq!(body["kernel"]["value"], KERNEL);
    assert_eq!(body["link"]["exists"], true);
    assert_eq!(body["link"]["points_to_canonical"], true);
    assert_eq!(body["original_saved"]["saved"], false);
    assert!(body["interrupted"].is_null());
    assert!(body["active_task"].is_null());
    assert_eq!(body["update_blocked"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn not_loaded_when_no_nvidia_module() {
    let (app, cookie) = setup().await;
    for m in ["nvidia", "nvidia_uvm", "nvidia_modeset", "nvidia_drm"] {
        app.host.unload_module(m);
    }
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(body["loaded_version"]["not_loaded"], true);
}

#[tokio::test]
async fn one_failing_reading_does_not_affect_the_others() {
    let (app, cookie) = setup().await;
    app.host.remove("/data/manifest.json");
    app.host.remove(CANONICAL_PATH);
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(
        body["truenas_version"]["unknown"]["key"],
        REASON_FILE_MISSING
    );
    assert_eq!(
        body["canonical"]["sha256"]["unknown"]["key"],
        REASON_FILE_MISSING
    );
    assert_eq!(body["kernel"]["value"], KERNEL);
    assert_eq!(body["loaded_version"]["value"], "580.173.02");
}

#[tokio::test]
async fn missing_link_is_reported_and_blocks_updates() {
    let (app, cookie) = setup().await;
    app.host.remove("/run/extensions/nvidia.raw");
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(body["link"]["exists"], false);
    let blocked = body["update_blocked"].as_array().unwrap();
    assert!(blocked.iter().any(|b| b["key"] == "extension_missing"));
}

#[tokio::test]
async fn link_pointing_elsewhere_is_reported() {
    let (app, cookie) = setup().await;
    app.host
        .put_link("/run/extensions/nvidia.raw", "/mnt/<pool>/other/nvidia.raw");
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(body["link"]["exists"], true);
    assert_eq!(body["link"]["points_to_canonical"], false);
    assert_eq!(body["link"]["target"], "/mnt/<pool>/other/nvidia.raw");
}

#[tokio::test]
async fn interrupted_task_is_shown_with_its_phase() {
    let (app, cookie) = setup().await;
    let id = "20260928T101530Z-3f9a0c12";
    write_state(&app.host, id, Status::Interrupted, Some(Phase::Unload));
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(body["interrupted"]["task_id"], id);
    assert_eq!(body["interrupted"]["phase"], "unload");
}

#[tokio::test]
async fn running_state_file_becomes_interrupted_and_unblocks_updates() {
    let (app, cookie) = setup().await;
    let id = "20260928T101530Z-3f9a0c12";
    write_state(&app.host, id, Status::Running, Some(Phase::Download));
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(body["interrupted"]["task_id"], id);
    assert_eq!(body["interrupted"]["phase"], "download");
    assert!(body["active_task"].is_null());
    let (_, list) = get(&app, "/api/tasks", Some(&cookie)).await;
    assert_eq!(list["tasks"][0]["status"], "INTERRUPTED");
}

#[tokio::test]
async fn original_saved_is_reported_when_a_matching_row_exists() {
    let (app, cookie) = setup().await;
    app.host.put_text(
        "/var/lib/nvm/drivers/20260928T101530Z-3f9a0c12-original/nvidia.raw",
        "original bytes",
    );
    app.host.put_text(
        "/var/lib/nvm/drivers/20260928T101530Z-3f9a0c12-original/record.json",
        &format!(
            r#"{{"kind":"original","version":"580.173.02","sha256":"{}","size":14,"target_kernel":"{KERNEL}","created_at":42,"fingerprint":null,"original_of":{{"kernel":"{KERNEL}","truenas_version":"{TRUENAS_VERSION}"}},"keep":true}}"#,
            sha256_hex(b"original bytes")
        ),
    );
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(body["original_saved"]["saved"], true);
    assert_eq!(body["original_saved"]["version"], "580.173.02");
    assert_eq!(body["original_saved"]["saved_at"], 42);
}

#[tokio::test]
async fn startup_failure_gives_unknown_readings_and_a_setup_block() {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let mut startup = healthy_startup(&host);
    startup.pid_host_ok = false;
    startup.host_data_dir = None;
    startup.problems.push(StartupProblem::PidHost);
    let app = app(host.clone(), settings(), startup);
    let _ = get(&app, "/api/login", None).await;
    let cookie = setup_app(&app, "hunter12").await;
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(
        body["canonical"]["sha256"]["unknown"]["key"],
        REASON_STARTUP_CHECK_FAILED
    );
    assert_eq!(body["link"]["unknown"]["key"], REASON_STARTUP_CHECK_FAILED);
    let blocked = body["update_blocked"].as_array().unwrap();
    assert!(blocked.iter().any(|b| b["key"] == "setup"), "{body}");
}

fn write_at(host: &FakeHost, id: &str, status: Status, phase: Option<Phase>, created_at: u64) {
    let mut state = write_state(host, id, status, phase);
    state.created_at = created_at;
    truenas_nvidia_driver_manager::task_state::write(
        host,
        &truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR),
        &state,
    )
    .unwrap();
}

#[tokio::test]
async fn an_empty_task_collection_has_no_latest_task_and_no_gpus() {
    let (app, cookie) = setup().await;
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert!(body["latest_task"].is_null(), "{body}");
    assert!(body["interrupted"].is_null(), "{body}");
    assert_eq!(body["gpus"]["value"]["devices"], serde_json::json!([]));
}

#[tokio::test]
async fn a_newer_recorded_task_covers_an_older_interrupt_even_when_its_id_sorts_lower() {
    let (app, cookie) = setup().await;
    // The interrupt id sorts first under task list's id order. Recorded time selects the later
    // failure instead, so the home interrupt notice stays off.
    write_at(
        &app.host,
        "20261005T000000Z-ffffffff",
        Status::Interrupted,
        Some(Phase::Unload),
        100,
    );
    write_at(
        &app.host,
        "20261004T000000Z-00000001",
        Status::Failed,
        Some(Phase::Check),
        200,
    );
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert!(body["interrupted"].is_null(), "{body}");
    assert_eq!(body["latest_task"]["task_id"], "20261004T000000Z-00000001");
    assert_eq!(body["latest_task"]["status"], "FAILED");
    assert_eq!(body["latest_task"]["created_at"], 200);
    assert!(body["latest_task"]["finished_at"].is_null(), "{body}");
    assert!(body["latest_task"]["interrupt_discovered_at"].is_null());

    let (_, list) = get(&app, "/api/tasks", Some(&cookie)).await;
    let ids: Vec<&str> = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["20261005T000000Z-ffffffff", "20261004T000000Z-00000001"]
    );
    assert_eq!(list["tasks"][0]["created_at"], 100);
    assert!(list["tasks"][0]["finished_at"].is_null());
    assert!(list["tasks"][0]["interrupt_discovered_at"].is_null());

    let (_, older) = get(&app, "/api/tasks/20261005T000000Z-ffffffff", Some(&cookie)).await;
    assert_eq!(older["task_id"], "20261005T000000Z-ffffffff");
    assert_eq!(older["status"], "INTERRUPTED");
    assert_eq!(older["created_at"], 100);
}

#[tokio::test]
async fn equal_recorded_times_are_unknown_and_do_not_echo_an_interrupt() {
    let (app, cookie) = setup().await;
    write_at(
        &app.host,
        "20261005T000000Z-aaaaaaaa",
        Status::Interrupted,
        Some(Phase::Download),
        100,
    );
    write_at(
        &app.host,
        "20261004T000000Z-bbbbbbbb",
        Status::Failed,
        Some(Phase::Build),
        100,
    );
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert!(body["interrupted"].is_null(), "{body}");
    assert_eq!(body["latest_task"]["unknown"]["key"], "task.latest_unknown");
    assert_eq!(
        body["latest_task"]["unknown"]["params"],
        serde_json::json!({})
    );
}

#[tokio::test]
async fn a_recovery_record_makes_the_latest_task_unknown() {
    let (app, cookie) = setup().await;
    write_at(
        &app.host,
        "20261001T000000Z-aaaaaaaa",
        Status::Interrupted,
        Some(Phase::Unload),
        50,
    );
    let mut recovery = write_state(
        &app.host,
        "20261002T000000Z-bbbbbbbb",
        Status::Interrupted,
        Some(Phase::Unload),
    );
    recovery.phase = None;
    recovery.plan = None;
    recovery.created_at = 9_000_000;
    recovery.finished_at = Some(9_000_000);
    recovery.state_note = Some(truenas_nvidia_driver_manager::task_state::StateNote {
        state_read_error: "missing".into(),
        kept_as: None,
    });
    truenas_nvidia_driver_manager::task_state::write(
        &app.host,
        &truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR),
        &recovery,
    )
    .unwrap();

    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert!(body["interrupted"].is_null(), "{body}");
    assert_eq!(body["latest_task"]["unknown"]["key"], "task.latest_unknown");

    let (_, list) = get(&app, "/api/tasks", Some(&cookie)).await;
    let recovery_row = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == "20261002T000000Z-bbbbbbbb")
        .unwrap();
    assert!(recovery_row["created_at"].is_null(), "{recovery_row}");
    assert!(recovery_row["finished_at"].is_null(), "{recovery_row}");
    assert!(recovery_row["interrupt_discovered_at"].is_null());
    let interrupt_row = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == "20261001T000000Z-aaaaaaaa")
        .unwrap();
    assert_eq!(interrupt_row["created_at"], 50);
}

#[tokio::test]
async fn an_older_looking_recovery_record_still_makes_the_latest_task_unknown() {
    let (app, cookie) = setup().await;
    write_at(
        &app.host,
        "20261004T000000Z-aaaaaaaa",
        Status::Interrupted,
        Some(Phase::Build),
        500,
    );
    let mut recovery = write_state(
        &app.host,
        "20261001T000000Z-bbbbbbbb",
        Status::Failed,
        Some(Phase::Write),
    );
    // The synthetic clock is earlier than the real interrupt. It is still not an original
    // created_at, so the selection stays unknown.
    recovery.created_at = 10;
    recovery.finished_at = Some(10);
    recovery.state_note = Some(truenas_nvidia_driver_manager::task_state::StateNote {
        state_read_error: "missing".into(),
        kept_as: None,
    });
    truenas_nvidia_driver_manager::task_state::write(
        &app.host,
        &truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR),
        &recovery,
    )
    .unwrap();

    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert!(body["interrupted"].is_null(), "{body}");
    assert_eq!(body["latest_task"]["unknown"]["key"], "task.latest_unknown");
}

#[tokio::test]
async fn a_running_task_with_a_later_recorded_time_covers_an_older_interrupt() {
    let (app, cookie) = setup().await;
    write_at(
        &app.host,
        "20261005T000000Z-ffffffff",
        Status::Interrupted,
        Some(Phase::Unload),
        100,
    );
    let id = "20261006T000000Z-00000002";
    write_at(&app.host, id, Status::Running, Some(Phase::Download), 300);
    app.host.set_unit(id, "active", "running", "success", 0);
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert!(body["interrupted"].is_null(), "{body}");
    assert_eq!(body["latest_task"]["task_id"], id);
    assert_eq!(body["latest_task"]["status"], "RUNNING");
    assert_eq!(body["latest_task"]["created_at"], 300);
    assert_eq!(body["active_task"]["task_id"], id);
}

#[tokio::test]
async fn an_interrupt_discovered_by_reconcile_is_not_shown_as_the_end_time() {
    let (app, cookie) = setup().await;
    app.host.set_wall_ms(424_242);
    let id = "20261007T000000Z-00000003";
    write_at(&app.host, id, Status::Running, Some(Phase::Download), 100);
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(body["interrupted"]["task_id"], id);
    assert_eq!(body["interrupted"]["created_at"], 100);
    assert!(body["interrupted"]["finished_at"].is_null(), "{body}");
    assert_eq!(body["interrupted"]["interrupt_discovered_at"], 424_242);
    assert_eq!(body["latest_task"]["interrupt_discovered_at"], 424_242);

    let (_, detail) = get(&app, &format!("/api/tasks/{id}"), Some(&cookie)).await;
    assert_eq!(detail["status"], "INTERRUPTED");
    assert_eq!(detail["created_at"], 100);
    assert!(detail["finished_at"].is_null(), "{detail}");
    assert_eq!(detail["interrupt_discovered_at"], 424_242);

    let (_, list) = get(&app, "/api/tasks", Some(&cookie)).await;
    assert_eq!(list["tasks"][0]["finished_at"], serde_json::Value::Null);
    assert_eq!(list["tasks"][0]["interrupt_discovered_at"], 424_242);
}

#[tokio::test]
async fn home_names_a_listed_gpu_from_the_table_and_omits_an_unlisted_name() {
    let (app, cookie) = setup().await;
    app.host.add_gpu("0000:02:00.0", 0x0eee, Some("nvidia"));
    app.host.add_gpu("0000:01:00.0", 0x2c33, Some("nvidia"));
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    let devices = &body["gpus"]["value"]["devices"];
    assert_eq!(devices[0]["address"], "0000:01:00.0");
    assert_eq!(devices[0]["device"], "2c33");
    assert_eq!(devices[0]["passthrough"], false);
    assert_eq!(
        devices[0]["name"],
        "NVIDIA RTX PRO 4000 Blackwell SFF Edition"
    );
    assert_eq!(devices[1]["address"], "0000:02:00.0");
    assert_eq!(devices[1]["device"], "0eee");
    assert!(devices[1].get("name").is_none(), "{devices}");
}

#[tokio::test]
async fn a_vfio_only_gpu_is_passthrough_and_distinct_from_no_gpu() {
    let (app, cookie) = setup().await;
    app.host.add_gpu("0000:03:00.0", 0x2c33, Some("vfio-pci"));
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    let devices = body["gpus"]["value"]["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["passthrough"], true);
    assert_eq!(devices[0]["device"], "2c33");
}

#[tokio::test]
async fn an_unreadable_gpu_scan_does_not_blank_the_other_home_readings() {
    let (app, cookie) = setup().await;
    app.host.remove_tree("/sys/bus/pci/devices");
    let (_, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(
        body["gpus"]["unknown"]["key"], REASON_FILE_MISSING,
        "{body}"
    );
    assert_eq!(body["loaded_version"]["value"], "580.173.02");
    assert_eq!(body["kernel"]["value"], KERNEL);
    assert!(body["gpus"].get("value").is_none(), "{body}");
}

#[tokio::test]
async fn startup_failure_does_not_scan_gpus() {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    host.mkdir("/sys/bus/pci/devices/not-a-pci");
    let mut startup = healthy_startup(&host);
    startup.pid_host_ok = false;
    startup.problems.push(StartupProblem::PidHost);
    let app = app(host, settings(), startup);
    let _ = get(&app, "/api/login", None).await;
    let cookie = setup_app(&app, "hunter12").await;
    app.host.clear_ops();
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["gpus"]["unknown"]["key"], REASON_STARTUP_CHECK_FAILED,
        "{body}"
    );
    assert!(
        app.host
            .ops()
            .iter()
            .all(|op| !op.path.contains("pci/devices")),
        "startup failure scanned pci: {:?}",
        app.host.ops()
    );
}

#[tokio::test]
async fn a_logged_out_home_request_does_not_read_the_host_gpus() {
    let (app, _cookie) = setup().await;
    app.host.add_gpu("0000:01:00.0", 0x2c33, Some("nvidia"));
    app.host.clear_ops();
    let (status, body) = get(&app, "/api/home", None).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED, "{body}");
    assert!(
        app.host
            .ops()
            .iter()
            .all(|op| !op.path.contains("pci/devices")),
        "logged-out home read pci: {:?}",
        app.host.ops()
    );
}

#[tokio::test]
async fn a_failed_canonical_listing_keeps_the_exit_code() {
    // The reading reason carries its phrase key and placeholder value.
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    host.set_unsquashfs_broken(CANONICAL_PATH);
    let app = app(host.clone(), settings(), healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let cookie = setup_app(&app, "longpass99").await;
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(
        body["canonical"]["version"]["unknown"],
        serde_json::json!({"key": REASON_EXIT_STATUS, "params": {"code": 1}})
    );
}

//! Planning/submission, the plan slot, submission and local sources.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::startup::StartupProblem;
use truenas_nvidia_driver_manager::task_state::{OriginalChoice, Phase, Status};

const INDEX: &str = r#"<a href="615.71.09/">615.71.09</a>
<a href="610.57.04/">610.57.04</a>
<a href="595.104.02/">595.104.02</a>
<a href="590.48.01/">590.48.01</a>
<a href="580.178.04/">580.178.04</a>
"#;

async fn setup() -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    prime_kernel_headers(&host, KERNEL);
    host.set_curl(CATALOG_URL, 200, INDEX.as_bytes());
    signed_in_app(host).await
}

async fn official_plan(
    app: &TestApp,
    cookie: &str,
    version: &str,
) -> (StatusCode, serde_json::Value) {
    post_plan(
        app,
        cookie,
        json!({"kind": "official", "version": version, "gpus": []}),
    )
    .await
}

#[tokio::test]
async fn planning_returns_the_dialog_for_an_official_version() {
    let (app, cookie) = setup().await;
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"], "ready");
    assert!(body["plan_id"].is_string());
    let dialog = &body["dialog"];
    assert_eq!(dialog["source"]["kind"], "official");
    assert_eq!(dialog["version"], "580.178.04");
    assert_eq!(dialog["kernel"], KERNEL);
    assert_eq!(dialog["truenas_version"], TRUENAS_VERSION);
    assert_eq!(dialog["original"]["saved"], false);
    assert_eq!(dialog["save_available"], true);
    assert_eq!(
        dialog["confirmation"],
        json!({
            "original": "save_available",
            "buttons": [{"button": "save", "enabled": true}, {"button": "skip", "enabled": true}],
            "how": {"key": "confirm.how.official", "params": {}},
            "warnings": [{"key": "original.skip_warning", "params": {}}],
        })
    );
}

#[tokio::test]
async fn submission_starts_a_task_and_writes_the_first_state() {
    let (app, cookie) = setup().await;
    let (_, plan) = official_plan(&app, &cookie, "580.178.04").await;
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["task_id"].as_str().unwrap().to_string();
    assert!(id.starts_with("2026"));
    let state: serde_json::Value = serde_json::from_slice(
        &app.host
            .file(&format!("/var/lib/nvm/manager/state/{id}.json"))
            .expect("state file"),
    )
    .unwrap();
    assert_eq!(state["status"], "RUNNING");
    assert_eq!(state["phase"], "read_loaded_version");
    assert_eq!(state["plan"]["original"], "skip");
    assert_eq!(state["plan"]["version"], "580.178.04");
    assert_eq!(
        app.host.count("systemd-run"),
        1,
        "{:?}",
        app.host.argv_log()
    );
    app.host.wait_for_units(std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn a_plan_can_be_submitted_only_once() {
    let (app, cookie) = setup().await;
    let (_, plan) = official_plan(&app, &cookie, "580.178.04").await;
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    let (status, _) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "plan.used");
    assert_eq!(app.host.count("systemd-run"), 1);
    app.host.wait_for_units(std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn a_changed_kernel_is_rejected_with_plan_changes() {
    let (app, cookie) = setup().await;
    let (_, plan) = official_plan(&app, &cookie, "580.178.04").await;
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    app.host.set_uname("6.12.96-production+truenas");
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "plan.changed");
    assert!(body["params"]["changed"]
        .as_array()
        .unwrap()
        .contains(&json!("kernel")));
    assert_eq!(app.host.count("systemd-run"), 0);
}

#[tokio::test]
async fn an_invalid_button_is_rejected() {
    let (app, cookie) = setup().await;
    let (_, plan) = official_plan(&app, &cookie, "580.178.04").await;
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    let (status, body) = post_task(&app, &cookie, &plan_id, "start").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.button_invalid");
}

#[tokio::test]
async fn a_running_task_blocks_new_plans_and_submissions() {
    let (app, cookie) = setup().await;
    let gate = app.host.set_gate("curl", &["--silent"]);
    let (_, plan) = official_plan(&app, &cookie, "580.178.04").await;
    let id = "20260928T101530Z-3f9a0c12";
    let mut state = write_state(&app.host, id, Status::Running, Some(Phase::Download));
    state.plan.as_mut().unwrap().original = OriginalChoice::Skip;
    truenas_nvidia_driver_manager::task_state::write(
        &app.host,
        &truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR),
        &state,
    )
    .unwrap();
    let started = truenas_nvidia_driver_manager::tasks::start(
        &app.host,
        std::path::Path::new(HOST_DATA_DIR),
        id,
    );
    assert!(matches!(
        started,
        truenas_nvidia_driver_manager::tasks::StartResult::Started
    ));
    for _ in 0..50 {
        if app.host.unit_ids().contains(&id.to_string()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (_, tasks) = get(&app, "/api/tasks", Some(&cookie)).await;
    assert!(!tasks["active_task"].is_null(), "{tasks}");
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200);
    assert_eq!(body["result"], "blocked");
    assert!(body["failures"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["item"] == "task_running"));
    let (status, body) = post_task(&app, &cookie, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "task.running");
    gate.release();
    app.host.wait_for_units(std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn blocked_prechecks_are_reported() {
    let (app, cookie) = setup().await;
    app.host.remove("/run/extensions/nvidia.raw");
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200);
    assert_eq!(body["result"], "blocked");
    assert!(body["failures"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["item"] == "link_missing"));
    app.host
        .put_link("/run/extensions/nvidia.raw", CANONICAL_PATH);
    app.host.open_device("nvidia");
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200);
    assert_eq!(body["result"], "blocked");
    assert_eq!(body["gpu"]["state"], "busy");
}

#[tokio::test]
async fn unreadable_running_task_state_is_reported() {
    let (app, cookie) = setup().await;
    app.host.remove("/var/lib/nvm/manager/state");
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200);
    assert_eq!(body["result"], "blocked");
    let failures = body["failures"].as_array().unwrap();
    assert!(failures.iter().any(|f| f["item"] == "unreadable"
        && f["object"] == "running_task"
        && !f["reason"].is_null()));
}

#[tokio::test]
async fn setup_problems_block_plans() {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let mut startup = healthy_startup(&host);
    startup.problems.push(StartupProblem::PidHost);
    let app = app(host.clone(), settings("hunter12"), startup);
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "longpass99").await;
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "setup.blocked");
}

#[tokio::test]
async fn local_source_plans_and_submits() {
    let (app, cookie) = setup().await;
    let bytes = b"local row bytes";
    app.host.put_file(
        "/var/lib/nvm/drivers/20260928T101530Z-3f9a0c12-build/nvidia.raw",
        bytes.to_vec(),
    );
    let record = format!(
        r#"{{"kind":"official","version":"580.95.05","sha256":"{}","size":{},"target_kernel":"{KERNEL}","created_at":7,"fingerprint":null,"original_of":null,"keep":null}}"#,
        sha256_hex(bytes),
        bytes.len()
    );
    app.host.put_text(
        "/var/lib/nvm/drivers/20260928T101530Z-3f9a0c12-build/record.json",
        &record,
    );
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "local", "row_id": "20260928T101530Z-3f9a0c12-build"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"], "ready");
    assert_eq!(body["dialog"]["source"]["kind"], "local");
    assert_eq!(body["dialog"]["source"]["row_kind"], "official");
    assert_eq!(body["dialog"]["version"], "580.95.05");
    assert_eq!(
        body["dialog"]["confirmation"]["warnings"],
        json!([{"key": "original.skip_warning", "params": {}}])
    );
    let (status, task) = post_task(&app, &cookie, &body["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = task["task_id"].as_str().unwrap().to_string();
    let state: serde_json::Value = serde_json::from_slice(
        &app.host
            .file(&format!("/var/lib/nvm/manager/state/{id}.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(state["plan"]["source"]["kind"], "local");
    assert_eq!(state["plan"]["gpus"].as_array().unwrap().len(), 0);
    app.host.wait_for_units(std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn a_missing_local_row_is_404() {
    let (app, cookie) = setup().await;
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "local", "row_id": "20260928T101530Z-3f9a0c12-build"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "plan.source_invalid.local_row_missing");
}

async fn write_local_row(app: &TestApp, version: &str) -> String {
    let row_id = "20260928T101530Z-3f9a0c12-build";
    let bytes = b"local row bytes";
    app.host.put_file(
        &format!("/var/lib/nvm/drivers/{row_id}/nvidia.raw"),
        bytes.to_vec(),
    );
    let record = format!(
        r#"{{"kind":"official","version":"{version}","sha256":"{}","size":{},"target_kernel":"{KERNEL}","created_at":7,"fingerprint":null,"original_of":null,"keep":null}}"#,
        sha256_hex(bytes),
        bytes.len()
    );
    app.host.put_text(
        &format!("/var/lib/nvm/drivers/{row_id}/record.json"),
        &record,
    );
    row_id.to_string()
}

#[tokio::test]
async fn a_local_record_version_change_is_rejected() {
    let (app, cookie) = setup().await;
    let row_id = write_local_row(&app, "580.95.05").await;
    let (status, body) = post_plan(&app, &cookie, json!({"kind": "local", "row_id": row_id})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let plan_id = body["plan_id"].as_str().unwrap().to_string();
    let bytes = b"local row bytes";
    let record = format!(
        r#"{{"kind":"official","version":"590.1.2","sha256":"{}","size":{},"target_kernel":"{KERNEL}","created_at":7,"fingerprint":null,"original_of":null,"keep":null}}"#,
        sha256_hex(bytes),
        bytes.len()
    );
    app.host.put_text(
        &format!("/var/lib/nvm/drivers/{row_id}/record.json"),
        &record,
    );
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "plan.changed");
    assert!(
        body["params"]["changed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "version"),
        "{body}"
    );
}

#[tokio::test]
async fn a_local_unreadable_record_is_blocked_not_404() {
    // A present but unreadable record produces two `reason.record_unreadable` checks and a 200
    // blocked result.
    let (app, cookie) = setup().await;
    let row_id = "20260928T101530Z-3f9a0c12-build";
    app.host.mkdir(&format!("/var/lib/nvm/drivers/{row_id}"));
    app.host.put_text(
        &format!("/var/lib/nvm/drivers/{row_id}/record.json"),
        "not-json",
    );
    let (status, body) = post_plan(&app, &cookie, json!({"kind": "local", "row_id": row_id})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "blocked");
    let failures = body["failures"].as_array().unwrap();
    let objects: Vec<&str> = failures
        .iter()
        .filter_map(|f| f["object"].as_str())
        .collect();
    assert!(objects.contains(&"local_checksum"), "{body}");
    assert!(objects.contains(&"local_target_kernel"), "{body}");
    for f in failures {
        if f["object"].is_string() {
            assert_eq!(f["reason"]["key"], REASON_RECORD_UNREADABLE, "{body}");
        }
    }
}

/// A web process whose host carries the given counted GPUs.
async fn setup_with_gpus(gpus: &[(&str, u16, Option<&str>)]) -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    prime_kernel_headers(&host, KERNEL);
    host.set_curl(CATALOG_URL, 200, INDEX.as_bytes());
    for (address, device, driver) in gpus {
        host.add_gpu(address, *device, *driver);
    }
    signed_in_app(host).await
}

async fn plan_gpus(
    app: &TestApp,
    cookie: &str,
    version: &str,
    gpus: &[&str],
) -> (StatusCode, serde_json::Value) {
    post_plan(
        app,
        cookie,
        json!({"kind": "official", "version": version, "gpus": gpus}),
    )
    .await
}

#[tokio::test]
async fn planning_reports_the_last_branch_and_the_conflict_of_the_selected_gpus() {
    let (app, cookie) = setup_with_gpus(&[
        ("0000:01:00.0", 0x1b80, Some("nvidia")),
        ("0000:02:00.0", 0x2b85, None),
    ])
    .await;
    // 590.48.01 cannot drive the Pascal GPU: the reason carries its name and the last branch it
    // supports.
    let (status, body) = plan_gpus(&app, &cookie, "590.48.01", &["0000:01:00.0"]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "plan.source_invalid.gpu_unsupported");
    assert_eq!(body["params"]["conflict"], false, "{body}");
    let gpu = &body["params"]["gpus"][0];
    assert_eq!(gpu["device"], "1b80");
    assert_eq!(gpu["name"], "NVIDIA GeForce GTX 1080");
    assert_eq!(gpu["last_branch"], 580);
    assert!(gpu.get("first_branch").is_none(), "{body}");

    // Closed-only and open-only in one selection: a conflict, whatever the version.
    let (status, body) = plan_gpus(
        &app,
        &cookie,
        "580.178.04",
        &["0000:01:00.0", "0000:02:00.0"],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["params"]["conflict"], true);
    assert_eq!(body["params"]["gpus"].as_array().unwrap().len(), 0);

    // The open-only GPU alone is fine and plans the open module type.
    let (status, body) = plan_gpus(&app, &cookie, "590.48.01", &["0000:02:00.0"]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "ready");
}

#[tokio::test]
async fn planning_reports_the_unsupported_gpus_sorted_by_address() {
    // The payload lists the GPUs that reject the selected version in address order, whatever order
    // the request named them in.
    let (app, cookie) = setup_with_gpus(&[
        ("0000:01:00.0", 0x1b80, Some("nvidia")),
        ("0000:03:00.0", 0x1b80, Some("nvidia")),
    ])
    .await;
    let (status, body) = plan_gpus(
        &app,
        &cookie,
        "590.48.01",
        &["0000:03:00.0", "0000:01:00.0"],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "plan.source_invalid.gpu_unsupported");
    let addresses: Vec<&str> = body["params"]["gpus"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["address"].as_str().unwrap())
        .collect();
    assert_eq!(addresses, vec!["0000:01:00.0", "0000:03:00.0"], "{body}");
    assert_eq!(body["params"]["conflict"], false, "{body}");
}

#[tokio::test]
async fn a_gpu_outside_the_table_is_judged_by_its_device_id_section() {
    let (app, cookie) = setup_with_gpus(&[
        ("0000:01:00.0", 0x1b07, Some("nvidia")),
        ("0000:02:00.0", 0x2684, Some("nvidia")),
    ])
    .await;
    // 0x1B07 sits in 0x1340–0x1DFF: last branch 580, closed modules only.
    let (status, body) = plan_gpus(&app, &cookie, "580.178.04", &["0000:01:00.0"]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "ready");
    let (status, body) = plan_gpus(&app, &cookie, "590.48.01", &["0000:01:00.0"]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let gpu = &body["params"]["gpus"][0];
    assert_eq!(gpu["device"], "1b07");
    assert_eq!(gpu["last_branch"], 580);
    assert!(gpu.get("name").is_none(), "no table name for {body}");

    // 0x2684 (Either) and 0x1B07 (ClosedOnly) are compatible on 580.
    let (status, body) = plan_gpus(
        &app,
        &cookie,
        "580.178.04",
        &["0000:01:00.0", "0000:02:00.0"],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A "newer than the first supported version" GPU reports first_branch.
    let (app96, cookie96) = setup_with_gpus(&[("0000:01:00.0", 0x2bb2, Some("nvidia"))]).await;
    app96.host.set_uname("6.12.96-production+truenas");
    app96.host.put_text(
        "/usr/src/linux-headers-truenas-production-amd64/include/linux/pci.h",
        "int pci_resize_resource(struct pci_dev *dev, int resno, int size);\n",
    );
    let (status, body) = plan_gpus(&app96, &cookie96, "565.77", &["0000:01:00.0"]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "plan.source_invalid.gpu_unsupported");
    let gpu = &body["params"]["gpus"][0];
    assert_eq!(gpu["device"], "2bb2");
    assert_eq!(gpu["first_branch"], 570);
    assert!(gpu.get("last_branch").is_none(), "{body}");
}

#[tokio::test]
async fn a_selected_gpu_bound_to_vfio_after_versions_is_reported_as_changed() {
    let (app, cookie) = setup_with_gpus(&[
        ("0000:01:00.0", 0x1b80, Some("nvidia")),
        ("0000:02:00.0", 0x2b85, None),
    ])
    .await;
    let (status, _) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    app.host.set_gpu_driver("0000:02:00.0", Some("vfio-pci"));
    let (status, body) = plan_gpus(&app, &cookie, "590.48.01", &["0000:02:00.0"]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "plan.changed");
    assert_eq!(body["params"]["changed"], json!(["gpus"]));

    // The other direction: nothing counted at versions, then a GPU appears.
    let (app2, cookie2) = setup_with_gpus(&[
        ("0000:01:00.0", 0x1b80, Some("vfio-pci")),
        ("0000:02:00.0", 0x2b85, Some("vfio-pci")),
    ])
    .await;
    let (status, body) = get(&app2, "/api/versions", Some(&cookie2)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["gpus"].as_array().unwrap().len(), 0);
    app2.host.set_gpu_driver("0000:02:00.0", Some("nvidia"));
    let (status, body) = plan_gpus(&app2, &cookie2, "590.48.01", &[]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["params"]["changed"], json!(["gpus"]));
}

#[tokio::test]
async fn an_unreadable_pci_vendor_file_makes_the_gpus_undecidable() {
    let (app, cookie) = setup_with_gpus(&[("0000:01:00.0", 0x1b80, Some("nvidia"))]).await;
    let vendor = "/sys/bus/pci/devices/0000:01:00.0/vendor";
    app.host.fail_read(vendor);
    // Versions reports the empty list with the sysfs path.
    let (status, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    let unreadable = &body["unreadable"][0];
    assert_eq!(unreadable["object"], "gpus");
    assert_eq!(unreadable["raw"], vendor);
    // Planning refuses with the undecidable reading, before any plan exists.
    let (status, body) = plan_gpus(&app, &cookie, "590.48.01", &["0000:01:00.0"]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "plan.source_invalid.gpu_undecidable");
    assert_eq!(body["params"]["unreadable"][0]["object"], "gpus");
    assert_eq!(body["params"]["unreadable"][0]["raw"], vendor);
}

#[tokio::test]
async fn the_dialog_reports_the_four_original_choices_and_an_unsavable_original() {
    let (app, cookie) = setup().await;
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dialog"]["original"]["saved"], false);
    assert_eq!(body["dialog"]["save_available"], true);
    // Start is not a legal button while nothing is saved.
    let plan_id = body["plan_id"].as_str().unwrap().to_string();
    let (status, body) = post_task(&app, &cookie, &plan_id, "start").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.button_invalid");
    assert_eq!(app.host.count("systemd-run"), 0);

    // A canonical file whose version cannot be read: only "skip" is legal.
    let (app2, cookie2) = setup().await;
    app2.host.set_listing(
        CANONICAL_PATH,
        &canonical_listing(KERNEL, "580.173.02").replace("libnvidia-ml.so.580.173.02", "other"),
    );
    let (status, body) = official_plan(&app2, &cookie2, "580.178.04").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dialog"]["save_available"], false);
    assert_eq!(body["dialog"]["confirmation"]["original"], "unavailable");
    assert_eq!(
        body["dialog"]["confirmation"]["buttons"],
        json!([
            {"button":"save", "enabled":false}, {"button":"skip", "enabled":true}
        ])
    );
    assert_eq!(
        body["dialog"]["confirmation"]["warnings"],
        json!([
            {"key":"confirm.original.unsavable", "params":{}},
            {"key":"original.skip_warning", "params":{}}
        ])
    );
    let plan_id = body["plan_id"].as_str().unwrap().to_string();
    let (status, body) = post_task(&app2, &cookie2, &plan_id, "save").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "plan.button_invalid");
    // Any submission consumes the one-shot snapshot; the legal button needs a fresh dialog.
    let (status, plan) = official_plan(&app2, &cookie2, "580.178.04").await;
    assert_eq!(status, 200, "{plan}");
    let (status, body) = post_task(&app2, &cookie2, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    app2.host.wait_for_units(std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn a_changed_truenas_version_or_original_is_listed_by_plan_changes() {
    let (app, cookie) = setup().await;
    let (_, plan) = official_plan(&app, &cookie, "580.178.04").await;
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    app.host
        .put_text("/data/manifest.json", r#"{"version":"25.10.8"}"#);
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "plan.changed");
    assert!(body["params"]["changed"]
        .as_array()
        .unwrap()
        .contains(&json!("truenas_version")));

    // An original that appears after the dialog changes the "original" item.
    let (app2, cookie2) = setup().await;
    let (_, plan) = official_plan(&app2, &cookie2, "580.178.04").await;
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    let row = "20260928T101530Z-3f9a0c11-original";
    let bytes = b"original bytes";
    app2.host.put_file(
        &format!("/var/lib/nvm/drivers/{row}/nvidia.raw"),
        bytes.to_vec(),
    );
    app2.host.put_text(
        &format!("/var/lib/nvm/drivers/{row}/record.json"),
        &format!(
            r#"{{"kind":"original","version":"580.173.02","sha256":"{}","size":{},"target_kernel":"{KERNEL}","created_at":7,"fingerprint":null,"original_of":{{"kernel":"{KERNEL}","truenas_version":"{TRUENAS_VERSION}"}},"keep":true}}"#,
            sha256_hex(bytes),
            bytes.len()
        ),
    );
    let (status, body) = post_task(&app2, &cookie2, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["params"]["changed"]
        .as_array()
        .unwrap()
        .contains(&json!("original")));
    assert_eq!(app2.host.count("systemd-run"), 0);
}

#[tokio::test]
async fn a_link_that_disappears_before_the_submit_is_a_precheck_failure() {
    let (app, cookie) = setup().await;
    let (_, plan) = official_plan(&app, &cookie, "580.178.04").await;
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    app.host.remove("/run/extensions/nvidia.raw");
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "plan.precheck_failed");
    assert_eq!(body["params"]["failures"][0]["item"], "link_missing");
    assert_eq!(app.host.count("systemd-run"), 0);
}

#[tokio::test]
async fn two_submissions_of_the_same_plan_race_but_only_one_wins() {
    let (app, cookie) = setup().await;
    let (_, plan) = official_plan(&app, &cookie, "580.178.04").await;
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    let first = post_task(&app, &cookie, &plan_id, "skip");
    let second = post_task(&app, &cookie, &plan_id, "skip");
    let ((s1, b1), (s2, b2)) = tokio::join!(first, second);
    let statuses = [s1, s2];
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::ACCEPTED)
            .count(),
        1,
        "{b1} {b2}"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    assert_eq!(app.host.count("systemd-run"), 1);
    app.host.wait_for_units(std::time::Duration::from_secs(5));
}

fn docker_inspect(
    name: &str,
    visible: Option<&str>,
    device_request: Option<(&str, &[&str])>,
    devices: &[&str],
) -> Vec<serde_json::Value> {
    let env = match visible {
        Some(v) => json!([format!("NVIDIA_VISIBLE_DEVICES={v}")]),
        None => json!([]),
    };
    let requests: Vec<serde_json::Value> = match device_request {
        Some((driver, caps)) => vec![json!({
            "Driver": driver,
            "Capabilities": [caps],
        })],
        None => Vec::new(),
    };
    let devs: Vec<serde_json::Value> = devices.iter().map(|p| json!({"PathOnHost": p})).collect();
    vec![json!({
        "Name": format!("/{name}"),
        "Config": {"Env": env},
        "HostConfig": {"DeviceRequests": requests, "Devices": devs},
    })]
}

const CONTAINER_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn a_render_node_open_makes_the_gpu_busy() {
    // An opened render node leaves nvidia_drm one reference above its dependents.
    let (app, cookie) = setup().await;
    app.host.load_module("nvidia_drm", 1, &["nvidia_modeset"]);
    app.host.open_device("nvidia_drm");
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"], "blocked");
    assert_eq!(body["gpu"]["state"], "busy");
    assert_eq!(app.host.count("systemd-run"), 0);
}

#[tokio::test]
async fn a_visible_devices_request_without_an_open_device_still_updates() {
    // The application asked for the GPU but holds no device.
    let (app, cookie) = setup().await;
    app.host.set_docker_ps(&[CONTAINER_ID]);
    app.host.set_docker_inspect(
        &serde_json::Value::Array(docker_inspect("ix-emby-1", Some("all"), None, &[])).to_string(),
    );
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"], "ready");
    let (status, _) = post_task(&app, &cookie, &body["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    app.host.wait_for_units(std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn the_busy_list_covers_the_five_required_object_kinds() {
    let (app, cookie) = setup().await;
    app.host.load_module("nvidia", 3, &[]);
    // A DRM node whose driver is nvidia, for the /dev/dri mappings.
    app.host.add_drm_node("renderD128", "nvidia");
    let quad = "1111111111111111111111111111111111111111111111111111111111111111";
    let penta = "2222222222222222222222222222222222222222222222222222222222222222";
    let mut inspect = Vec::new();
    // 1. TrueNAS App: a device request with Driver nvidia.
    inspect.extend(docker_inspect(
        "ix-emby-1",
        None,
        Some(("nvidia", &[])),
        &[],
    ));
    // 2. --gpus: a device request with the gpu capability.
    inspect.extend(docker_inspect(
        "gpu-runner",
        None,
        Some(("", &["gpu"])),
        &[],
    ));
    // 3. Only NVIDIA_VISIBLE_DEVICES set.
    inspect.extend(docker_inspect("visible-only", Some("all"), None, &[]));
    // 4. Only a NVIDIA /dev/dri node mapped.
    inspect.extend(docker_inspect(
        "dri-only",
        None,
        None,
        &["/dev/dri/renderD128"],
    ));
    app.host.set_docker_ps(&[
        CONTAINER_ID,
        quad,
        penta,
        "3333333333333333333333333333333333333333333333333333333333333333",
    ]);
    app.host
        .set_docker_inspect(&serde_json::Value::Array(inspect).to_string());
    // 5. An Incus container with a GPU device.
    app.host.set_midclt(
        "call virt.instance.query",
        FakeOutput::stdout(
            r#"[{"id":"incus-1","name":"cuda-box","type":"CONTAINER","status":"RUNNING"}]"#,
        ),
    );
    app.host.set_midclt(
        "call virt.instance.device_list incus-1",
        FakeOutput::stdout(r#"[{"dev_type":"GPU"},{"dev_type":"disk"}]"#),
    );
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"], "blocked", "{body}");
    assert_eq!(body["gpu"]["state"], "busy");
    let names: Vec<String> = body["gpu"]["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            format!(
                "{}:{}",
                u["name"].as_str().unwrap(),
                u["type"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        names,
        // Every category is ordered by name; Docker users come first, then the Incus instances.
        vec![
            "dri-only:docker",
            "gpu-runner:docker",
            "ix-emby-1:docker",
            "visible-only:docker",
            "cuda-box:incus"
        ],
        "{body}"
    );
}

#[tokio::test]
async fn one_failed_object_query_leaves_the_other_category_readable() {
    let (app, cookie) = setup().await;
    app.host.load_module("nvidia", 3, &[]);
    app.host.set_docker_ps(&[CONTAINER_ID]);
    app.host.set_docker_inspect(
        &serde_json::Value::Array(docker_inspect("ix-emby-1", Some("all"), None, &[])).to_string(),
    );
    app.host.set_midclt(
        "call virt.instance.query",
        FakeOutput::code(1).with_stderr("midclt: connection refused"),
    );
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["gpu"]["state"], "busy");
    let errors = body["gpu"]["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{body}");
    assert_eq!(errors[0]["type"], "incus");
    let users = body["gpu"]["users"].as_array().unwrap();
    assert!(users.iter().any(|u| u["name"] == "ix-emby-1"), "{body}");

    // The other direction: docker inspect fails, Incus still lists.
    let (app2, cookie2) = setup().await;
    app2.host.load_module("nvidia", 3, &[]);
    app2.host.set_docker_ps(&[CONTAINER_ID]);
    app2.host.set_docker_inspect_fails(true);
    app2.host.set_midclt(
        "call virt.instance.query",
        FakeOutput::stdout(
            r#"[{"id":"incus-1","name":"cuda-box","type":"CONTAINER","status":"RUNNING"}]"#,
        ),
    );
    app2.host.set_midclt(
        "call virt.instance.device_list incus-1",
        FakeOutput::stdout(r#"[{"dev_type":"GPU"}]"#),
    );
    let (_, body) = official_plan(&app2, &cookie2, "580.178.04").await;
    assert_eq!(body["gpu"]["state"], "busy", "{body}");
    let errors = body["gpu"]["errors"].as_array().unwrap();
    assert_eq!(errors[0]["type"], "docker", "{body}");
    assert!(body["gpu"]["users"]
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["name"] == "cuda-box"));
}

#[tokio::test]
async fn an_unreadable_proc_modules_is_reported_as_the_unreadable_occupancy() {
    let (app, cookie) = setup().await;
    app.host.remove("/proc/modules");
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"], "blocked");
    assert_eq!(body["gpu"]["state"], "unreadable");
    assert_eq!(
        body["gpu"]["read_reason"]["key"], REASON_FILE_MISSING,
        "{body}"
    );
    assert_eq!(app.host.count("systemd-run"), 0);

    // A corrupted /proc/modules is a format failure of the reading.
    let (app2, cookie2) = setup().await;
    app2.host
        .put_text("/proc/modules", "garbage without fields\n");
    let (_, body) = official_plan(&app2, &cookie2, "580.178.04").await;
    assert_eq!(body["gpu"]["state"], "unreadable");
    assert_eq!(
        body["gpu"]["read_reason"]["key"], REASON_FORMAT_UNRECOGNIZED,
        "{body}"
    );
}

#[tokio::test]
async fn void_visible_devices_and_an_empty_list_are_not_gpu_users() {
    let (app, cookie) = setup().await;
    app.host.load_module("nvidia", 3, &[]);
    let id = "4444444444444444444444444444444444444444444444444444444444444444";
    app.host.set_docker_ps(&[id]);
    app.host.set_docker_inspect(
        &serde_json::Value::Array(docker_inspect("voided", Some("void"), None, &[])).to_string(),
    );
    let (status, body) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"], "blocked");
    // The module was open, so the state is busy but no Docker user is listed.
    let users = body["gpu"]["users"].as_array().unwrap();
    assert!(users.is_empty(), "{body}");
}

async fn ready_plan_id(app: &TestApp, cookie: &str) -> String {
    let (status, plan) = official_plan(app, cookie, "580.178.04").await;
    assert_eq!(status, 200, "{plan}");
    plan["plan_id"].as_str().unwrap().to_string()
}

async fn submit_plan(
    app: &TestApp,
    cookie: &str,
    plan_id: &str,
) -> (StatusCode, serde_json::Value) {
    post_task(app, cookie, &plan_id, "skip").await
}

#[tokio::test]
async fn a_runner_copy_failure_is_reported_and_leaves_no_trace() {
    let (app, cookie) = setup().await;
    let plan_id = ready_plan_id(&app, &cookie).await;
    app.host.fail_write("/var/lib/nvm/manager/bin");
    let (status, body) = submit_plan(&app, &cookie, &plan_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "submit.failed");
    assert_eq!(body["params"]["reason"]["key"], "submit.runner_copy_failed");
    assert!(
        body["raw"].as_str().is_some_and(|raw| !raw.is_empty()),
        "the system error raw is missing: {body}"
    );
    assert!(app.host.children("/var/lib/nvm/manager/state").is_empty());
    assert!(app.host.children("/var/lib/nvm/manager/tasks").is_empty());
    assert_eq!(app.host.count("systemd-run"), 0);
}

#[tokio::test]
async fn an_unwritable_first_state_file_is_reported_and_leaves_no_trace() {
    let (app, cookie) = setup().await;
    let plan_id = ready_plan_id(&app, &cookie).await;
    app.host.fail_write("/var/lib/nvm/manager/state");
    let (status, body) = submit_plan(&app, &cookie, &plan_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["params"]["reason"]["key"], "submit.state_unwritable");
    assert!(app.host.children("/var/lib/nvm/manager/state").is_empty());
    assert!(app.host.children("/var/lib/nvm/manager/tasks").is_empty());
    assert_eq!(app.host.count("systemd-run"), 0);
}

/// A plain non-zero `systemd-run` exit with no unit to show is **not** proof that no child ran: the
/// response may have been lost after the service started. The submission therefore stays accepted
/// and the same task keeps its state and its only input instead of being rolled back; the positive
/// `ExecMainStatus=203` proof is covered by
/// [`a_unit_that_failed_to_exec_is_reported_as_exec_failed`].
#[tokio::test]
async fn an_ambiguous_start_failure_is_unknown_and_keeps_the_task_and_input() {
    let (app, cookie) = setup().await;
    let plan_id = ready_plan_id(&app, &cookie).await;
    app.host.preset(
        "systemd-run",
        &[],
        FakeOutput::code(1).with_stderr("Failed to start transient service unit"),
    );
    let (status, body) = submit_plan(&app, &cookie, &plan_id).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(body["task_id"].is_string(), "{body}");
    assert_eq!(app.host.count("systemd-run"), 1);
    // Unknown: no rollback, no second task, the unique input is kept.
    assert_eq!(app.host.children("/var/lib/nvm/manager/state").len(), 1);
    assert_eq!(app.host.children("/var/lib/nvm/manager/tasks").len(), 1);
}

#[tokio::test]
async fn a_unit_that_failed_to_exec_is_reported_as_exec_failed() {
    let (app, cookie) = setup().await;
    let plan_id = ready_plan_id(&app, &cookie).await;
    app.host.preset(
        "systemd-run",
        &[],
        FakeOutput::code(1).with_stderr("Job failed"),
    );
    app.host.preset(
        "systemctl",
        &["show"],
        FakeOutput::stdout(
            "LoadState=loaded\nActiveState=failed\nSubState=failed\nResult=exit-code\nExecMainStatus=203\n",
        ),
    );
    let (status, body) = submit_plan(&app, &cookie, &plan_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["params"]["reason"]["key"], "submit.exec_failed");
    assert!(app.host.children("/var/lib/nvm/manager/state").is_empty());
    assert!(app.host.children("/var/lib/nvm/manager/tasks").is_empty());
}

#[tokio::test]
async fn an_interrupted_task_does_not_block_a_new_update() {
    // After the interrupted record, planning is ready and submission starts a new unit.
    let (app, cookie) = setup().await;
    let id = "20260928T101530Z-3f9a0c11";
    write_state(&app.host, id, Status::Running, Some(Phase::Write));
    let (_, tasks) = get(&app, "/api/tasks", Some(&cookie)).await;
    assert_eq!(tasks["tasks"][0]["status"], "INTERRUPTED", "{tasks}");
    let (status, plan) = official_plan(&app, &cookie, "580.178.04").await;
    assert_eq!(status, 200, "{plan}");
    assert_eq!(plan["result"], "ready");
    let (status, task) = post_task(&app, &cookie, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(app.host.count("systemd-run"), 1);
    app.host.wait_for_units(std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn a_local_row_whose_file_changed_is_blocked_with_row_checksum_mismatch() {
    let (app, cookie) = setup().await;
    let row_id = write_local_row(&app, "580.95.05").await;
    let path = format!("/var/lib/nvm/drivers/{row_id}/nvidia.raw");
    let mut bytes = app.host.file(&path).unwrap();
    bytes[0] ^= 0x01;
    app.host.put_file(&path, bytes);
    let (status, body) = post_plan(&app, &cookie, json!({"kind": "local", "row_id": row_id})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "blocked");
    assert!(
        body["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["item"] == "local_checksum_mismatch"),
        "{body}"
    );
    assert_eq!(app.host.count("systemd-run"), 0);
}

async fn submit_and_read_plan(app: &TestApp, cookie: &str, gpus: &[&str]) -> serde_json::Value {
    let (status, plan) = plan_gpus(app, cookie, "580.178.04", gpus).await;
    assert_eq!(status, 200, "{plan}");
    assert_eq!(plan["result"], "ready");
    let (status, task) = post_task(app, cookie, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id = task["task_id"].as_str().unwrap().to_string();
    let state: serde_json::Value = serde_json::from_slice(
        &app.host
            .file(&format!("/var/lib/nvm/manager/state/{id}.json"))
            .unwrap(),
    )
    .unwrap();
    app.host.wait_for_units(std::time::Duration::from_secs(5));
    state
}

#[tokio::test]
async fn a_plan_without_selected_gpus_records_no_module_type() {
    let (app, cookie) = setup().await;
    let state = submit_and_read_plan(&app, &cookie, &[]).await;
    assert!(state["plan"]["kernel_module_type"].is_null(), "{state}");
}

#[tokio::test]
async fn confirmation_discloses_the_qualified_community_patch() {
    let (app, cookie) = setup().await;
    let (status, body) = official_plan(&app, &cookie, "470.256.02").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["dialog"]["confirmation"]["warnings"][0],
        json!({
            "key":"confirm.community_patch", "params":{}
        })
    );
}

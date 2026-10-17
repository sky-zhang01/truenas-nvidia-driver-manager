//! The local driver table, deletion and Keep.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::task_state::{OriginalChoice, Phase, Status};

async fn setup() -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "longpass99").await;
    (app, cookie)
}

#[allow(clippy::too_many_arguments)]
fn put_row(
    app: &TestApp,
    id: &str,
    kind: &str,
    version: &str,
    sha: &str,
    target_kernel: &str,
    keep: Option<bool>,
    original_of: Option<(&str, &str)>,
) {
    let bytes = format!("payload of {id}");
    app.host
        .put_text(&format!("/var/lib/nvm/drivers/{id}/nvidia.raw"), &bytes);
    let original = match original_of {
        Some((k, t)) => format!(r#"{{"kernel":"{k}","truenas_version":"{t}"}}"#),
        None => "null".to_string(),
    };
    let keep = match keep {
        Some(k) => k.to_string(),
        None => "null".to_string(),
    };
    let fingerprint = if kind == "upload" {
        format!("\"{}\"", "b".repeat(12))
    } else {
        "null".to_string()
    };
    app.host.put_text(
        &format!("/var/lib/nvm/drivers/{id}/record.json"),
        &format!(
            r#"{{"kind":"{kind}","version":"{version}","sha256":"{sha}","size":{},"target_kernel":"{target_kernel}","created_at":1000,"fingerprint":{fingerprint},"original_of":{original},"keep":{keep}}}"#,
            bytes.len()
        ),
    );
}

#[tokio::test]
async fn driver_list_reports_row_statuses() {
    let (app, cookie) = setup().await;
    let canonical_sha = sha256_hex(b"driver image");
    put_row(
        &app,
        "20260928T101530Z-3f9a0c11-build",
        "official",
        "580.173.02",
        &canonical_sha,
        KERNEL,
        None,
        None,
    );
    put_row(
        &app,
        "20260928T101530Z-3f9a0c12-build",
        "official",
        "580.178.04",
        &"c".repeat(64),
        KERNEL,
        None,
        None,
    );
    put_row(
        &app,
        "20260928T101530Z-3f9a0c13-build",
        "official",
        "580.95.05",
        &"d".repeat(64),
        "6.12.96",
        None,
        None,
    );
    put_row(
        &app,
        "20260928T101530Z-3f9a0c14-original",
        "original",
        "580.173.02",
        &"e".repeat(64),
        KERNEL,
        Some(true),
        Some(("6.12.105-production+truenas", "25.10.7")),
    );
    app.host.put_text(
        "/var/lib/nvm/drivers/20260928T101530Z-3f9a0c15-build/record.json",
        "not json",
    );
    let (status, body) = get(&app, "/api/drivers", Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    let find = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("missing row {id}"))
    };
    assert_eq!(find("20260928T101530Z-3f9a0c11-build")["status"], "in_use");
    assert_eq!(
        find("20260928T101530Z-3f9a0c11-build")["delete_blocked"],
        "in_use"
    );
    assert_eq!(
        find("20260928T101530Z-3f9a0c12-build")["status"],
        "available"
    );
    assert_eq!(find("20260928T101530Z-3f9a0c12-build")["can_update"], true);
    assert_eq!(
        find("20260928T101530Z-3f9a0c13-build")["status"],
        "kernel_mismatch"
    );
    assert_eq!(find("20260928T101530Z-3f9a0c13-build")["can_update"], false);
    assert_eq!(find("20260928T101530Z-3f9a0c14-original")["keep"], true);
    assert_eq!(
        find("20260928T101530Z-3f9a0c14-original")["delete_blocked"],
        "kept"
    );
    assert_eq!(find("20260928T101530Z-3f9a0c15-build")["status"], "UNKNOWN");
    assert_eq!(
        find("20260928T101530Z-3f9a0c15-build")["version"],
        serde_json::Value::Null
    );
    assert_eq!(body["kernel"], KERNEL);
    assert!(body["delete_unavailable"].is_null());
}

#[tokio::test]
async fn driver_deletion_refuses_in_use_and_kept_rows_together() {
    let (app, cookie) = setup().await;
    let canonical_sha = sha256_hex(b"driver image");
    put_row(
        &app,
        "20260928T101530Z-3f9a0c11-build",
        "official",
        "580.173.02",
        &canonical_sha,
        KERNEL,
        None,
        None,
    );
    put_row(
        &app,
        "20260928T101530Z-3f9a0c12-build",
        "official",
        "580.178.04",
        &"c".repeat(64),
        KERNEL,
        None,
        None,
    );
    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": ["20260928T101530Z-3f9a0c11-build"]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "delete.refused");
    assert_eq!(body["params"]["rows"][0]["reason"], "in_use");
    assert!(app
        .host
        .exists("/var/lib/nvm/drivers/20260928T101530Z-3f9a0c11-build/nvidia.raw"));
    let before = app.host.file(CANONICAL_PATH).unwrap();
    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": ["20260928T101530Z-3f9a0c12-build"]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["deleted"][0], "20260928T101530Z-3f9a0c12-build");
    assert!(!app
        .host
        .exists("/var/lib/nvm/drivers/20260928T101530Z-3f9a0c12-build/nvidia.raw"));
    assert_eq!(app.host.file(CANONICAL_PATH).unwrap(), before);
}

#[tokio::test]
async fn kept_original_must_be_released_before_deletion() {
    let (app, cookie) = setup().await;
    put_row(
        &app,
        "20260928T101530Z-3f9a0c14-original",
        "original",
        "580.173.02",
        &"e".repeat(64),
        KERNEL,
        Some(true),
        Some((KERNEL, TRUENAS_VERSION)),
    );
    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": ["20260928T101530Z-3f9a0c14-original"]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["params"]["rows"][0]["reason"], "kept");
    let (status, body) = put(
        &app,
        "/api/drivers/20260928T101530Z-3f9a0c14-original/keep",
        json!({"keep": false}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["keep"], false);
    assert!(app
        .host
        .file_text("/var/lib/nvm/drivers/20260928T101530Z-3f9a0c14-original/nvidia.raw")
        .is_some());
    let (status, _) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": ["20260928T101530Z-3f9a0c14-original"]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn keep_is_only_for_original_rows() {
    let (app, cookie) = setup().await;
    put_row(
        &app,
        "20260928T101530Z-3f9a0c12-build",
        "official",
        "580.178.04",
        &"c".repeat(64),
        KERNEL,
        None,
        None,
    );
    let (status, body) = put(
        &app,
        "/api/drivers/20260928T101530Z-3f9a0c12-build/keep",
        json!({"keep": true}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "keep.not_original");
    let (status, body) = put(
        &app,
        "/api/drivers/20260928T101530Z-3f9a0c13-build/keep",
        json!({"keep": true}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "delete.row_missing");
}

#[tokio::test]
async fn deletion_is_blocked_while_a_task_runs() {
    let (app, cookie) = setup().await;
    put_row(
        &app,
        "20260928T101530Z-3f9a0c12-build",
        "official",
        "580.178.04",
        &"c".repeat(64),
        KERNEL,
        None,
        None,
    );
    let gate = app.host.set_gate("curl", &["--silent"]);
    let id = "20260928T101530Z-3f9a0c12";
    let mut state = write_state(&app.host, id, Status::Running, Some(Phase::Download));
    state.plan.as_mut().unwrap().original = OriginalChoice::Skip;
    truenas_nvidia_driver_manager::task_state::write(
        &app.host,
        &truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR),
        &state,
    )
    .unwrap();
    let _ = truenas_nvidia_driver_manager::tasks::start(
        &app.host,
        std::path::Path::new(HOST_DATA_DIR),
        id,
    );
    for _ in 0..50 {
        if app.host.unit_ids().contains(&id.to_string()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (status, body) = get(&app, "/api/drivers", Some(&cookie)).await;
    assert_eq!(status, 200);
    assert_eq!(body["delete_unavailable"]["key"], "task_running");
    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": ["20260928T101530Z-3f9a0c12-build"]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "delete.task_running");
    gate.release();
    app.host.wait_for_units(std::time::Duration::from_secs(5));
    // Keep update is allowed while a task runs: it only changes the record.
    let _ = put(
        &app,
        "/api/drivers/20260928T101530Z-3f9a0c12-build/keep",
        json!({"keep": false}),
        Some(&cookie),
    )
    .await;
}

#[tokio::test]
async fn unreadable_canonical_blocks_deletion() {
    let (app, cookie) = setup().await;
    put_row(
        &app,
        "20260928T101530Z-3f9a0c12-build",
        "official",
        "580.178.04",
        &"c".repeat(64),
        KERNEL,
        None,
        None,
    );
    app.host.remove(CANONICAL_PATH);
    let (_status, body) = get(&app, "/api/drivers", Some(&cookie)).await;
    assert_eq!(body["delete_unavailable"]["key"], "canonical_unreadable");
    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": ["20260928T101530Z-3f9a0c12-build"]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "delete.canonical_unreadable");
    // Rows whose record cannot be read are UNKNOWN and cannot be deleted.
    app.host.put_text(
        "/var/lib/nvm/drivers/20260928T101530Z-3f9a0c13-build/record.json",
        "nope",
    );
    let (_, body) = get(&app, "/api/drivers", Some(&cookie)).await;
    let unknown = body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "20260928T101530Z-3f9a0c13-build")
        .unwrap();
    assert_eq!(unknown["status"], "UNKNOWN");
    assert_eq!(unknown["delete_blocked"], "unknown");
}

#[tokio::test]
async fn driver_list_can_update_survives_an_unreadable_canonical() {
    // A permanent-path checksum that cannot be read makes the row UNKNOWN but leaves "update to
    // this version" available.
    let (app, cookie) = setup().await;
    put_row(
        &app,
        "20260928T101530Z-3f9a0c12-build",
        "official",
        "580.178.04",
        &"c".repeat(64),
        KERNEL,
        None,
        None,
    );
    app.host.remove(CANONICAL_PATH);
    let (status, body) = get(&app, "/api/drivers", Some(&cookie)).await;
    assert_eq!(status, 200);
    let row = body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "20260928T101530Z-3f9a0c12-build")
        .unwrap()
        .clone();
    assert_eq!(row["status"], "UNKNOWN");
    assert_eq!(row["can_update"], true);
    assert_eq!(row["delete_blocked"], serde_json::Value::Null);
}

#[tokio::test]
async fn driver_keep_refuses_a_kind_suffix_mismatch() {
    // A record whose kind does not match the row id suffix is unreadable; the keep switch must not
    // rewrite it.
    let (app, cookie) = setup().await;
    let id = "20260928T101530Z-3f9a0c12-build";
    put_row(
        &app,
        id,
        "original",
        "580.178.04",
        &"c".repeat(64),
        KERNEL,
        Some(true),
        Some((KERNEL, TRUENAS_VERSION)),
    );
    let record_path = format!("/var/lib/nvm/drivers/{id}/record.json");
    let before = app.host.file_text(&record_path).unwrap();
    let (status, body) = put(
        &app,
        &format!("/api/drivers/{id}/keep"),
        json!({"keep": false}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "keep.row_unreadable");
    assert_eq!(app.host.file_text(&record_path).unwrap(), before);
}

#[tokio::test]
async fn driver_deletion_is_refused_when_pid_host_failed() {
    // No reconciliation at all when `pid: host` failed, so deletion cannot be decided and must be
    // refused.
    use truenas_nvidia_driver_manager::startup::{Startup, StartupProblem};
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let startup = Startup {
        problems: vec![StartupProblem::PidHost],
        pid_host_ok: false,
        host_data_dir: Some(std::path::PathBuf::from(HOST_DATA_DIR)),
        image_id: Some(format!("sha256:{}", "a".repeat(64))),
    };
    let app = app(host.clone(), settings("hunter12"), startup);
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "longpass99").await;
    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": ["20260928T101530Z-3f9a0c12-build"]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "delete.task_state_unknown");
    assert_eq!(body["params"]["reason"]["key"], REASON_STARTUP_CHECK_FAILED);
}

fn all_paths(app: &TestApp) -> Vec<String> {
    fn walk(app: &TestApp, dir: &str, out: &mut Vec<String>) {
        for name in app.host.children(dir) {
            let path = format!("{dir}/{name}");
            out.push(path.clone());
            walk(app, &path, out);
        }
    }
    let mut out = Vec::new();
    walk(app, "/var/lib/nvm", &mut out);
    out.sort();
    out
}

#[tokio::test]
async fn deleting_rows_removes_exactly_the_selected_directories() {
    let (app, cookie) = setup().await;
    put_row(
        &app,
        "20260928T101530Z-3f9a0c11-build",
        "official",
        "580.95.05",
        &"a".repeat(64),
        KERNEL,
        None,
        None,
    );
    put_row(
        &app,
        "20260928T101530Z-3f9a0c12-build",
        "official",
        "580.178.04",
        &"c".repeat(64),
        KERNEL,
        None,
        None,
    );
    let canonical_before = app.host.file(CANONICAL_PATH).unwrap();
    let before = all_paths(&app);
    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": ["20260928T101530Z-3f9a0c11-build"]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let after = all_paths(&app);
    let removed: Vec<String> = before
        .iter()
        .filter(|p| !after.contains(p))
        .cloned()
        .collect();
    assert!(!removed.is_empty());
    for path in &removed {
        assert!(
            path.contains("20260928T101530Z-3f9a0c11-build"),
            "unexpected removal: {path}"
        );
    }
    assert!(removed
        .iter()
        .any(|p| p.ends_with("20260928T101530Z-3f9a0c11-build/nvidia.raw")));
    assert_eq!(app.host.file(CANONICAL_PATH).unwrap(), canonical_before);
}

#[tokio::test]
async fn releasing_keep_never_touches_the_original_file() {
    let (app, cookie) = setup().await;
    let id = "20260928T101530Z-3f9a0c14-original";
    put_row(
        &app,
        id,
        "original",
        "580.173.02",
        &"e".repeat(64),
        KERNEL,
        Some(true),
        Some((KERNEL, TRUENAS_VERSION)),
    );
    let raw = format!("/var/lib/nvm/drivers/{id}/nvidia.raw");
    let sha_before = sha256_hex(&app.host.file(&raw).unwrap());
    let ops_before = app.host.ops_for(&raw);
    let (status, _) = put(
        &app,
        &format!("/api/drivers/{id}/keep"),
        json!({"keep": false}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(sha256_hex(&app.host.file(&raw).unwrap()), sha_before);
    // The switch only rewrites the record: the file itself is never opened, read, written or
    // renamed (an `lstat` is not an open).
    let after = app.host.ops_for(&raw);
    for forbidden in [
        "open",
        "read",
        "create_new",
        "create_truncate",
        "rename",
        "append",
    ] {
        assert!(
            !after.contains(&forbidden),
            "{forbidden} on the original: {after:?} vs {ops_before:?}"
        );
    }
}

#[tokio::test]
async fn a_local_source_with_a_kernel_mismatch_is_blocked() {
    let (app, cookie) = setup().await;
    let id = "20260928T101530Z-3f9a0c12-build";
    put_row(
        &app,
        id,
        "official",
        "580.178.04",
        &sha256_hex(b"payload of 20260928T101530Z-3f9a0c12-build"),
        "6.12.96",
        None,
        None,
    );
    let (status, body) = post_plan(&app, &cookie, json!({"kind": "local", "row_id": id})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"], "blocked");
    let failure = body["failures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["item"] == "local_kernel_mismatch")
        .expect("kernel mismatch failure");
    assert_eq!(failure["target_kernel"], "6.12.96");
    assert_eq!(failure["current_kernel"], KERNEL);
    assert_eq!(failure["row_kind"], "official");
    assert_eq!(app.host.count("systemd-run"), 0);
}

#[tokio::test]
async fn a_new_kernel_and_truenas_pair_starts_without_an_original() {
    // The original belongs to the (kernel, TrueNAS version) pair.
    let (app, cookie) = setup().await;
    put_row(
        &app,
        "20260928T101530Z-3f9a0c14-original",
        "original",
        "580.173.02",
        &"e".repeat(64),
        KERNEL,
        Some(true),
        Some((KERNEL, TRUENAS_VERSION)),
    );
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dialog"]["original"]["saved"], true);
    app.host.set_uname("6.12.96-production+truenas");
    app.host
        .put_text("/data/manifest.json", r#"{"version":"25.10.8"}"#);
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dialog"]["original"]["saved"], false);
    assert_eq!(body["dialog"]["truenas_version"], "25.10.8");
    assert_eq!(body["dialog"]["kernel"], "6.12.96-production+truenas");
}

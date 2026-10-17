//! State read errors use five plain keys consistently in task lists, task details, and home. Other
//! reason fields use `reason.*` phrases.

mod common;

use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::task_state::{ErrorRecord, Phase, Status};

async fn setup() -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    signed_in_app(host).await
}

fn state_path(id: &str) -> String {
    format!("{CONTAINER_DATA_DIR}/manager/state/{id}.json")
}

fn task_item<'a>(list: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    list["tasks"]
        .as_array()
        .expect("task list carries a tasks array")
        .iter()
        .find(|t| t["task_id"] == id)
        .unwrap_or_else(|| panic!("task {id} is missing from task list: {list}"))
}

/// One running unit whose state file produces `case`'s read result.
async fn running_unreadable(case: &str) -> (TestApp, String, String) {
    let id = "20260928T101530Z-3f9a0c12";
    let (app, cookie) = setup().await;
    write_state(&app.host, id, Status::Running, Some(Phase::Write));
    app.host.set_unit(id, "active", "running", "success", 0);
    let path = state_path(id);
    match case {
        "missing" => app.host.remove(&path),
        "incomplete" => app.host.put_text(&path, "{\"task_id\":"),
        "invalid" => app.host.put_text(&path, "not json"),
        "permission_denied" => app.host.fail_read(&path),
        // A directory where the state file belongs reads back as EISDIR.
        "io_error" => {
            app.host.remove(&path);
            app.host.mkdir(&path);
        }
        other => unreachable!("unknown case {other}"),
    }
    (app, cookie, id.to_string())
}

#[tokio::test]
async fn the_state_read_error_closed_set_reaches_task_list_task_detail_and_home() {
    for (case, key) in [
        ("missing", "missing"),
        ("incomplete", "incomplete"),
        ("invalid", "invalid"),
        ("permission_denied", "permission_denied"),
        ("io_error", "io_error"),
    ] {
        let (app, cookie, id) = running_unreadable(case).await;
        let (status, detail) = get(&app, &format!("/api/tasks/{id}"), Some(&cookie)).await;
        assert_eq!(status, 200, "{case}: {detail}");
        assert_eq!(detail["status"], "UNKNOWN", "{case}");
        assert_eq!(
            detail["state_read_error"], key,
            "task detail {case}: {detail}"
        );

        let (status, list) = get(&app, "/api/tasks", Some(&cookie)).await;
        assert_eq!(status, 200, "{case}: {list}");
        let item = task_item(&list, &id);
        assert_eq!(item["status"], "UNKNOWN", "task list {case}: {list}");
        assert_eq!(item["state_read_error"], key, "task list {case}: {list}");

        let (status, home) = get(&app, "/api/home", Some(&cookie)).await;
        assert_eq!(status, 200, "{case}: {home}");
        assert_eq!(home["active_task"]["task_id"], id, "home {case}: {home}");
        assert_eq!(
            home["active_task"]["status"], "UNKNOWN",
            "home {case}: {home}"
        );
        assert_eq!(
            home["active_task"]["state_read_error"], key,
            "home {case}: {home}"
        );
        app.host.assert_red_lines();
    }
}

#[tokio::test]
async fn a_succeeded_task_missing_checks_keeps_its_bytes_and_reports_an_invalid_reason() {
    // A SUCCEEDED state missing its observations is invalid. The original bytes are kept and the
    // rewritten note uses the plain key.
    let id = "20260928T101530Z-3f9a0c12";
    let (app, cookie) = setup().await;
    let mut state = write_state(
        &app.host,
        id,
        Status::Succeeded,
        Some(Phase::ReadLoadedVersionEnd),
    );
    state.checks.clear();
    let bytes = serde_json::to_vec_pretty(&state).unwrap();
    let path = state_path(id);
    app.host.put_file(&path, bytes.clone());

    let (status, body) = get(&app, &format!("/api/tasks/{id}"), Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "INTERRUPTED", "{body}");
    assert_eq!(body["state_read_error"], "invalid", "{body}");
    assert_eq!(body["state_note"]["state_read_error"], "invalid", "{body}");
    assert_eq!(
        body["state_note"]["kept_as"],
        format!("{id}.json.unreadable"),
        "{body}"
    );
    assert_eq!(
        app.host.file(&format!("{path}.unreadable")),
        Some(bytes),
        "the original bytes must be kept byte for byte"
    );
    app.host.assert_red_lines();
}

#[tokio::test]
async fn an_io_error_on_a_stopped_task_writes_a_plain_note() {
    // The INTERRUPTED record rewritten by reconciliation must itself be a legal state file that
    // reads back cleanly on the next request.
    let id = "20260928T101530Z-3f9a0c12";
    let (app, cookie) = setup().await;
    write_state(&app.host, id, Status::Running, Some(Phase::Write));
    let path = state_path(id);
    app.host.remove(&path);
    app.host.mkdir(&path);

    let (status, body) = get(&app, &format!("/api/tasks/{id}"), Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "INTERRUPTED", "{body}");
    assert_eq!(body["state_read_error"], "io_error", "{body}");
    assert_eq!(body["state_note"]["state_read_error"], "io_error", "{body}");
    assert_eq!(
        body["state_note"]["kept_as"],
        format!("{id}.json.unreadable"),
        "{body}"
    );
    assert!(app.host.exists(&format!("{path}.unreadable")), "{body}");

    let (status, again) = get(&app, &format!("/api/tasks/{id}"), Some(&cookie)).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["status"], "INTERRUPTED", "{again}");
    assert!(
        again["state_read_error"].is_null(),
        "a legal record needs no state read error: {again}"
    );
    app.host.assert_red_lines();
}

#[tokio::test]
async fn a_failed_state_survives_an_exit_zero_unit() {
    // Task status comes from the state file: a unit that exited 0 over a FAILED file still shows
    // FAILED.
    let id = "20260928T101530Z-3f9a0c12";
    let (app, cookie) = setup().await;
    write_state(&app.host, id, Status::Failed, Some(Phase::Check));
    app.host.set_unit(id, "inactive", "dead", "success", 0);

    let (status, body) = get(&app, &format!("/api/tasks/{id}"), Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "FAILED", "{body}");
    assert!(body["state_read_error"].is_null(), "{body}");
    app.host.assert_red_lines();
}

#[tokio::test]
async fn the_generic_reason_phrases_stay_outside_state_read_error() {
    // The other reason fields keep their `reason.*` phrases while the state read error key is
    // plain.
    let id = "20260928T101530Z-3f9a0c12";
    let (app, cookie) = setup().await;
    app.host.set_unit(id, "active", "running", "success", 0);
    app.host
        .remove(&format!("{CONTAINER_DATA_DIR}/manager/state"));

    let (status, list) = get(&app, "/api/tasks", Some(&cookie)).await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(
        list["state_dir"]["reason"]["key"], REASON_IO_ERROR,
        "{list}"
    );
    assert_eq!(
        task_item(&list, id)["state_read_error"],
        "missing",
        "{list}"
    );
    app.host.assert_red_lines();
}

#[tokio::test]
async fn an_unknown_reason_code_keeps_the_record() {
    // A record written by a newer build may carry reason codes and parameters this build does not
    // know: reading it must keep the record exactly as it is, instead of moving it aside and
    // rewriting an INTERRUPTED stub over a task that actually FAILED.
    let id = "20260928T101530Z-3f9a0c12";
    let (app, cookie) = setup().await;
    let mut state = write_state(&app.host, id, Status::Failed, Some(Phase::Check));
    state.finished_at = Some(2);
    state.error = Some(ErrorRecord {
        code: "error.brand_new_failure".to_string(),
        params: json!({"reason": {"key": "build.brand_new", "params": {"x": "y"}}}),
        command: None,
        exit: None,
        raw: None,
    });
    let bytes = serde_json::to_vec_pretty(&state).unwrap();
    let path = state_path(id);
    app.host.put_file(&path, bytes.clone());

    let (status, body) = get(&app, &format!("/api/tasks/{id}"), Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "FAILED", "{body}");
    assert!(body["state_read_error"].is_null(), "{body}");
    assert_eq!(
        app.host.file(&path),
        Some(bytes),
        "the record must stay byte for byte"
    );
    app.host.assert_red_lines();
}

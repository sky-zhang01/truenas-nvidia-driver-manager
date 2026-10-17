//! Reconciliation and cleanup.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::host::{At, Cmd, Host};
use truenas_nvidia_driver_manager::store::{JsonRead, Paths};
use truenas_nvidia_driver_manager::task_state::{self, Pass, Phase, ReplaceRecord, Status};

fn paths() -> Paths {
    Paths::new(CONTAINER_DATA_DIR)
}

fn state_text(host: &FakeHost, id: &str) -> Option<String> {
    host.file_text(&format!("/var/lib/nvm/manager/state/{id}.json"))
}

#[test]
fn a_running_task_without_a_unit_becomes_interrupted() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    write_state(&host, id, Status::Running, Some(Phase::Write));
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert_eq!(reconciled.tasks.len(), 1);
    assert_eq!(reconciled.tasks[0].status, Some(Status::Interrupted));
    assert_eq!(reconciled.tasks[0].phase, Some(Phase::Write));
    let text = state_text(&host, id).unwrap();
    assert!(text.contains("\"INTERRUPTED\""));
    assert!(text.contains("\"write\""));
    assert!(matches!(
        reconciled.active,
        truenas_nvidia_driver_manager::tasks::ActiveState::None
    ));
}

#[test]
fn a_failure_killed_after_entering_stage_fifteen_is_interrupted() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    let mut ctx = truenas_nvidia_driver_manager::runner::RunCtx {
        host: std::sync::Arc::new(host.clone()),
        paths: Paths::new(HOST_DATA_DIR),
        id: id.to_string(),
        state: write_state(&host, id, Status::Running, Some(Phase::Build)),
        build_deadline: None,
    };
    ctx.fail(ErrorCode::BuildTimeout, serde_json::json!({}), None);
    ctx.enter(Phase::ReadLoadedVersionEnd);
    drop(ctx); // The runner is killed before its terminal write.

    let pending: serde_json::Value = serde_json::from_str(&state_text(&host, id).unwrap()).unwrap();
    assert_eq!(pending["status"], "RUNNING", "{pending}");
    assert_eq!(pending["phase"], "read_loaded_version_end");
    assert_eq!(pending["error"]["code"], "BUILD_TIMEOUT");
    assert!(pending["finished_at"].is_null());

    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    let task = &reconciled.tasks[0];
    assert_eq!(task.status, Some(Status::Interrupted));
    assert_eq!(task.phase, Some(Phase::ReadLoadedVersionEnd));
    let result: serde_json::Value = serde_json::from_str(&state_text(&host, id).unwrap()).unwrap();
    assert_eq!(result["status"], "INTERRUPTED", "{result}");
    assert_eq!(result["error"]["code"], "BUILD_TIMEOUT");
}

#[test]
fn a_terminal_state_is_kept() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    write_state(&host, id, Status::Failed, Some(Phase::Check));
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert_eq!(reconciled.tasks[0].status, Some(Status::Failed));
    assert!(state_text(&host, id).unwrap().contains("\"FAILED\""));
}

#[test]
fn an_unreadable_state_file_is_kept_aside_with_a_note() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    host.put_text(&format!("/var/lib/nvm/manager/state/{id}.json"), "not json");
    let before = host
        .file_text(&format!("/var/lib/nvm/manager/state/{id}.json"))
        .unwrap();
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert!(host.exists(&format!("/var/lib/nvm/manager/state/{id}.json.unreadable")));
    assert_eq!(
        host.file_text(&format!("/var/lib/nvm/manager/state/{id}.json.unreadable"))
            .unwrap(),
        before
    );
    let task = &reconciled.tasks[0];
    assert_eq!(task.status, Some(Status::Interrupted));
    assert_eq!(task.state_read_error.as_deref(), Some("invalid"));
    assert_eq!(
        task.state_note.as_ref().unwrap().kept_as.as_deref(),
        Some(format!("{id}.json.unreadable").as_str())
    );
}

#[test]
fn a_missing_state_file_with_a_task_dir_gets_a_note() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    host.mkdir(&format!("/var/lib/nvm/manager/tasks/{id}/in"));
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    let task = &reconciled.tasks[0];
    assert_eq!(task.status, Some(Status::Interrupted));
    assert_eq!(task.state_read_error.as_deref(), Some("missing"));
    assert_eq!(
        task.state_note.as_ref().unwrap().state_read_error,
        "missing".to_string()
    );
}

#[test]
fn a_running_unit_keeps_its_running_state() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    let gate = host.set_gate("curl", &["--silent"]);
    let mut state = write_state(&host, id, Status::Running, Some(Phase::Download));
    state.plan.as_mut().unwrap().original =
        truenas_nvidia_driver_manager::task_state::OriginalChoice::Skip;
    truenas_nvidia_driver_manager::task_state::write(&host, &paths(), &state).unwrap();
    let started =
        truenas_nvidia_driver_manager::tasks::start(&host, std::path::Path::new(HOST_DATA_DIR), id);
    assert!(matches!(
        started,
        truenas_nvidia_driver_manager::tasks::StartResult::Started
    ));
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert_eq!(reconciled.tasks[0].status, Some(Status::Running));
    assert!(matches!(
        reconciled.active,
        truenas_nvidia_driver_manager::tasks::ActiveState::Running { .. }
    ));
    gate.release();
    host.wait_for_units(std::time::Duration::from_secs(5));
}

#[test]
fn a_killed_runner_is_interrupted_and_its_leftovers_cleaned() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    write_state(&host, id, Status::Running, Some(Phase::Write));
    // `set_unit` names the task id; the fixture adds the unit name itself.
    host.set_unit(id, "failed", "failed", "signal", 0);
    host.mkdir(&format!("/var/lib/nvm/manager/tasks/{id}/out"));
    host.put_text(
        &format!("/var/lib/nvm/manager/state/.{id}.json.tmp-0123456789abcdef"),
        "tmp",
    );
    host.mkdir(RW_MOUNT);
    host.add_mount(
        DATASET,
        "/run/truenas-nvidia-driver-manager/usr-rw",
        "zfs",
        "rw",
    );
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert_eq!(reconciled.tasks[0].status, Some(Status::Interrupted));
    assert!(!host.exists(&format!("/var/lib/nvm/manager/tasks/{id}")));
    assert!(!host.exists(&format!(
        "/var/lib/nvm/manager/state/.{id}.json.tmp-0123456789abcdef"
    )));
    assert!(host
        .argv_log()
        .iter()
        .any(|cmd| cmd.iter().any(|arg| arg == "umount")));
    assert_eq!(host.readonly(), "on");
    assert!(reconciled.tasks[0].state.as_ref().unwrap().rootfs.is_none());
}

#[test]
fn unit_query_failure_keeps_files_and_reports_unreadable() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    write_state(&host, id, Status::Running, Some(Phase::Download));
    host.preset(
        "systemctl",
        &["list-units"],
        FakeOutput::code(1).with_stderr("systemctl: failed"),
    );
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert!(reconciled.unit_query_error.is_some());
    assert_eq!(reconciled.tasks[0].status, Some(Status::Running));
    assert!(matches!(
        reconciled.active,
        truenas_nvidia_driver_manager::tasks::ActiveState::Running { .. }
    ));
    assert!(state_text(&host, id).unwrap().contains("\"RUNNING\""));
}

#[test]
fn state_directory_failure_is_unreadable() {
    let host = default_host();
    host.remove("/var/lib/nvm/manager/state");
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert!(matches!(
        reconciled.active,
        truenas_nvidia_driver_manager::tasks::ActiveState::Unreadable { .. }
    ));
}

fn running_with_rootfs(host: &FakeHost, id: &str, phase: Phase) {
    use truenas_nvidia_driver_manager::task_state::Rootfs;
    let mut state = write_state(host, id, Status::Running, Some(phase));
    state.rootfs = Some(Rootfs {
        dataset: USR_DATASET.into(),
        readonly_before: "on".into(),
    });
    truenas_nvidia_driver_manager::task_state::write(host, &paths(), &state).unwrap();
}

#[test]
fn a_host_reboot_interrupts_the_task_and_restores_the_readonly_property() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    running_with_rootfs(&host, id, Phase::Write);
    host.mkdir(&format!("/var/lib/nvm/manager/tasks/{id}/out"));
    host.set_unit(id, "active", "running", "success", 0);
    // The host came back: no units, /run cleared, the boot mount table.
    host.reboot_host();
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert_eq!(reconciled.tasks[0].status, Some(Status::Interrupted));
    assert_eq!(reconciled.tasks[0].phase, Some(Phase::Write));
    assert_eq!(host.readonly(), "on");
    assert_eq!(host.count("systemd-run"), 0);
}

#[test]
fn the_cleanup_reads_the_host_mount_table_not_the_container_one() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    running_with_rootfs(&host, id, Phase::Write);
    host.mkdir(&format!("/var/lib/nvm/manager/tasks/{id}/out"));
    host.mkdir("/run/truenas-nvidia-driver-manager/usr-rw");
    host.put_text(
        "/run/truenas-nvidia-driver-manager/usr-rw/nvidia.raw.tmp",
        "x",
    );
    // The mount exists only in the App container's own table: the host table does not show it, so
    // cleanup must not touch or unmount it.
    host.add_container_mount(
        "tank/ROOT/env/usr",
        "/run/truenas-nvidia-driver-manager/usr-rw",
        "zfs",
        "rw",
    );
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert_eq!(reconciled.tasks[0].status, Some(Status::Interrupted));
    assert!(host.exists("/run/truenas-nvidia-driver-manager/usr-rw/nvidia.raw.tmp"));
    assert!(!host.argv_log().iter().any(|a| a[0] == "umount"));
    assert_eq!(host.readonly(), "on");

    let host = default_host();
    running_with_rootfs(&host, id, Phase::Write);
    host.mkdir(&format!("/var/lib/nvm/manager/tasks/{id}/out"));
    host.put_text(
        "/run/truenas-nvidia-driver-manager/usr-rw/nvidia.raw.tmp",
        "x",
    );
    host.add_mount(
        "tank/ROOT/env/usr",
        "/run/truenas-nvidia-driver-manager/usr-rw",
        "zfs",
        "rw",
    );
    let _ = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert!(!host.exists("/run/truenas-nvidia-driver-manager/usr-rw/nvidia.raw.tmp"));
    assert!(host.argv_log().iter().any(|a| a[0] == "umount"));
}

#[test]
fn cleanup_preserves_a_nested_mount_after_unmounting_the_writable_view() {
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    let nested = format!("{RW_MOUNT}/foreign");
    let foreign_file = format!("{nested}/keep");
    host.put_text(&foreign_file, "foreign data");
    host.add_mount(DATASET, RW_MOUNT, "zfs", "rw");
    host.add_mount("foreign-dataset", &nested, "zfs", "rw");
    host.mkdir(&format!("/var/lib/nvm/manager/tasks/{id}/out"));

    let results = truenas_nvidia_driver_manager::tasks::cleanup(&host, &paths(), id, None);
    assert!(results
        .iter()
        .any(|step| step.step == CleanupStep::UnmountWritableMount && !step.ok));
    assert!(host.exists(&foreign_file));
    assert!(host.exists(&format!("/var/lib/nvm/manager/tasks/{id}")));
    assert!(!host.ops_for(RW_MOUNT).contains(&"remove_dir_all"));
}

#[test]
fn a_leftover_task_directory_survives_while_another_unit_runs() {
    let host = default_host();
    let old = "20260928T101530Z-3f9a0c11";
    let running = "20260928T101530Z-3f9a0c12";
    // A finished task whose directory could not be cleaned earlier.
    write_state(
        &host,
        old,
        Status::Succeeded,
        Some(Phase::ReadLoadedVersionEnd),
    );
    host.mkdir(&format!("/var/lib/nvm/manager/tasks/{old}"));
    // A unit that is still running, stopped at a gate.
    let gate = host.set_gate("curl", &["--silent"]);
    let mut state = write_state(&host, running, Status::Running, Some(Phase::Download));
    state.plan.as_mut().unwrap().original =
        truenas_nvidia_driver_manager::task_state::OriginalChoice::Skip;
    truenas_nvidia_driver_manager::task_state::write(&host, &paths(), &state).unwrap();
    host.mkdir(&format!("/var/lib/nvm/manager/tasks/{running}/out"));
    let started = truenas_nvidia_driver_manager::tasks::start(
        &host,
        std::path::Path::new(HOST_DATA_DIR),
        running,
    );
    assert!(matches!(
        started,
        truenas_nvidia_driver_manager::tasks::StartResult::Started
    ));
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert!(matches!(
        reconciled.active,
        truenas_nvidia_driver_manager::tasks::ActiveState::Running { .. }
    ));
    // No cleanup ran at all: the old directory and the running task's leftovers are both intact.
    assert!(host.exists(&format!("/var/lib/nvm/manager/tasks/{old}")));
    assert!(host.exists(&format!("/var/lib/nvm/manager/tasks/{running}/out")));
    gate.release();
    host.wait_for_units(std::time::Duration::from_secs(5));
}

#[test]
fn a_succeeded_state_without_observations_is_kept_aside_as_interrupted() {
    // A SUCCEEDED file without the two observations is illegal; the original is kept and the task
    // becomes INTERRUPTED with state read error.
    let host = default_host();
    let id = "20260928T101530Z-3f9a0c12";
    let mut state = write_state(
        &host,
        id,
        Status::Succeeded,
        Some(Phase::ReadLoadedVersionEnd),
    );
    state.checks.clear();
    // Write it directly: `write_state` would be legal, this file is the one a broken runner could
    // have left.
    let bytes = serde_json::to_vec_pretty(&state).unwrap();
    host.put_file(
        &format!("/var/lib/nvm/manager/state/{id}.json"),
        bytes.clone(),
    );
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    let task = &reconciled.tasks[0];
    assert_eq!(task.status, Some(Status::Interrupted));
    assert_eq!(task.state_read_error.as_deref(), Some("invalid"));
    assert_eq!(
        task.state_note.as_ref().unwrap().kept_as.as_deref(),
        Some(format!("{id}.json.unreadable").as_str())
    );
    assert_eq!(
        host.file(&format!("/var/lib/nvm/manager/state/{id}.json.unreadable"))
            .map(|kept| kept == bytes),
        Some(true)
    );
    let now = task.state.as_ref().unwrap();
    assert_eq!(now.status, Status::Interrupted);
    assert!(now.checks.is_empty());
}

/// Unexplained state material stops the managed reconciliation: the bytes are the only evidence, so
/// the task directory of that id must survive, the collection is incomplete and the active answer
/// is undetermined instead of "no task". The real entry is used.
#[test]
fn unexplained_state_material_blocks_the_managed_reconcile_cleanup() {
    let host = default_host();
    // A readable, idle inventory: the missing cleanup here can only come from the unexplained
    // material, never from an unanswerable query.
    host.preset("systemctl", &[], FakeOutput::ok());
    host.preset("docker", &[], FakeOutput::ok());
    let id = "20260928T101530Z-3f9a0c12";
    let state_dir = format!("{HOST_DATA_DIR}/manager/state");
    let task_dir = format!("{HOST_DATA_DIR}/manager/tasks/{id}");
    host.mkdir(&state_dir);
    host.mkdir(&task_dir);
    let unknown = format!("{state_dir}/.{id}.json.tmp-not-a-recognized-suffix");
    host.put_text(&unknown, "unknown original bytes");

    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile_managed(&host, &paths());
    assert!(
        host.exists(&unknown),
        "the unknown bytes survive: {reconciled:#?}"
    );
    assert_eq!(
        host.file_text(&unknown).as_deref(),
        Some("unknown original bytes")
    );
    assert!(
        host.exists(&task_dir),
        "the task directory of the unexplained id is not deleted: {reconciled:#?}"
    );
    assert!(
        !reconciled.collection_complete,
        "an unexplained state entry makes the collection incomplete: {reconciled:#?}"
    );
    assert!(
        matches!(
            reconciled.active,
            truenas_nvidia_driver_manager::tasks::ActiveState::Unreadable { .. }
        ),
        "the answer is undetermined, not 'no task': {reconciled:#?}"
    );
}

/// Positive control for the case above: with the same shape but only a **recognized** temporary,
/// the managed reconciliation still cleans it up and reports a complete collection.
#[test]
fn a_clean_state_directory_still_allows_the_managed_cleanup() {
    use truenas_nvidia_driver_manager::task_state::{Phase, Status};
    let host = default_host();
    host.preset("systemctl", &[], FakeOutput::ok());
    host.preset("docker", &[], FakeOutput::ok());
    let id = "20260928T101530Z-3f9a0c12";
    let state_dir = format!("{HOST_DATA_DIR}/manager/state");
    let task_dir = format!("{HOST_DATA_DIR}/manager/tasks/{id}");
    host.mkdir(&state_dir);
    host.mkdir(&task_dir);
    let mut settled = write_state(
        &host,
        id,
        Status::Interrupted,
        Some(Phase::ReadLoadedVersion),
    );
    settled.status = Status::Interrupted;
    truenas_nvidia_driver_manager::task_state::write(&host, &paths(), &settled)
        .expect("the record is persisted");
    let temp = format!("{state_dir}/.{id}.json.tmp-0123456789abcdef");
    host.put_text(&temp, "a recognized temporary");

    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile_managed(&host, &paths());
    assert!(
        !host.exists(&temp),
        "a recognized temporary is removed by the managed path: {reconciled:#?}"
    );
    assert!(
        reconciled.collection_complete,
        "nothing makes the collection incomplete here: {reconciled:#?}"
    );
    assert!(
        !matches!(
            reconciled.active,
            truenas_nvidia_driver_manager::tasks::ActiveState::Unreadable { .. }
        ),
        "a clean reading is not undetermined: {reconciled:#?}"
    );
}

// ---- the host reset of an unsettled record ---------------------------

const ID: &str = "20261005T101530Z-abcdef12";
/// A dataset name the record validation accepts and the fixture models. The placeholder `DATASET`
/// carries angle brackets and is not a valid record.
const USR_DATASET: &str = "tank/ROOT/env/usr";
const INDEX: &str = r#"<a href="580.178.04/">580.178.04</a>
"#;

/// Make the modelled property really read back `value`, as it does after a write phase set it. The
/// model returns a non-zero exit code on purpose (the property is applied even when the remount
/// cannot follow), so only the value is checked.
fn set_readonly_model(host: &FakeHost, value: &str) {
    let _ = host.run(&Cmd::new(At::Host, "zfs", HOST_CMD_TIMEOUT).args([
        "set",
        &format!("readonly={value}"),
        USR_DATASET,
    ]));
    assert_eq!(host.readonly(), value);
    host.clear_presets_and_commands();
}

/// The record a runner leaves behind when it is killed inside its write phase: the rootfs recovery
/// information and the unloaded modules are on disk, the writable mount is still there and the
/// property is already `off`.
fn killed_in_write_phase(host: &FakeHost) {
    running_with_rootfs(host, ID, Phase::Write);
    let mut state = match task_state::read(host, &paths(), ID) {
        JsonRead::Ok(state) => state,
        other => panic!("the fixture record must be readable: {other:?}"),
    };
    state.replace.push(ReplaceRecord {
        pass: Pass::Update,
        unloaded: vec!["nvidia_uvm".to_string()],
        reloaded: Vec::new(),
        refresh_exit: None,
        ldconfig_exit: None,
        readback_ok: None,
        readonly_after: None,
    });
    task_state::write(host, &paths(), &state).unwrap();
    host.mkdir(paths().task_dir(ID).to_str().unwrap());
    host.add_mount(USR_DATASET, RW_MOUNT, "zfs", "rw");
    host.put_text(RW_TMP_PATH, "half-written");
    set_readonly_model(host, "off");
}

fn kept_record(host: &FakeHost) -> serde_json::Value {
    serde_json::from_str(&state_text(host, ID).expect("the record is kept")).unwrap()
}

fn writable_mounts(host: &FakeHost) -> usize {
    host.mount_table()
        .unwrap()
        .iter()
        .filter(|m| m.mount_point.starts_with(RW_MOUNT))
        .count()
}

#[test]
fn a_killed_write_phase_is_put_back_while_the_record_is_kept() {
    let host = default_host();
    killed_in_write_phase(&host);
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());

    assert_eq!(reconciled.tasks.len(), 1);
    assert_eq!(reconciled.tasks[0].status, Some(Status::Interrupted));
    assert!(
        reconciled.host_residue.is_none(),
        "a completed reset leaves no leftover: {:?}",
        reconciled.host_residue
    );
    // The host is back: no writable mount, no temporary file, and the property reads back as the
    // value the record kept.
    assert_eq!(writable_mounts(&host), 0);
    assert!(!host.exists(RW_TMP_PATH));
    assert_eq!(host.readonly(), "on");
    // The recovery material is untouched: the record, its rootfs information and the task directory
    // are the only way back to the host state.
    let kept = kept_record(&host);
    assert_eq!(kept["status"], "INTERRUPTED");
    assert_eq!(kept["rootfs"]["readonly_before"], "on");
    assert_eq!(kept["replace"][0]["unloaded"][0], "nvidia_uvm");
    assert!(host.exists(paths().task_dir(ID).to_str().unwrap()));

    // The reset is idempotent: running it again changes nothing.
    let again = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert!(again.host_residue.is_none());
    assert_eq!(writable_mounts(&host), 0);
    assert_eq!(host.readonly(), "on");
    assert!(host.exists(paths().task_dir(ID).to_str().unwrap()));
}

#[test]
fn a_leftover_writable_mount_is_reported_and_keeps_the_task() {
    let host = default_host();
    killed_in_write_phase(&host);
    host.preset(
        "umount",
        &[RW_MOUNT],
        FakeOutput::code(1).with_stderr("umount: target is busy"),
    );
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());

    let residue = reconciled
        .host_residue
        .expect("the writable mount is still there");
    assert_eq!(residue.key, HOST_RW_MOUNT_LEFTOVER);
    assert_eq!(writable_mounts(&host), 1);
    assert!(host.exists(paths().task_dir(ID).to_str().unwrap()));
    assert_eq!(kept_record(&host)["rootfs"]["readonly_before"], "on");
}

#[test]
fn a_readonly_that_does_not_read_back_is_reported_with_its_reading() {
    let host = default_host();
    killed_in_write_phase(&host);
    // The property can be written but never reads back: the overlay case.
    host.preset(
        "zfs",
        &["get", "-H", "-o", "value", "readonly", USR_DATASET],
        FakeOutput::stdout("off\n"),
    );
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());

    let residue = reconciled
        .host_residue
        .expect("the property did not come back");
    assert_eq!(residue.key, HOST_READONLY_LEFTOVER);
    assert_eq!(residue.params["dataset"], USR_DATASET);
    assert_eq!(residue.params["reading"], "off");
    assert_eq!(residue.params["expected"], "on");
    // The mount itself was put back even though the property was not.
    assert_eq!(writable_mounts(&host), 0);
}

// ---- the plan and the submit are refused -----------------------------

async fn setup() -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    prime_kernel_headers(&host, KERNEL);
    host.set_curl(CATALOG_URL, 200, INDEX.as_bytes());
    signed_in_app(host).await
}

async fn official_plan(app: &TestApp, cookie: &str) -> (StatusCode, serde_json::Value) {
    post_plan(
        app,
        cookie,
        json!({"kind": "official", "version": "580.178.04", "gpus": []}),
    )
    .await
}

#[tokio::test]
async fn a_host_leftover_blocks_the_plan_and_the_remembered_submit() {
    let (app, cookie) = setup().await;
    let (status, plan) = official_plan(&app, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["result"], "ready", "{plan}");

    // The next read finds the leftover of an interrupted write phase.
    killed_in_write_phase(&app.host);
    app.host.preset(
        "umount",
        &[RW_MOUNT],
        FakeOutput::code(1).with_stderr("umount: target is busy"),
    );

    let (status, blocked) = official_plan(&app, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{blocked}");
    assert_eq!(blocked["result"], "blocked", "{blocked}");
    assert!(
        blocked["failures"].as_array().unwrap().iter().any(|f| {
            f["item"] == "host_not_restored" && f["reason"]["key"] == HOST_RW_MOUNT_LEFTOVER
        }),
        "{blocked}"
    );

    // The plan remembered for this host is refused at submit for the same reason instead of
    // starting on a host that still shows the leftover.
    let (status, refused) = post_task(&app, &cookie, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"], "plan.precheck_failed", "{refused}");
    assert!(
        refused["params"]["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["item"] == "host_not_restored"),
        "{refused}"
    );
    assert!(!app
        .host
        .unit_ids()
        .iter()
        .any(|id| id.contains(ID) && !id.is_empty()));
}

// ---- a terminal detail skips the whole-host reconciliation -----------

#[tokio::test]
async fn a_terminal_task_detail_does_not_reconcile_the_whole_host() {
    let (app, cookie) = setup().await;
    write_state(&app.host, ID, Status::Failed, Some(Phase::Check));
    app.host.clear_presets_and_commands();

    let (status, body) = get(&app, &format!("/api/tasks/{ID}"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "FAILED", "{body}");
    assert_eq!(body["phase"], "check", "{body}");
    // A terminal record cannot change again: the unit list, the container listing and the
    // state-directory walk are not asked for.
    assert_eq!(
        app.host.count("systemctl"),
        0,
        "a terminal detail queried systemd"
    );
    assert_eq!(
        app.host.count("docker"),
        0,
        "a terminal detail listed containers"
    );
}

#[tokio::test]
async fn a_running_task_detail_still_reconciles() {
    let (app, cookie) = setup().await;
    write_state(&app.host, ID, Status::Running, Some(Phase::Write));
    app.host.set_unit(ID, "active", "running", "success", 0);
    app.host.clear_presets_and_commands();

    let (status, body) = get(&app, &format!("/api/tasks/{ID}"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "RUNNING", "{body}");
    assert!(
        app.host.count("systemctl") > 0,
        "a running detail must reconcile"
    );
}

#[tokio::test]
async fn an_unreadable_record_still_goes_through_the_reconciliation() {
    let (app, cookie) = setup().await;
    app.host
        .put_text(paths().state(ID).to_str().unwrap(), "not json");
    app.host.mkdir(paths().task_dir(ID).to_str().unwrap());

    let (status, body) = get(&app, &format!("/api/tasks/{ID}"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The full path rebuilds the record and keeps the original bytes aside; the fast path must not
    // have answered from the unreadable file.
    assert_eq!(body["status"], "INTERRUPTED", "{body}");
    assert_eq!(body["state_read_error"], "invalid", "{body}");
    assert_eq!(
        body["state_note"]["kept_as"],
        format!("{ID}.json.unreadable")
    );
}

#[test]
fn release_after_later_success_keeps_history_and_deletes_material() {
    let host = default_host();
    release_scene(&host);
    truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    let state: serde_json::Value =
        serde_json::from_str(&state_text(&host, RELEASE_X).unwrap()).unwrap();
    assert_eq!(state["released_by"], RELEASE_L, "{state}");
    assert!(state["released_at"].is_u64(), "{state}");
    assert!(!host.exists(paths().task_dir(RELEASE_X).to_str().unwrap()));
    assert!(host.exists(paths().state_log(RELEASE_X).to_str().unwrap()));
}

fn assert_release_kept(host: &FakeHost) {
    truenas_nvidia_driver_manager::tasks::reconcile(host, &paths());
    let state: serde_json::Value =
        serde_json::from_str(&state_text(host, RELEASE_X).unwrap()).unwrap();
    assert!(state["released_by"].is_null(), "{state}");
    assert!(host.exists(paths().task_dir(RELEASE_X).to_str().unwrap()));
}

#[test]
fn release_requires_a_later_normally_closed_real_replacement() {
    for case in [
        "newest",
        "same_time",
        "failed",
        "interrupted",
        "no_replace",
        "no_check",
        "bad_refresh",
        "same_merge_time",
    ] {
        let host = default_host();
        release_scene(&host);
        let mut l = release_state(&host, RELEASE_L);
        match case {
            "newest" => l.created_at = 0,
            "same_time" => l.created_at = 1,
            "failed" => l.status = Status::Failed,
            "interrupted" => l.status = Status::Interrupted,
            "no_replace" => {
                l.replace.clear();
                l.checks.clear();
            }
            "no_check" => l.checks.clear(),
            "bad_refresh" => l.checks[0].refresh_ok = false,
            "same_merge_time" => l.checks[0].since_before = l.checks[0].since_after.clone(),
            _ => unreachable!(),
        }
        task_state::write(&host, &paths(), &l).unwrap();
        assert_release_kept(&host);
    }
}

#[test]
fn release_requires_the_verified_target_row_and_canonical_hash() {
    for case in ["missing_row", "row_hash", "canonical_hash"] {
        let host = default_host();
        release_scene(&host);
        let row = paths().row_dir(&format!("{RELEASE_L}-build"));
        match case {
            "missing_row" => host.remove(row.to_str().unwrap()),
            "row_hash" => host.put_text(row.join("nvidia.raw").to_str().unwrap(), "changed"),
            "canonical_hash" => host.put_text(CANONICAL_PATH, "changed"),
            _ => unreachable!(),
        }
        assert_release_kept(&host);
    }
}

#[test]
fn release_requires_matching_live_kernel_and_smi_versions_and_modules() {
    for case in ["kernel", "smi", "module"] {
        let host = default_host();
        release_scene(&host);
        match case {
            "kernel" => host.put_text("/proc/driver/nvidia/version", "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  580.173.02  Release Build\n"),
            "smi" => host.preset("nvidia-smi", &[], FakeOutput::stdout("580.173.02\n")),
            "module" => host.put_text("/proc/modules", "nvidia 100 0 - Live 0\n"),
            _ => unreachable!(),
        }
        assert_release_kept(&host);
    }
}

#[test]
fn release_requires_clean_host_readback_for_all_unresolved_records() {
    for case in ["mount", "readonly", "another_record"] {
        let host = default_host();
        release_scene(&host);
        if case == "mount" {
            host.add_mount(RELEASE_DATASET, RW_MOUNT, "zfs", "rw");
            host.preset("umount", &[RW_MOUNT], FakeOutput::code(1));
        } else {
            if case == "another_record" {
                let mut other = release_state(&host, RELEASE_X);
                other.task_id = "20261004T101530Z-abcdef56".into();
                other.rootfs.as_mut().unwrap().readonly_before = "off".into();
                task_state::write(&host, &paths(), &other).unwrap();
                host.mkdir(paths().task_dir(&other.task_id).to_str().unwrap());
            }
            host.preset(
                "zfs",
                &["get", "-H", "-o", "value", "readonly", RELEASE_DATASET],
                FakeOutput::stdout(if case == "readonly" { "off\n" } else { "on\n" }),
            );
        }
        assert_release_kept(&host);
    }
}

#[test]
fn release_keeps_unreadable_and_recovery_sidecars() {
    for case in ["unreadable", "recovery", "later_unreadable"] {
        let host = default_host();
        release_scene(&host);
        let id = if case == "later_unreadable" {
            RELEASE_L
        } else {
            RELEASE_X
        };
        let name = if case == "recovery" {
            format!(".{id}.json.recovery-0123456789abcdef")
        } else {
            format!("{id}.json.unreadable")
        };
        host.put_text(
            paths().state_dir().join(name).to_str().unwrap(),
            "protected bytes",
        );
        assert_release_kept(&host);
    }
}

#[test]
fn release_after_remerge_accepts_current_verified_image() {
    let host = default_host();
    release_scene(&host);
    host.model_sysext_unmerge();
    host.model_sysext_merge();
    truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    let state: serde_json::Value =
        serde_json::from_str(&state_text(&host, RELEASE_X).unwrap()).unwrap();
    assert_eq!(state["released_by"], RELEASE_L, "{state}");
    assert!(!host.exists(paths().task_dir(RELEASE_X).to_str().unwrap()));
}

#[test]
fn release_cleans_multiple_interrupted_records() {
    let host = default_host();
    release_scene(&host);
    let mut other = release_state(&host, RELEASE_X);
    other.task_id = "20261004T101530Z-abcdef56".into();
    task_state::write(&host, &paths(), &other).unwrap();
    host.mkdir(paths().task_dir(&other.task_id).to_str().unwrap());
    truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    for id in [RELEASE_X, &other.task_id] {
        let state: serde_json::Value =
            serde_json::from_str(&state_text(&host, id).unwrap()).unwrap();
        assert_eq!(state["released_by"], RELEASE_L, "{state}");
        assert!(!host.exists(paths().task_dir(id).to_str().unwrap()));
    }
}

#[test]
fn release_marker_survives_cleanup_failure_and_reconcile_retries() {
    let host = default_host();
    release_scene(&host);
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let wrapped = Wrap(ReleaseCleanupFailure {
        host: host.clone(),
        fail: fail.clone(),
    });
    truenas_nvidia_driver_manager::tasks::reconcile(&wrapped, &paths());
    let state: serde_json::Value =
        serde_json::from_str(&state_text(&host, RELEASE_X).unwrap()).unwrap();
    assert_eq!(state["released_by"], RELEASE_L, "{state}");
    assert!(host.exists(paths().task_dir(RELEASE_X).to_str().unwrap()));
    fail.store(false, std::sync::atomic::Ordering::SeqCst);
    truenas_nvidia_driver_manager::tasks::reconcile(&wrapped, &paths());
    assert!(!host.exists(paths().task_dir(RELEASE_X).to_str().unwrap()));
}

#[test]
fn release_accepts_a_verified_stock_restore_pass() {
    let host = default_host();
    release_restore_scene(&host);
    truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    let state: serde_json::Value =
        serde_json::from_str(&state_text(&host, RELEASE_X).unwrap()).unwrap();
    assert_eq!(state["released_by"], RELEASE_L, "{state}");
    assert!(!host.exists(paths().task_dir(RELEASE_X).to_str().unwrap()));
}

#[test]
fn release_auto_requires_idle_units_and_an_available_execution_lock() {
    for case in ["running", "unknown", "locked"] {
        let host = default_host();
        release_scene(&host);
        let _lock = if case == "locked" {
            Some(
                truenas_nvidia_driver_manager::admission::try_execution_lock(&host)
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        if case == "running" {
            host.set_unit(
                "20261007T101530Z-abcdef78",
                "active",
                "running",
                "success",
                0,
            );
        } else if case == "unknown" {
            host.preset("systemctl", &["list-units"], FakeOutput::code(1));
        }
        assert_release_kept(&host);
    }
}

#[test]
fn release_marker_does_not_override_new_protected_recovery_evidence() {
    let host = default_host();
    release_scene(&host);
    let mut x = release_state(&host, RELEASE_X);
    x.released_by = Some(RELEASE_L.into());
    x.released_at = Some(4);
    task_state::write(&host, &paths(), &x).unwrap();
    host.put_text(
        paths().state_unreadable(RELEASE_X).to_str().unwrap(),
        "protected bytes",
    );
    truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    assert!(host.exists(paths().task_dir(RELEASE_X).to_str().unwrap()));
    assert!(host.exists(paths().state_unreadable(RELEASE_X).to_str().unwrap()));
    assert!(truenas_nvidia_driver_manager::tasks::driver_settled(&host, &paths(), &x).is_err());
}

#[test]
fn release_requires_a_durable_marker_before_removing_directory() {
    let host = default_host();
    release_scene(&host);
    host.fail_rename_to_suffix(&format!("{RELEASE_X}.json"));
    assert_release_kept(&host);
}

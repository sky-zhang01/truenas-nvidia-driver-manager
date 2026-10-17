//! Whole-task replacement paths on the injected host.

mod common;

use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::drivers::{OriginalOf, Record};
use truenas_nvidia_driver_manager::host::{Exit, Host};
use truenas_nvidia_driver_manager::runner::build::{check_artifact, Artifact, BuildFailure};
use truenas_nvidia_driver_manager::runner::{self, RunCtx};
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{
    self, new_state, OriginalChoice, Phase, PlanSource, RowKind, Status, TaskState,
};
use truenas_nvidia_driver_manager::tasks;

const ID: &str = "20260928T101530Z-3f9a0c12";
const NEW_BYTES: &[u8] = b"new driver image";
const OLD_BYTES: &[u8] = b"stock driver image";

fn paths() -> Paths {
    Paths::new(HOST_DATA_DIR)
}

fn record(kind: RowKind, version: &str, bytes: &[u8]) -> Record {
    Record {
        kind,
        version: version.into(),
        sha256: sha256_hex(bytes),
        size: bytes.len() as u64,
        target_kernel: KERNEL.into(),
        created_at: 1_790_000_000_000,
        fingerprint: None,
        original_of: (kind == RowKind::Original).then(|| OriginalOf {
            kernel: KERNEL.into(),
            truenas_version: TRUENAS_VERSION.into(),
        }),
        keep: (kind == RowKind::Original).then_some(false),
    }
}

fn put_row(host: &FakeHost, row_id: &str, record: &Record, bytes: &[u8]) {
    let row = paths().row_dir(row_id);
    host.put_file(row.join("nvidia.raw").to_str().unwrap(), bytes.to_vec());
    host.put_text(
        row.join("record.json").to_str().unwrap(),
        &serde_json::to_string(record).unwrap(),
    );
    host.set_listing(
        row.join("nvidia.raw").to_str().unwrap(),
        &canonical_listing(KERNEL, &record.version),
    );
}

fn scene(save_original: bool) -> FakeHost {
    let host = FakeHost::new();
    // A real ZFS dataset name must satisfy the state contract's dataset-name grammar.
    host.remove_mount("/usr");
    host.add_mount("tank/ROOT/default/usr", "/usr", "zfs", "ro");
    host.put_file(CANONICAL_PATH, OLD_BYTES.to_vec());
    host.set_listing(CANONICAL_PATH, &canonical_listing(KERNEL, "580.173.02"));
    host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH);
    host.load_module("nvidia", 0, &[]);
    let row_id = format!("{ID}-build");
    let row = record(RowKind::Official, "580.178.04", NEW_BYTES);
    put_row(&host, &row_id, &row, NEW_BYTES);
    if save_original {
        let row_id = format!("{ID}-original");
        let row = record(RowKind::Original, "580.173.02", OLD_BYTES);
        put_row(&host, &row_id, &row, OLD_BYTES);
    }
    let mut plan = sample_plan(PlanSource::Local {
        row_id,
        row_kind: RowKind::Official,
        fingerprint: None,
        sha256: row.sha256,
    });
    plan.original = if save_original {
        OriginalChoice::AlreadySaved
    } else {
        OriginalChoice::Skip
    };
    let state = new_state(ID, 1, plan, Phase::ReadLoadedVersion);
    task_state::write(&host, &paths(), &state).unwrap();
    host
}

fn run(host: &FakeHost) -> std::process::ExitCode {
    let _runner = host.enter_runner_scope();
    runner::run_with(Arc::new(host.clone()) as Arc<dyn Host>, paths(), ID)
}

fn persisted(host: &FakeHost) -> TaskState {
    match task_state::read(host, &paths(), ID) {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!(
            "state unreadable: {other:?}; raw: {:?}",
            host.file_text(paths().state(ID).to_str().unwrap())
        ),
    }
}

/// Release every registered barrier and collect the runner even during unwinding.
struct BlockedRunner {
    join: Option<std::thread::JoinHandle<std::process::ExitCode>>,
    gates: Vec<Arc<common::fake_host::Gate>>,
}

impl BlockedRunner {
    fn wait_at(&mut self, gate: &Arc<common::fake_host::Gate>) {
        self.gates.push(gate.clone());
        assert!(
            gate.wait_blocked(self.join.as_ref().unwrap()),
            "runner did not reach its barrier"
        );
    }

    fn join(mut self) -> std::thread::Result<std::process::ExitCode> {
        for gate in &self.gates {
            gate.release();
        }
        self.join.take().unwrap().join()
    }
}

impl Drop for BlockedRunner {
    fn drop(&mut self) {
        for gate in &self.gates {
            gate.release();
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run_until_blocked(host: &FakeHost, gate: &Arc<common::fake_host::Gate>) -> BlockedRunner {
    let worker = host.clone();
    let mut runner = BlockedRunner {
        join: Some(std::thread::spawn(move || run(&worker))),
        gates: Vec::new(),
    };
    runner.wait_at(gate);
    runner
}

#[test]
fn admitted_update_writes_canonical_and_finishes_succeeded() {
    let host = scene(false);
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Succeeded);
    assert_eq!(state.checks.len(), 1);
    assert!(state.checks[0].passed);
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(NEW_BYTES));
    assert!(!host.exists(RW_TMP_PATH));
    assert_eq!(state.replace[0].readback_ok, Some(true));
    assert_eq!(
        state.replace[0]
            .readonly_after
            .as_ref()
            .and_then(|r| r.as_value())
            .map(String::as_str),
        Some("on")
    );
    assert_eq!(host.readonly(), "on");
    assert!(!host
        .mount_table()
        .unwrap()
        .iter()
        .any(|m| m.mount_point == RW_MOUNT));
    assert_eq!(host.count("mv"), 1);
    assert_eq!(host.count("systemd-sysext"), 5);
}

#[test]
fn production_runner_confirms_unmerge_before_accepting_the_merge() {
    let host = scene(false);
    let _runner = host.enter_runner_scope();
    assert_eq!(
        runner::run_with(Arc::new(host.clone()), paths(), ID),
        std::process::ExitCode::SUCCESS
    );
    let state = persisted(&host);
    assert_eq!(state.status, Status::Succeeded);
    assert!(state.checks[0].refresh_ok);
    let sysext = host
        .argv_log()
        .into_iter()
        .filter(|argv| argv[0] == "systemd-sysext")
        .collect::<Vec<_>>();
    assert_eq!(
        sysext,
        vec![
            vec!["systemd-sysext", "status", "--json=short"],
            vec!["systemd-sysext", "unmerge"],
            vec!["systemd-sysext", "status", "--json=short"],
            vec!["systemd-sysext", "merge"],
            vec!["systemd-sysext", "status", "--json=short"],
        ]
    );
}

#[test]
fn same_version_new_bytes_are_merged_while_an_old_file_handle_stays_old() {
    let host = scene(false);
    let row_id = format!("{ID}-build");
    let row = record(RowKind::Official, "580.173.02", NEW_BYTES);
    put_row(&host, &row_id, &row, NEW_BYTES);
    let mut state = persisted(&host);
    state.plan.as_mut().unwrap().version = "580.173.02".into();
    task_state::write(&host, &paths(), &state).unwrap();
    let mut old_handle = host.open(Path::new(CANONICAL_PATH)).unwrap();

    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Succeeded);
    assert!(state.checks[0].refresh_ok);
    assert!(state.checks[0].version_ok);
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(NEW_BYTES));
    let mut held_bytes = Vec::new();
    old_handle.read_to_end(&mut held_bytes).unwrap();
    assert_eq!(held_bytes, OLD_BYTES);
}

#[test]
fn unreadable_intermediate_status_still_merges_but_cannot_pass_refresh() {
    for (case, output) in [
        (
            "missing since",
            FakeOutput::stdout(r#"[{"hierarchy":"/usr","extensions":"none"}]"#),
        ),
        (
            "other hierarchy",
            FakeOutput::stdout(r#"[{"hierarchy":"/opt","extensions":"none","since":null}]"#),
        ),
        (
            "empty array",
            FakeOutput::stdout(r#"[{"hierarchy":"/usr","extensions":[],"since":7}]"#),
        ),
        (
            "code",
            FakeOutput {
                exit: Exit::Code(1),
                ..FakeOutput::stdout(r#"[{"hierarchy":"/usr","extensions":"none","since":null}]"#)
            },
        ),
        (
            "signal",
            FakeOutput {
                exit: Exit::Signal(9),
                ..FakeOutput::ok()
            },
        ),
        (
            "timeout",
            FakeOutput {
                exit: Exit::TimedOut,
                ..FakeOutput::ok()
            },
        ),
        (
            "spawn failed",
            FakeOutput {
                exit: Exit::SpawnFailed,
                ..FakeOutput::ok()
            },
        ),
    ] {
        let host = scene(false);
        let gate = host.set_gate("systemd-sysext", &["unmerge"]);
        let join = run_until_blocked(&host, &gate);
        assert!(gate.blocked(), "unmerge was not reached: {case}");
        host.preset_once("systemd-sysext", &["status", "--json=short"], output);
        gate.release();

        assert_eq!(join.join().unwrap(), std::process::ExitCode::SUCCESS);
        let state = persisted(&host);
        assert_eq!(state.status, Status::Failed, "{case}");
        assert_eq!(
            state.error.as_ref().unwrap().code,
            "NO_USABLE_ORIGINAL",
            "{case}"
        );
        assert_eq!(state.checks.len(), 1, "{case}");
        assert!(!state.checks[0].refresh_ok, "{case}");
        assert!(!state.checks[0].passed, "{case}");
        assert_ne!(
            state.checks[0].since_before, state.checks[0].since_after,
            "{case}"
        );
        assert_eq!(state.replace[0].refresh_exit, Some(Exit::Code(0).to_json()));
        let sysext = host
            .argv_log()
            .into_iter()
            .filter(|argv| argv[0] == "systemd-sysext")
            .collect::<Vec<_>>();
        assert_eq!(sysext.len(), 5, "{case}: {sysext:?}");
        assert_eq!(sysext[3], ["systemd-sysext", "merge"], "{case}");
        assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(NEW_BYTES));
    }
}

/// Holds `systemd-sysext <verb>` with a failing exit, lets the host model the real effect, and
/// expects the actual status reading to decide success.
fn sysext_exit_case(verb: &str, model: fn(&FakeHost), refresh_exit: fn(Exit) -> serde_json::Value) {
    for exit in [
        Exit::Code(1),
        Exit::Signal(9),
        Exit::TimedOut,
        Exit::SpawnFailed,
    ] {
        let host = scene(false);
        let gate = host.set_gate_output(
            "systemd-sysext",
            &[verb],
            FakeOutput {
                exit,
                ..FakeOutput::ok()
            },
        );
        let join = run_until_blocked(&host, &gate);
        assert!(gate.blocked(), "{verb} was not reached: {exit:?}");
        model(&host);
        gate.release();

        assert_eq!(join.join().unwrap(), std::process::ExitCode::SUCCESS);
        let state = persisted(&host);
        assert_eq!(state.status, Status::Succeeded, "{exit:?}");
        assert!(state.checks[0].refresh_ok, "{exit:?}");
        assert_eq!(state.replace[0].refresh_exit, Some(refresh_exit(exit)));
        assert_eq!(host.count("systemd-sysext"), 5, "{exit:?}");
        let log = host
            .file_text(paths().state_log(ID).to_str().unwrap())
            .unwrap();
        let logged = log
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|line| line["key"] == "log.cmd" && line["p"]["argv"][1] == verb)
            .unwrap();
        assert_eq!(logged["p"]["exit"], exit.to_json());
    }
}

#[test]
fn unmerge_exit_does_not_replace_the_intermediate_host_reading() {
    sysext_exit_case("unmerge", FakeHost::model_sysext_unmerge, |_| {
        Exit::Code(0).to_json()
    });
}

#[test]
fn merge_exit_is_recorded_but_actual_new_status_decides_success() {
    sysext_exit_case("merge", FakeHost::model_sysext_merge, |exit| exit.to_json());
}

#[test]
fn entering_stage_one_logs_once() {
    let host = scene(false);
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let log = host
        .file_text(paths().state_log(ID).to_str().unwrap())
        .unwrap();
    let entries = log
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|line| {
            line["key"] == "log.phase_enter" && line["p"]["phase"] == "read_loaded_version"
        })
        .count();
    assert_eq!(entries, 1, "{log}");
}

#[test]
fn success_remains_running_until_stage_fifteen_commits() {
    let host = scene(false);
    let gate = host.set_gate("docker", &["ps"]);
    let join = run_until_blocked(&host, &gate);
    let blocked = gate.blocked();
    let during = persisted(&host);
    gate.release();
    assert!(blocked, "stage 15 did not reach its GPU observation");
    assert_eq!(during.phase, Some(Phase::ReadLoadedVersionEnd));
    assert_eq!(during.status, Status::Running);
    assert_eq!(during.finished_at, None);
    assert_eq!(join.join().unwrap(), std::process::ExitCode::SUCCESS);
    assert_eq!(persisted(&host).status, Status::Succeeded);
}

#[test]
fn failed_final_write_leaves_running_without_a_finished_log() {
    let host = scene(false);
    let gate = host.set_gate("docker", &["ps"]);
    let join = run_until_blocked(&host, &gate);
    let blocked = gate.blocked();
    if blocked {
        host.fail_write(&format!("{HOST_DATA_DIR}/manager/state/.{ID}.json.tmp-"));
    }
    gate.release();
    assert!(blocked, "stage 15 did not reach its GPU observation");
    assert_eq!(join.join().unwrap(), std::process::ExitCode::FAILURE);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Running);
    assert_eq!(state.finished_at, None);
    assert_eq!(state.phase, Some(Phase::ReadLoadedVersionEnd));
    let log = host
        .file_text(paths().state_log(ID).to_str().unwrap())
        .unwrap();
    assert!(!log.contains("log.finished"));
}

#[test]
fn failed_update_check_restores_original_once() {
    let host = scene(true);
    host.preset_once(
        "nvidia-smi",
        &["--query-gpu=driver_version", "--format=csv,noheader"],
        FakeOutput::stdout("0.0\n"),
    );
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::StockRestored);
    assert_eq!(state.checks.len(), 2);
    assert!(!state.checks[0].passed);
    assert!(state.checks[1].passed);
    assert_eq!(state.replace.len(), 2);
    assert_eq!(host.count("mv"), 2);
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
    assert_eq!(
        host.file(&format!("{HOST_DATA_DIR}/drivers/{ID}-original/nvidia.raw"))
            .as_deref(),
        Some(OLD_BYTES)
    );
    assert_eq!(host.readonly(), "on");
}

#[test]
fn failed_update_check_without_original_finishes_with_reason() {
    let host = scene(false);
    host.preset_once(
        "nvidia-smi",
        &["--query-gpu=driver_version", "--format=csv,noheader"],
        FakeOutput::stdout("0.0\n"),
    );
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    assert_eq!(state.error.unwrap().code, "NO_USABLE_ORIGINAL");
    assert_eq!(
        state.restore.unwrap().reason.as_deref(),
        Some(RESTORE_NOT_SAVED)
    );
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(NEW_BYTES));
    assert_eq!(host.count("mv"), 1);
}

/// A signaled reading is an unknown check that restores the saved original.
fn signaled_check_restores_original(
    program: &str,
    args: &[&str],
    stderrs: [&str; 2],
    reading: fn(&task_state::CheckRecord) -> serde_json::Value,
) {
    for stderr in stderrs {
        let host = scene(true);
        let mut signaled = FakeOutput::ok();
        signaled.exit = Exit::Signal(9);
        signaled.stderr = stderr.as_bytes().to_vec();
        host.preset_once(program, args, signaled);
        assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
        let state = persisted(&host);
        assert_eq!(state.status, Status::StockRestored);
        assert!(state.finished_at.is_some());
        assert!(state.error.is_none());
        assert_eq!(state.checks.len(), 2);
        assert!(!state.checks[0].passed);
        assert_eq!(
            reading(&state.checks[0]),
            serde_json::json!({"unknown": {"key": REASON_SIGNAL, "params": {"signal": 9}}})
        );
        assert!(state.checks[1].passed);
        assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
        assert_eq!(host.count("modprobe"), 2);
        let log = host
            .file_text(paths().state_log(ID).to_str().unwrap())
            .unwrap();
        assert!(log.contains("\"signal\":9"));
        assert!(!log.contains("\"code\":9"));
    }
}

#[test]
fn signaled_sysext_status_is_an_unknown_check_and_restores_original() {
    signaled_check_restores_original(
        "systemd-sysext",
        &["status", "--json=short"],
        ["", "systemd-sysext: killed"],
        |check| serde_json::to_value(&check.since_before).unwrap(),
    );
}

#[test]
fn signaled_nvidia_smi_is_an_unknown_check_and_restores_original() {
    signaled_check_restores_original(
        "nvidia-smi",
        &["--query-gpu=driver_version", "--format=csv,noheader"],
        ["", "nvidia-smi: killed"],
        |check| serde_json::to_value(&check.nvidia_smi).unwrap(),
    );
}

#[test]
fn signaled_check_without_original_records_the_existing_no_restore_result() {
    let host = scene(false);
    let mut signaled = FakeOutput::ok();
    signaled.exit = Exit::Signal(9);
    host.preset_once("systemd-sysext", &["status", "--json=short"], signaled);
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    assert_eq!(state.error.unwrap().code, "NO_USABLE_ORIGINAL");
    assert_eq!(
        state.restore.unwrap().reason.as_deref(),
        Some(RESTORE_NOT_SAVED)
    );
    assert_eq!(
        serde_json::to_value(&state.checks[0].since_before).unwrap(),
        serde_json::json!({"unknown": {"key": REASON_SIGNAL, "params": {"signal": 9}}})
    );
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(NEW_BYTES));
}

#[test]
fn failed_temp_write_rolls_back_without_touching_canonical() {
    let host = scene(false);
    host.fail_write(RW_TMP_PATH);
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    assert_eq!(state.error.unwrap().code, "WRITE_FAILED");
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
    assert_eq!(host.count("mv"), 0);
    assert_eq!(host.count("rmmod"), 1);
    assert_eq!(host.count("modprobe"), 1);
    assert_eq!(host.readonly(), "on");
    assert!(!host
        .mount_table()
        .unwrap()
        .iter()
        .any(|m| m.mount_point == RW_MOUNT));
}

#[test]
fn changed_local_row_metadata_cannot_reach_canonical_write() {
    for wrong_kind in [false, true] {
        let host = scene(false);
        let mut row = record(RowKind::Official, "580.178.04", NEW_BYTES);
        if wrong_kind {
            row.kind = RowKind::Upload;
            row.fingerprint = Some("a".repeat(12));
        } else {
            row.target_kernel = "6.12.120-production+truenas".into();
        }
        put_row(&host, &format!("{ID}-build"), &row, NEW_BYTES);
        assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
        let state = persisted(&host);
        assert_eq!(state.status, Status::Failed);
        let error = state.error.unwrap();
        assert_eq!(error.code, "WRITE_FAILED");
        assert_eq!(error.params["write_reason"]["key"], WRITE_SOURCE_UNREADABLE);
        assert_eq!(host.count("mv"), 0);
        assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
        assert_eq!(host.count("modprobe"), 1);
    }
}

#[test]
fn unconfirmed_umount_never_recursively_deletes_the_live_mount() {
    for (stop_before_write, exit_code) in [(true, 32), (false, 32), (true, 0), (false, 0)] {
        let host = scene(false);
        host.mkdir(paths().task_dir(ID).to_str().unwrap());
        if stop_before_write {
            host.fail_write(RW_TMP_PATH);
        }
        // A zero exit without a changed mount table is also unconfirmed.
        host.preset("umount", &[RW_MOUNT], FakeOutput::code(exit_code));
        assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
        assert_eq!(
            persisted(&host).status,
            if stop_before_write {
                Status::Failed
            } else {
                Status::Succeeded
            }
        );
        assert!(host
            .mount_table()
            .unwrap()
            .iter()
            .any(|m| m.mount_point == RW_MOUNT));
        assert!(!host.ops_for(RW_MOUNT).contains(&"remove_dir_all"));
        assert!(host.exists(paths().task_dir(ID).to_str().unwrap()));
    }
}

#[test]
fn a_nested_mount_survives_the_successful_parent_unmount() {
    let host = scene(false);
    let nested = format!("{RW_MOUNT}/foreign");
    let foreign_file = format!("{nested}/keep");
    host.put_text(&foreign_file, "foreign data");
    host.add_mount("foreign-dataset", &nested, "zfs", "rw");

    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    assert_eq!(persisted(&host).status, Status::Succeeded);
    assert!(host.exists(&foreign_file));
    assert!(host
        .mount_table()
        .unwrap()
        .iter()
        .any(|m| m.mount_point == nested));
    assert!(!host.ops_for(RW_MOUNT).contains(&"remove_dir_all"));
}

#[test]
fn refused_temp_removal_keeps_the_mount_for_cleanup_retry() {
    let host = scene(false);
    host.mkdir(paths().task_dir(ID).to_str().unwrap());
    host.preset_once(
        "mv",
        &["-T", RW_TMP_PATH, RW_CANONICAL_PATH],
        FakeOutput::code(9),
    );
    host.fail_remove(RW_TMP_PATH);
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    assert_eq!(state.error.unwrap().code, "WRITE_FAILED");
    assert!(host.exists(RW_TMP_PATH));
    assert!(host
        .mount_table()
        .unwrap()
        .iter()
        .any(|m| m.mount_point == RW_MOUNT));
    assert_eq!(host.count("umount"), 0);
    assert!(!host.ops_for(RW_MOUNT).contains(&"remove_dir_all"));
    assert!(!host
        .ops_for(paths().task_dir(ID).to_str().unwrap())
        .contains(&"remove_dir_all"));
    assert!(host.exists(paths().task_dir(ID).to_str().unwrap()));
    let log = host
        .file_text(paths().state_log(ID).to_str().unwrap())
        .unwrap();
    assert!(log.contains("log.cleanup_failed"));
}

#[test]
fn preexisting_foreign_mount_is_neither_used_nor_removed() {
    let host = scene(false);
    host.add_mount("tank/foreign/usr", RW_MOUNT, "zfs", "rw");
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    let failure = state.error.unwrap();
    assert_eq!(failure.code, "WRITE_FAILED");
    assert_eq!(
        failure.params["write_reason"]["key"],
        WRITE_RW_MOUNT_NOT_CONFIRMED
    );
    assert_eq!(host.count("mount"), 0);
    assert_eq!(host.count("umount"), 0);
    assert_eq!(host.count("mv"), 0);
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
    assert!(host
        .mount_table()
        .unwrap()
        .iter()
        .any(|m| m.mount_point == RW_MOUNT && m.source == "tank/foreign/usr"));
    assert!(!host.ops_for(RW_MOUNT).contains(&"remove_dir_all"));
}

#[test]
fn early_failure_without_rootfs_does_not_unmount_foreign_view() {
    let host = scene(false);
    host.add_mount("tank/foreign/usr", RW_MOUNT, "zfs", "rw");
    host.refuse_unload("nvidia");
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    assert!(state.rootfs.is_none());
    assert_eq!(state.error.unwrap().code, "MODULE_UNLOAD_REFUSED");
    assert_eq!(host.count("umount"), 0);
    assert!(host
        .mount_table()
        .unwrap()
        .iter()
        .any(|m| m.mount_point == RW_MOUNT && m.source == "tank/foreign/usr"));
    assert!(!host.ops_for(RW_MOUNT).contains(&"remove_dir_all"));
}

#[test]
fn mount_deadline_rolls_back_and_preserves_command_exit() {
    let host = scene(false);
    let mut timed_out = FakeOutput::ok();
    timed_out.exit = Exit::TimedOut;
    host.preset_once(
        "mount",
        &[
            "-t",
            "zfs",
            "-o",
            "zfsutil,rw",
            "tank/ROOT/default/usr",
            RW_MOUNT,
        ],
        timed_out,
    );
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    let error = state.error.unwrap();
    assert_eq!(error.code, "REPLACE_DEADLINE_EXCEEDED");
    assert_eq!(error.exit, Some(serde_json::json!({"timed_out": true})));
    assert_eq!(error.command.unwrap()[0], "mount");
    assert_eq!(host.count("mv"), 0);
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
    assert_eq!(host.readonly(), "on");
}

#[test]
fn interrupted_rmmod_rolls_back_a_possible_kernel_side_unload() {
    for exit in [Exit::TimedOut, Exit::Signal(9)] {
        let host = scene(false);
        host.load_module("nvidia_uvm", 0, &[]);
        let mut output = FakeOutput::ok();
        output.exit = exit;
        let gate = host.set_gate_output("rmmod", &["nvidia"], output);
        let join = run_until_blocked(&host, &gate);
        let blocked = gate.blocked();
        if blocked {
            host.unload_module("nvidia");
        }
        gate.release();
        assert!(blocked, "rmmod did not reach the gate");
        assert_eq!(join.join().unwrap(), std::process::ExitCode::SUCCESS);
        let state = persisted(&host);
        assert_eq!(state.replace[0].unloaded, ["nvidia_uvm"]);
        assert_eq!(
            state.replace[0]
                .reloaded
                .iter()
                .map(|item| item.module.as_str())
                .collect::<Vec<_>>(),
            ["nvidia", "nvidia_uvm"]
        );
        assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
        assert!(host
            .read(PROC_MODULES_PATH.as_ref())
            .unwrap()
            .starts_with(b"nvidia"));
        if exit == Exit::TimedOut {
            assert_eq!(state.status, Status::Failed);
            assert_eq!(state.error.unwrap().code, "REPLACE_DEADLINE_EXCEEDED");
        } else {
            assert_eq!(state.status, Status::Failed);
            let error = state.error.unwrap();
            assert_eq!(error.code, "MODULE_UNLOAD_REFUSED");
            assert_eq!(
                error.params["unload_reason"],
                serde_json::json!({"key": REASON_SIGNAL, "params": {"signal": 9}})
            );
            assert_eq!(error.exit, Some(serde_json::json!({"signal": 9})));
        }
    }
}

/// A failed pre-write rename must leave the old file and exactly one reload.
fn failed_rename(output: FakeOutput) -> (FakeHost, TaskState) {
    let host = scene(false);
    host.preset_once("mv", &["-T", RW_TMP_PATH, RW_CANONICAL_PATH], output);
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
    assert_eq!(host.count("modprobe"), 1);
    (host, state)
}

#[test]
fn refused_rename_keeps_old_file_and_raw_command_diagnostic() {
    let (_host, state) = failed_rename(FakeOutput::code(9).with_stderr("mv: refused"));
    let error = state.error.unwrap();
    assert_eq!(error.code, "WRITE_FAILED");
    assert_eq!(error.params["write_reason"]["key"], WRITE_RENAME_FAILED);
    assert_eq!(error.exit, Some(serde_json::json!({"code": 9})));
    assert_eq!(
        error.command.unwrap(),
        vec!["mv", "-T", RW_TMP_PATH, RW_CANONICAL_PATH]
    );
    assert!(error.raw.unwrap().contains("mv: refused"));
}

#[test]
fn signaled_rename_with_temp_still_present_keeps_old_file() {
    let mut signaled = FakeOutput::ok();
    signaled.exit = Exit::Signal(9);
    let (_host, state) = failed_rename(signaled);
    let error = state.error.unwrap();
    assert_eq!(error.code, "WRITE_FAILED");
    assert_eq!(error.params["write_reason"]["key"], WRITE_RENAME_FAILED);
    assert_eq!(error.exit, Some(serde_json::json!({"signal": 9})));
}

#[test]
fn timed_out_rename_with_temp_still_present_is_prewrite_deadline() {
    let mut timed_out = FakeOutput::ok();
    timed_out.exit = Exit::TimedOut;
    let (host, state) = failed_rename(timed_out);
    assert_eq!(state.error.unwrap().code, "REPLACE_DEADLINE_EXCEEDED");
    assert!(!host.exists(RW_TMP_PATH));
}

/// Models the rename completing on the host while `mv` is held at its gate.
fn kernel_side_rename_done(host: &FakeHost) {
    host.put_file(RW_CANONICAL_PATH, NEW_BYTES.to_vec());
    host.put_file(CANONICAL_PATH, NEW_BYTES.to_vec());
    host.set_listing(CANONICAL_PATH, &canonical_listing(KERNEL, "580.178.04"));
    host.remove(RW_TMP_PATH);
}

#[test]
fn timed_out_rename_after_kernel_side_move_finishes_written_pass() {
    let host = scene(false);
    let gate = host.set_gate("mv", &["-T", RW_TMP_PATH, RW_CANONICAL_PATH]);
    let join = run_until_blocked(&host, &gate);
    let blocked = gate.blocked();
    if blocked {
        // Model rename completing on the host just before the command's remaining-time watchdog
        // fires. The preset itself has no side effect.
        kernel_side_rename_done(&host);
        host.advance(REPLACE_DEADLINE + std::time::Duration::from_secs(1));
    }
    gate.release();
    assert!(blocked, "rename did not reach the gate");
    assert_eq!(join.join().unwrap(), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Succeeded);
    assert_eq!(state.replace[0].readback_ok, Some(true));
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(NEW_BYTES));
}

#[test]
fn signaled_rename_after_kernel_side_move_runs_success_check() {
    let host = scene(false);
    let mut signaled = FakeOutput::ok();
    signaled.exit = Exit::Signal(9);
    let gate = host.set_gate_output("mv", &["-T", RW_TMP_PATH, RW_CANONICAL_PATH], signaled);
    let join = run_until_blocked(&host, &gate);
    let blocked = gate.blocked();
    if blocked {
        kernel_side_rename_done(&host);
    }
    gate.release();
    assert!(blocked, "rename did not reach the gate");
    assert_eq!(join.join().unwrap(), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Succeeded);
    assert_eq!(state.checks.len(), 1);
    assert!(state.checks[0].passed);
    assert_eq!(state.replace[0].readback_ok, Some(true));
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(NEW_BYTES));
}

#[test]
fn uncertain_signaled_rename_with_signaled_rollback_stays_running() {
    let host = scene(false);
    let mut signaled = FakeOutput::ok();
    signaled.exit = Exit::Signal(9);
    let gate = host.set_gate_output("mv", &["-T", RW_TMP_PATH, RW_CANONICAL_PATH], signaled);
    let join = run_until_blocked(&host, &gate);
    let blocked = gate.blocked();
    if blocked {
        // The temp-path lookup cannot establish whether the interrupted move happened. A
        // non-directory parent makes lstat return ENOTDIR.
        let parent = RW_TMP_PATH.rsplit_once('/').unwrap().0;
        host.remove_tree(parent);
        host.put_text(parent, "not a directory");
        host.preset(
            "modprobe",
            &["nvidia"],
            FakeOutput {
                exit: Exit::Signal(15),
                ..FakeOutput::ok()
            },
        );
    }
    gate.release();
    assert!(blocked, "mv did not reach the fake-host gate");
    assert_eq!(join.join().unwrap(), std::process::ExitCode::FAILURE);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Running);
    assert_eq!(state.finished_at, None);
    assert!(state.error.is_none());
    assert_eq!(state.replace[0].unloaded, ["nvidia"]);
    assert_eq!(state.replace[0].reloaded.len(), 1);
    assert_eq!(state.replace[0].reloaded[0].module, "nvidia");
    assert!(!state.replace[0].reloaded[0].ok);
    assert_eq!(
        state.replace[0].reloaded[0].reason,
        Some(task_state::ReasonJson::key(
            REASON_SIGNAL,
            serde_json::json!({"signal": 15})
        ))
    );
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
}

#[test]
fn restore_check_failure_stops_after_one_restore_pass() {
    for exit in [
        Exit::Code(1),
        Exit::Signal(9),
        Exit::TimedOut,
        Exit::SpawnFailed,
    ] {
        let host = scene(true);
        host.preset(
            "systemd-sysext",
            &["merge"],
            FakeOutput {
                exit,
                ..FakeOutput::ok()
            },
        );
        assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
        let state = persisted(&host);
        assert_eq!(state.status, Status::Failed, "{exit:?}");
        assert_eq!(state.error.unwrap().code, "RESTORE_CHECK_FAILED");
        assert_eq!(state.checks.len(), 2);
        assert!(!state.checks[0].passed);
        assert!(!state.checks[1].passed);
        assert_eq!(state.replace.len(), 2);
        assert_eq!(state.replace[0].refresh_exit, Some(exit.to_json()));
        assert_eq!(state.replace[1].refresh_exit, Some(exit.to_json()));
        assert_eq!(host.count("mv"), 2);
        assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
    }
}

#[test]
fn restore_with_no_before_merge_time_cannot_claim_stock_restored() {
    let host = scene(true);
    host.preset_once("systemd-sysext", &["merge"], FakeOutput::code(1));
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    assert_eq!(state.error.as_ref().unwrap().code, "RESTORE_CHECK_FAILED");
    assert_eq!(state.replace.len(), 2);
    assert_eq!(state.checks.len(), 2);
    assert!(!state.checks[0].refresh_ok);
    assert!(!state.checks[1].refresh_ok);
    assert!(state.checks[1].since_before.as_value().is_none());
    assert!(state.checks[1].since_after.as_value().is_some());
    assert_eq!(state.replace[0].refresh_exit, Some(Exit::Code(1).to_json()));
    assert_eq!(state.replace[1].refresh_exit, Some(Exit::Code(0).to_json()));
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
    assert_eq!(
        host.argv_log()
            .iter()
            .filter(|argv| argv.first().is_some_and(|p| p == "systemd-sysext")
                && argv.get(1).is_some_and(|arg| arg == "merge"))
            .count(),
        2
    );
}

#[test]
fn restore_with_unreadable_intermediate_status_still_merges_once_and_fails_check() {
    let host = scene(true);
    let update_check = host.set_gate_output(
        "nvidia-smi",
        &["--query-gpu=driver_version", "--format=csv,noheader"],
        FakeOutput::stdout("0.0\n"),
    );
    let mut join = run_until_blocked(&host, &update_check);
    assert!(update_check.blocked(), "update check was not reached");
    let restore_unmerge = host.set_gate("systemd-sysext", &["unmerge"]);
    host.preset_once(
        "nvidia-smi",
        &["--query-gpu=driver_version", "--format=csv,noheader"],
        FakeOutput::stdout("580.173.02\n"),
    );
    join.gates.push(restore_unmerge.clone());
    update_check.release();
    assert!(restore_unmerge.wait_blocked(join.join.as_ref().unwrap()));
    assert!(restore_unmerge.blocked(), "restore unmerge was not reached");
    host.preset_once(
        "systemd-sysext",
        &["status", "--json=short"],
        FakeOutput::stdout("not JSON"),
    );
    restore_unmerge.release();

    assert_eq!(join.join().unwrap(), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    assert_eq!(state.error.as_ref().unwrap().code, "RESTORE_CHECK_FAILED");
    assert_eq!(state.checks.len(), 2);
    assert!(state.checks[0].refresh_ok);
    assert!(!state.checks[0].version_ok);
    assert!(!state.checks[1].refresh_ok);
    assert!(state.checks[1].version_ok);
    assert_ne!(state.checks[1].since_before, state.checks[1].since_after);
    assert_eq!(state.replace[1].refresh_exit, Some(Exit::Code(0).to_json()));
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
    assert_eq!(
        host.argv_log()
            .iter()
            .filter(|argv| argv.first().is_some_and(|p| p == "systemd-sysext")
                && argv.get(1).is_some_and(|arg| arg == "merge"))
            .count(),
        2
    );
}

#[test]
fn restore_temp_failure_is_not_mistaken_for_update_failure() {
    let host = scene(true);
    let gate = host.set_gate("systemd-sysext", &["merge"]);
    let join = run_until_blocked(&host, &gate);
    assert!(gate.blocked(), "update merge did not reach the gate");
    host.fail_write(RW_TMP_PATH);
    gate.release();
    assert_eq!(join.join().unwrap(), std::process::ExitCode::SUCCESS);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Failed);
    let error = state.error.unwrap();
    assert_eq!(error.code, "RESTORE_NOT_WRITTEN");
    assert_eq!(error.params["cause"], "write_failed");
    assert_eq!(error.params["write_reason"]["key"], WRITE_TEMP_FAILED);
    assert_eq!(state.checks.len(), 1);
    assert_eq!(state.replace.len(), 2);
    assert_eq!(host.count("mv"), 1);
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(NEW_BYTES));
    assert_eq!(host.readonly(), "on");
}

#[test]
fn unpersistable_replace_record_never_unloads_a_module() {
    let host = scene(false);
    host.fail_write(&format!("{HOST_DATA_DIR}/manager/state/.{ID}.json.tmp-"));
    assert_eq!(run(&host), std::process::ExitCode::FAILURE);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Running);
    assert!(state.replace.is_empty());
    assert_eq!(host.count("rmmod"), 0);
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
}

fn build_ctx(host: &FakeHost) -> RunCtx {
    let plan = sample_plan(PlanSource::Upload {
        run_sha256: "a".repeat(64),
        fingerprint: "a".repeat(12),
        deb_sha256: std::collections::BTreeMap::new(),
    });
    RunCtx {
        host: Arc::new(host.clone()),
        paths: paths(),
        id: ID.into(),
        state: new_state(ID, 1, plan, Phase::Build),
        build_deadline: None,
    }
}

/// The build environment `runner::stage_build` would have prepared: these cases exercise the build
/// stage itself, not the pinned environment (which has its own file, `build_env.rs`).
fn build_env() -> runner::build_env::BuildEnv {
    runner::build_env::BuildEnv::new(
        format!("sha256:{}", "d".repeat(64)),
        false,
        vec!["/build-kit/binutils_2.40-2_amd64.deb".to_string()],
    )
}

#[test]
fn invalid_kernel_never_reaches_docker_mount_arguments() {
    for kernel in ["6.12.105,readonly=false", "6.12.105 bad", "6.12.105\nother"] {
        let host = FakeHost::new();
        host.set_uname(kernel);
        let failure = runner::build::build(&mut build_ctx(&host), &build_env()).unwrap_err();
        assert_eq!(failure.code.as_str(), "BUILD_FAILED");
        assert_eq!(failure.params["reason"]["key"], BUILD_DOCKER_RUN_FAILED);
        assert!(failure.raw.unwrap().contains(REASON_FORMAT_UNRECOGNIZED));
        assert_eq!(host.count("docker"), 0, "kernel={kernel:?}");
    }
}

#[test]
fn expired_build_budget_never_starts_a_container() {
    let host = FakeHost::new();
    let mut ctx = build_ctx(&host);
    ctx.build_deadline = Some(host.now() + std::time::Duration::from_secs(1));
    host.advance(std::time::Duration::from_secs(1));

    let failure = runner::build::build(&mut ctx, &build_env()).unwrap_err();
    assert_eq!(failure.code, ErrorCode::BuildTimeout);
    assert!(failure.command.is_none());
    assert!(failure.raw.is_none());
    assert_eq!(host.count("docker"), 0);
    let log = host
        .file_text(paths().state_log(ID).to_str().unwrap())
        .unwrap();
    assert!(log.contains("log.build_timeout"));
    assert!(!log.contains("log.build_container"));
}

#[test]
fn signaled_docker_run_reports_a_build_failure_for_stage_fifteen_cleanup() {
    let host = FakeHost::new();
    let mut signaled = FakeOutput::ok();
    signaled.exit = Exit::Signal(9);
    signaled.stdout = b"NVM:STEP:check\nprogress\n".to_vec();
    signaled.stderr = b"terminated\n".to_vec();
    host.preset_once("docker", &["run"], signaled);
    let failure = runner::build::build(&mut build_ctx(&host), &build_env()).unwrap_err();
    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(
        failure.params["reason"],
        serde_json::json!({"key": "reason.signal", "params": {"signal": 9}})
    );
    assert_eq!(failure.command.as_ref().unwrap().0[0], "docker");
    assert_eq!(failure.command.unwrap().1, Exit::Signal(9));
    assert!(failure.raw.unwrap().contains("terminated"));
    assert_eq!(host.count("docker"), 1);
}

#[test]
fn valid_newer_kernel_is_used_in_build_mounts() {
    let host = FakeHost::new();
    host.set_uname("6.12.120-production+truenas");
    let _ = runner::build::build(&mut build_ctx(&host), &build_env());
    let docker = host
        .commands()
        .into_iter()
        .find(|cmd| cmd.program == "docker")
        .expect("valid newer kernel reaches Docker");
    let argv = docker.argv().join(" ");
    assert!(
        argv.contains("/usr/lib/modules/6.12.120-production+truenas"),
        "{argv}"
    );
}

/// The build container runs the pinned build environment: the program's own embedded build script
/// inside the task's `in` directory, the installer as the entrypoint, the read-only offline package
/// folder, and the image the environment prepared (not the plan's own value).
#[test]
fn the_build_container_runs_the_pinned_environment() {
    let host = FakeHost::new();
    let env = build_env();
    let _ = runner::build::build(&mut build_ctx(&host), &env);
    let docker = host
        .commands()
        .into_iter()
        .find(|cmd| cmd.program == "docker")
        .expect("the build container is started");
    let argv = docker.argv();
    let text = argv.join(" ");
    assert!(text.contains("--pull never"), "{text}");
    assert!(text.contains("--platform linux/amd64"), "{text}");
    assert!(text.contains("--network none"), "{text}");
    assert!(text.contains("--cap-drop ALL"), "{text}");
    assert!(text.contains("no-new-privileges"), "{text}");
    assert!(!text.contains("--collect"), "{text}");
    assert!(
        text.contains(&format!(
            "type=bind,src={HOST_DATA_DIR}/manager/build-kit,dst=/build-kit,readonly"
        )),
        "{text}"
    );
    assert!(text.contains("dst=/in,readonly"), "{text}");
    assert!(
        text.contains("NVM_BUILD_KIT_LIST=/in/build-kit.list"),
        "{text}"
    );
    assert!(
        text.contains("NVM_BUILD_SCRIPT=/in/build-driver.sh"),
        "{text}"
    );
    // The last argument after `--entrypoint /bin/bash` is the installer; the argument before it is
    // the image, the ID the environment verified.
    assert_eq!(argv.last().unwrap(), "/in/install-build-kit.sh");
    assert_eq!(argv[argv.len() - 2], env.image_id);
    assert!(argv.contains(&"--entrypoint".to_string()) && argv.contains(&"/bin/bash".to_string()));
    // The build script really landed in the task's `in` directory first (the installer and the
    // package list are written by `build_env::acquire`, which has cases of its own).
    let in_dir = format!("{HOST_DATA_DIR}/manager/tasks/{ID}/in");
    assert!(host
        .file_text(&format!("{in_dir}/build-driver.sh"))
        .is_some_and(|script| script.contains("NVM:STEP:")));
}

/// An operation that does not compile (switching to an already built local row) must not touch the
/// build environment at all: no image query, no pull, no package download.
#[test]
fn a_local_switch_never_touches_the_build_environment() {
    let host = scene(false);
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    assert_eq!(persisted(&host).status, Status::Succeeded);
    let docker: Vec<Vec<String>> = host
        .commands()
        .into_iter()
        .filter(|cmd| cmd.program == "docker")
        .map(|cmd| cmd.args)
        .collect();
    for args in &docker {
        assert_ne!(args.first().map(String::as_str), Some("image"), "{args:?}");
        assert_ne!(args.first().map(String::as_str), Some("pull"), "{args:?}");
    }
    let snapshot = host.commands().into_iter().any(|cmd| {
        cmd.program == "curl"
            && cmd
                .args
                .iter()
                .any(|arg| arg.starts_with("https://snapshot.debian.org/"))
    });
    assert!(!snapshot, "a local switch downloaded build packages");
}

/// The terminal cleanup of a *settled* run: the complete exact container listing is taken and the
/// task directory is removed once recovery is no longer needed.
#[test]
fn a_settled_run_takes_the_container_listing_and_removes_the_task_dir() {
    let host = scene(false);
    let task_dir = format!("{HOST_DATA_DIR}/manager/tasks/{ID}");
    host.mkdir(&task_dir);
    host.put_text(&format!("{task_dir}/in/driver.run"), "the only copy");
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    assert_eq!(persisted(&host).status, Status::Succeeded);
    assert!(
        host.argv_log()
            .iter()
            .any(|a| a.len() > 3 && a[0] == "docker" && a[1] == "ps" && a[2] == "-a"),
        "the terminal cleanup judged the complete container listing"
    );
    assert!(
        !host.exists(&task_dir),
        "a settled task is cleaned up, inputs and all"
    );
}

/// Runs the gated certain-rename flow: `moved` models the kernel-side move while the rename command
/// is held, then the run must end unresolved.
fn certain_rename_is_not_success(moved: impl FnOnce(&FakeHost)) -> (FakeHost, TaskState) {
    let host = scene(false);
    let gate = host.set_gate_output(
        "mv",
        &["-T", RW_TMP_PATH, RW_CANONICAL_PATH],
        FakeOutput::ok(),
    );
    let join = run_until_blocked(&host, &gate);
    moved(&host);
    gate.release();
    assert!(gate.blocked(), "the rename did not reach the gate");
    assert_eq!(join.join().unwrap(), std::process::ExitCode::FAILURE);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Running, "{state:?}");
    assert!(truenas_nvidia_driver_manager::tasks::driver_settled(&host, &paths(), &state).is_err());
    (host, state)
}

/// A certain rename whose readback cannot be read is never success: the run stays unresolved and
/// the state keeps its recovery inputs.
#[test]
fn an_unreadable_readback_after_a_certain_rename_is_not_success() {
    let task_dir = format!("{HOST_DATA_DIR}/manager/tasks/{ID}");
    let (host, state) = certain_rename_is_not_success(|host| {
        host.put_text(&format!("{task_dir}/in/driver.run"), "the only copy");
        host.put_file(CANONICAL_PATH, NEW_BYTES.to_vec());
        host.set_listing(CANONICAL_PATH, &canonical_listing(KERNEL, "580.178.04"));
        host.remove(RW_CANONICAL_PATH);
        host.remove(RW_TMP_PATH);
    });
    assert_eq!(state.replace[0].readback_ok, None);
    assert!(host.exists(&format!("{task_dir}/in/driver.run")));
}

/// The same shape with a *readable* but different image at the readback path.
#[test]
fn a_mismatched_readback_after_a_certain_rename_is_not_success() {
    let (_host, state) = certain_rename_is_not_success(|host| {
        host.put_file(RW_CANONICAL_PATH, b"some other image".to_vec());
        host.put_file(CANONICAL_PATH, NEW_BYTES.to_vec());
        host.set_listing(CANONICAL_PATH, &canonical_listing(KERNEL, "580.178.04"));
        host.remove(RW_TMP_PATH);
    });
    assert_eq!(state.replace[0].readback_ok, Some(false));
}

/// The unit is started with a real `Type=exec` property: the runner's own process must be the
/// unit's main process, so a failed `exec` is visible instead of a forked child that never ran.
#[test]
fn the_task_unit_is_started_with_type_exec() {
    let host = scene(false);
    let started = tasks::start(&host, std::path::Path::new(HOST_DATA_DIR), ID);
    assert!(
        matches!(started, tasks::StartResult::Started),
        "{started:?}"
    );
    let argv = host
        .argv_log()
        .into_iter()
        .find(|a| a.first().map(String::as_str) == Some("systemd-run"))
        .expect("systemd-run was called");
    assert!(
        argv.iter().any(|a| a == "--property=Type=exec"),
        "the real argv carries the property: {argv:?}"
    );
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--" && w[1].ends_with("truenas-nvidia-driver-manager")),
        "{argv:?}"
    );
    assert!(
        argv.ends_with(&["task".to_string(), "run".to_string(), ID.to_string()]),
        "{argv:?}"
    );
}

/// A late runner B for the same id has zero side effects: no host command and no state write after
/// A started (or finished).
#[test]
fn a_late_runner_for_the_same_id_has_zero_side_effects() {
    // B after A already started: the persisted read-only stage is the proof.
    let host = scene(false);
    {
        use truenas_nvidia_driver_manager::store::JsonRead;
        let mut state = match task_state::read(&host, &paths(), ID) {
            JsonRead::Ok(state) => state,
            other => panic!("state: {other:?}"),
        };
        state.loaded_version_start = Some(
            truenas_nvidia_driver_manager::task_state::ReadingJson::value("580.173.02".to_string()),
        );
        task_state::write(&host, &paths(), &state).unwrap();
    }
    let before = host.argv_log().len();
    assert_eq!(run(&host), std::process::ExitCode::FAILURE);
    assert_eq!(
        host.argv_log().len(),
        before,
        "a duplicate runner runs no command at all"
    );

    // B after A finished: the terminal status is not RUNNING any more.
    let host = scene(false);
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let after = host.argv_log().len();
    assert_eq!(
        run(&host),
        std::process::ExitCode::FAILURE,
        "a finished task is never replayed"
    );
    assert_eq!(host.argv_log().len(), after, "and nothing is run again");
}

/// The first state write happens before any host side effect: when it fails, nothing is saved,
/// built, unloaded or cleaned up.
#[test]
fn a_failed_first_state_write_runs_no_host_side_effect() {
    let host = scene(false);
    host.fail_write(&format!("{HOST_DATA_DIR}/manager/state"));
    assert_eq!(run(&host), std::process::ExitCode::FAILURE);
    let argv = host.argv_log();
    // No download, no build, no unload, no write and no cleanup: the failed first state write stops
    // the run before the first host side effect.
    for forbidden in ["docker", "rmmod", "mv", "zfs", "modprobe", "curl"] {
        assert!(
            !argv
                .iter()
                .any(|a| a.first().map(String::as_str) == Some(forbidden)),
            "{forbidden} must not run: {argv:?}"
        );
    }
    // The read-only stage may still have run, but nothing changed on the host.
    assert!(host.file(CANONICAL_PATH).as_deref() == Some(OLD_BYTES));
    assert!(host.argv_log().iter().all(|a| {
        let p = a.first().map(String::as_str).unwrap_or("");
        !matches!(p, "docker" | "zfs" | "mv")
    }));
}

/// A runner takes only the execution lock: a control lock held by another writer does not stop the
/// launched unit.
#[test]
fn a_runner_takes_only_the_execution_lock() {
    let host = scene(false);
    let mut control = host
        .open_lock(
            std::path::Path::new("/run/nvm-admission/control.lock"),
            0o600,
        )
        .expect("the control lock opens");
    assert!(
        control.try_acquire().expect("the fixture lock works"),
        "the control lock is free to hold"
    );
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    assert_eq!(persisted(&host).status, Status::Succeeded);
    control.release().expect("the control lock releases");
}

// ---- the pre-delivery artifact check requires regular files ---------------

/// The reason of an artifact the check cannot read.
const UNREADABLE: &str = "artifact.unreadable";
/// A call that must return without touching a node that can block gets this much wall-clock time; a
/// stuck call is not waited out here.
const BUDGET: Duration = Duration::from_secs(2);

fn running() -> TaskState {
    new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Build)
}

fn ctx(host: &FakeHost, state: TaskState) -> RunCtx {
    RunCtx {
        host: Arc::new(host.clone()),
        paths: paths(),
        id: ID.to_string(),
        state,
        build_deadline: None,
    }
}

fn task_out() -> String {
    paths()
        .task_dir(ID)
        .join("out")
        .to_string_lossy()
        .into_owned()
}

fn reason_key(failure: &BuildFailure) -> &str {
    failure.params["reason"]["key"].as_str().unwrap_or_default()
}

/// Run the real `check_artifact` on a worker thread and hand back its outcome, or `None` when it
/// did not finish inside `BUDGET`. A check that blocks on the node it was given is the failure this
/// bounded wait reports; the worker may stay blocked until the test process exits.
fn check_within(host: &FakeHost) -> Option<Result<Artifact, BuildFailure>> {
    let host = host.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut ctx = ctx(&host, running());
        let _ = tx.send(check_artifact(&mut ctx, KERNEL));
    });
    rx.recv_timeout(BUDGET).ok()
}

/// A symlink at `out/nvidia.raw` is refused, and its target is never hashed: the target holds a
/// different byte string than `result.txt` records, so following the link would end in a hash
/// mismatch instead.
#[test]
fn a_symlinked_artifact_is_refused_and_not_hashed() {
    let host = default_host();
    let out = task_out();
    let payload = b"not the built image";
    host.put_file(&format!("{out}/payload.bin"), payload.to_vec());
    host.put_link(&format!("{out}/nvidia.raw"), &format!("{out}/payload.bin"));
    host.put_text(
        &format!("{out}/result.txt"),
        &format!(
            "ARTIFACT_SHA256={}\nARTIFACT_BYTES={}\n",
            "0".repeat(64),
            payload.len()
        ),
    );

    let outcome = check_within(&host).expect("the check of a symlink must return");
    let failure = outcome.expect_err("a symlinked artifact is not accepted");
    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(reason_key(&failure), UNREADABLE);
    assert_eq!(
        failure.raw.as_deref(),
        Some("nvidia.raw is not a regular file")
    );
    // The node was judged by `lstat`; nothing opened, read or hashed it.
    let ops = host.ops_for(&format!("{out}/nvidia.raw"));
    assert!(ops.contains(&"lstat"), "no lstat of the node: {ops:?}");
    assert!(
        !ops.iter().any(|kind| *kind == "read" || *kind == "open"),
        "the symlinked node was read: {ops:?}"
    );
}

/// A FIFO at `out/nvidia.raw` is refused; reading it would block forever.
#[test]
fn a_fifo_artifact_is_refused_without_blocking() {
    let host = default_host();
    let out = task_out();
    host.put_text(
        &format!("{out}/result.txt"),
        &format!("ARTIFACT_SHA256={}\nARTIFACT_BYTES=5\n", "0".repeat(64)),
    );
    host.put_fifo(&format!("{out}/nvidia.raw"));

    let outcome = check_within(&host).expect("the check of a FIFO must return");
    let failure = outcome.expect_err("a FIFO artifact is not accepted");
    assert_eq!(reason_key(&failure), UNREADABLE);
    assert_eq!(
        failure.raw.as_deref(),
        Some("nvidia.raw is not a regular file")
    );
}

/// A FIFO at `out/result.txt` is refused the same way instead of blocking the record read.
#[test]
fn a_fifo_result_record_is_refused_without_blocking() {
    let host = default_host();
    let out = task_out();
    host.put_file(&format!("{out}/nvidia.raw"), b"payload".to_vec());
    host.put_fifo(&format!("{out}/result.txt"));

    let outcome = check_within(&host).expect("the check of a FIFO must return");
    let failure = outcome.expect_err("a FIFO record is not accepted");
    assert_eq!(reason_key(&failure), UNREADABLE);
    assert_eq!(
        failure.raw.as_deref(),
        Some("result.txt is not a regular file")
    );
}

/// Two regular files still pass the whole check valid.
#[test]
fn regular_artifact_files_still_pass_the_check() {
    let host = default_host();
    let mut ctx = ctx(&host, running());
    let out = ctx.task_dir().join("out");
    let payload = b"built image";
    host.put_file(out.join("nvidia.raw").to_str().unwrap(), payload.to_vec());
    host.put_text(
        out.join("result.txt").to_str().unwrap(),
        &format!(
            "ARTIFACT_SHA256={}\nARTIFACT_BYTES={}\n",
            sha256_hex(payload),
            payload.len()
        ),
    );
    let ko = format!("usr/lib/modules/{KERNEL}/kernel/drivers/video/nvidia.ko");
    let listing = format!(
        "drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr\n\
         drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr/lib/modules/{KERNEL}\n\
         -rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/{ko}\n\
         -rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.580.178.04\n\
         -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-ctk\n\
         -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime\n\
         -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime-hook\n"
    );
    host.set_listing(out.join("nvidia.raw").to_str().unwrap(), &listing);
    host.set_modinfo(
        &format!("{HOST_DATA_DIR}/manager/tasks/{ID}/inspect/{ko}"),
        "580.178.04",
    );

    let artifact = check_artifact(&mut ctx, KERNEL).expect("the regular artifact passes");
    assert_eq!(artifact.sha256, sha256_hex(payload));
    assert_eq!(artifact.size, payload.len() as u64);
    assert_eq!(artifact.version, "580.178.04");
    assert_eq!(artifact.target_kernel, KERNEL);
}

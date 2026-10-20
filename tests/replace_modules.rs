//! Module handling, exercised only through the injected Host.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::facts::{self, ReloadBasis};
use truenas_nvidia_driver_manager::host::Exit;
use truenas_nvidia_driver_manager::runner::replace::{self, ModuleError, ModuleFailure};
use truenas_nvidia_driver_manager::runner::RunCtx;
use truenas_nvidia_driver_manager::store::{JsonRead, Paths, Validate};
use truenas_nvidia_driver_manager::task_state::{
    self, new_state, Pass, Phase, PlanSource, ReloadTargetsJson, Status, TaskState,
};

const ID: &str = "20260928T101530Z-3f9a0c12";

fn targets(basis: ReloadBasis, modules: &[&str]) -> ReloadTargetsJson {
    ReloadTargetsJson {
        basis: basis.as_str().into(),
        modules: modules.iter().map(|m| m.to_string()).collect(),
    }
}

fn context(host: &FakeHost, reload_targets: Option<ReloadTargetsJson>) -> RunCtx {
    let paths = Paths::new(HOST_DATA_DIR);
    let mut state = new_state(
        ID,
        1,
        sample_plan(PlanSource::Local {
            row_id: "20260928T101530Z-3f9a0c12-build".into(),
            row_kind: task_state::RowKind::Official,
            fingerprint: None,
            sha256: "a".repeat(64),
        }),
        Phase::ReadLoadedVersion,
    );
    state.reload_targets = reload_targets;
    task_state::write(host, &paths, &state).unwrap();
    RunCtx {
        host: Arc::new(host.clone()),
        paths,
        id: ID.into(),
        state,
        build_deadline: None,
    }
}

fn persisted(host: &FakeHost) -> TaskState {
    match task_state::read(host, &Paths::new(HOST_DATA_DIR), ID) {
        JsonRead::Ok(state) => {
            state.validate().unwrap();
            state
        }
        other => panic!("state was not valid: {other:?}"),
    }
}

fn log(host: &FakeHost) -> String {
    host.file_text(&format!("{HOST_DATA_DIR}/manager/state/{ID}.log"))
        .unwrap()
}

fn occupancy_params(host: &FakeHost) -> serde_json::Value {
    log(host)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|line| line["key"] == "log.occupancy")
        .unwrap()["p"]
        .clone()
}

fn approved(error: ModuleError) -> ModuleFailure {
    match error {
        ModuleError::Failure(failure) => failure,
        other => panic!("expected an approved failure, got {other:?}"),
    }
}

fn argv(host: &FakeHost) -> Vec<Vec<String>> {
    host.argv_log()
        .into_iter()
        .filter(|cmd| cmd[0] == "rmmod" || cmd[0] == "modprobe")
        .collect()
}

#[test]
fn busy_modules_stop_both_passes_without_rmmod() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        host.load_module("nvidia", 2, &["nvidia_uvm"]);
        host.load_module("nvidia_uvm", 0, &[]);
        let mut ctx = context(
            &host,
            Some(targets(
                ReloadBasis::LoadedAtStart,
                &["nvidia", "nvidia_uvm"],
            )),
        );
        let started = std::time::Instant::now();
        let failure = approved(replace::unload(&mut ctx, pass).unwrap_err());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the occupancy wait must use the fake clock, elapsed {:?}",
            started.elapsed()
        );
        let state = persisted(&host);
        assert!(state.replace[0].unload_waits.is_empty());
        assert_eq!(state.replace[0].occupancy_waits.len(), 1);
        assert_eq!(
            state.replace[0].occupancy_waits[0].result,
            task_state::OccupancyWaitResult::StillInUse
        );
        assert!(
            state.replace[0].occupancy_waits[0].waited_ms >= 30_000,
            "{}",
            state.replace[0].occupancy_waits[0].waited_ms
        );
        assert!(!log(&host).contains("log.module_unload_wait"));
        assert!(log(&host).contains("log.occupancy_wait"));
        assert!(log(&host).contains("\"result\":\"still_in_use\""));
        assert_eq!(
            failure.code,
            if pass == Pass::Update {
                ErrorCode::GpuInUse
            } else {
                ErrorCode::RestoreAborted
            }
        );
        assert_eq!(
            failure.params,
            if pass == Pass::Update {
                json!({"occupancy": "busy"})
            } else {
                json!({"cause": "busy"})
            }
        );
        assert!(failure.command.is_none());
        assert_eq!(host.count("rmmod"), 0);
        assert_eq!(host.count("modprobe"), 0);
        let state = persisted(&host);
        assert_eq!(state.status, Status::Running);
        assert_eq!(state.finished_at, None);
        assert_eq!(state.replace[0].pass, pass);
        assert!(state.replace[0].unloaded.is_empty());
        assert!(log(&host).contains("\"usage_result\":\"busy\""));
    }
}

#[test]
fn unreadable_or_malformed_proc_modules_never_unloads() {
    for pass in [Pass::Update, Pass::Restore] {
        for malformed in [false, true] {
            let host = FakeHost::new();
            host.load_module("nvidia", 0, &[]);
            if malformed {
                host.put_text(PROC_MODULES_PATH, "nvidia bad-format\n");
            } else {
                host.fail_read(PROC_MODULES_PATH);
            }
            let mut ctx = context(
                &host,
                Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
            );
            let started = std::time::Instant::now();
            let failure = approved(replace::unload(&mut ctx, pass).unwrap_err());
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "an unreadable reading must not wait, elapsed {:?}",
                started.elapsed()
            );
            let reason = if malformed {
                REASON_FORMAT_UNRECOGNIZED
            } else {
                REASON_PERMISSION_DENIED
            };
            assert_eq!(
                failure.params,
                if pass == Pass::Update {
                    json!({"occupancy": "unreadable", "read_reason": {"key": reason, "params": {}}})
                } else {
                    json!({"cause": "unreadable", "read_reason": {"key": reason, "params": {}}})
                }
            );
            assert_eq!(host.count("rmmod"), 0);
            assert!(log(&host).contains("\"usage_result\":\"unreadable\""));
            assert!(!log(&host).contains("log.occupancy_wait"));
            assert!(persisted(&host).replace[0].occupancy_waits.is_empty());
            assert_eq!(persisted(&host).status, Status::Running);
        }
    }
}

#[test]
fn a_deadline_inside_the_occupancy_wait_is_not_an_occupancy_result() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.sustain_open("nvidia", Duration::ZERO, Duration::from_secs(1_000));
    // The first `/proc/modules` read is the occupancy read, after the 600s deadline is captured.
    host.advance_on_proc_read(1, Duration::from_secs(590));
    let mut ctx = context(
        &host,
        Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
    );
    let started = std::time::Instant::now();
    let failure = approved(replace::unload(&mut ctx, Pass::Update).unwrap_err());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(failure.code, ErrorCode::ReplaceDeadlineExceeded);
    assert_eq!(host.count("rmmod"), 0);
    assert!(!log(&host).contains("log.occupancy_wait"));
    assert!(persisted(&host).replace[0].occupancy_waits.is_empty());
}

#[test]
fn an_unreadable_reading_during_the_occupancy_wait_keeps_the_unreadable_result() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.sustain_open("nvidia", Duration::ZERO, Duration::from_secs(10));
    host.on_wait({
        let host = host.clone();
        move || {
            host.put_text(PROC_MODULES_PATH, "not a module line\n");
        }
    });
    let mut ctx = context(
        &host,
        Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
    );
    let failure = approved(replace::unload(&mut ctx, Pass::Update).unwrap_err());
    assert_eq!(failure.params["occupancy"], json!("unreadable"));
    assert_eq!(host.count("rmmod"), 0);
    assert!(!log(&host).contains("log.occupancy_wait"));
    assert!(persisted(&host).replace[0].occupancy_waits.is_empty());
}

#[test]
fn all_five_unload_in_fixed_order_then_reload_original_targets() {
    let host = FakeHost::new();
    host.put_text(CANONICAL_PATH, "stock image");
    for module in facts::DRIVER_MODULES {
        host.load_module(module, 0, &[]);
    }
    host.clear_ops();
    let original = [
        "nvidia",
        "nvidia_uvm",
        "nvidia_peermem",
        "nvidia_modeset",
        "nvidia_drm",
    ];
    let mut ctx = context(&host, Some(targets(ReloadBasis::LoadedAtStart, &original)));
    let current = replace::unload(&mut ctx, Pass::Update).unwrap();
    assert_eq!(current.pass, Pass::Update);
    assert_eq!(
        argv(&host),
        vec![
            vec!["rmmod", "nvidia_drm"],
            vec!["rmmod", "nvidia_modeset"],
            vec!["rmmod", "nvidia_peermem"],
            vec!["rmmod", "nvidia_uvm"],
            vec!["rmmod", "nvidia"],
        ]
    );
    assert!(host
        .commands()
        .iter()
        .all(|c| c.at == truenas_nvidia_driver_manager::host::At::Host));
    assert!(host
        .commands()
        .iter()
        .all(|c| c.timeout > Duration::ZERO && c.timeout <= REPLACE_DEADLINE));
    assert_eq!(
        persisted(&host).replace[0].unloaded,
        facts::UNLOAD_ORDER.map(str::to_string)
    );
    let readings = occupancy_params(&host)["module_readings"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(readings.len(), 5);
    assert!(readings
        .iter()
        .all(|r| r["key"] == "log.module_readings.entry"));
    assert!(readings.iter().all(|r| r["params"]["refcount"] == 0));
    replace::reload_targets(&mut ctx, &current).unwrap();
    let state = persisted(&host);
    assert_eq!(state.status, Status::Running);
    assert_eq!(state.finished_at, None);
    assert_eq!(state.phase, Some(Phase::Reload));
    assert_eq!(
        state.replace[0]
            .reloaded
            .iter()
            .map(|r| r.module.as_str())
            .collect::<Vec<_>>(),
        original
    );
    assert!(state.replace[0]
        .reloaded
        .iter()
        .all(|r| r.ok && r.reason.is_none()));
    assert_eq!(
        host.file_text(CANONICAL_PATH).as_deref(),
        Some("stock image")
    );
    assert!(!host.any_op_for(CANONICAL_PATH, "create_truncate"));
    assert!(!host.any_op_for(CANONICAL_PATH, "rename"));
    assert_eq!(host.count("sh"), 0);
}

#[test]
fn refusal_rolls_back_only_actual_unloads_and_continues_after_reload_failure() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        for module in ["nvidia_drm", "nvidia_modeset", "nvidia_uvm"] {
            host.load_module(module, 0, &[]);
        }
        host.refuse_unload("nvidia_uvm");
        host.fail_reload("nvidia_modeset");
        let mut ctx = context(
            &host,
            Some(targets(
                ReloadBasis::LoadedAtStart,
                &["nvidia_uvm", "nvidia_modeset", "nvidia_drm"],
            )),
        );
        let failure = approved(replace::unload(&mut ctx, pass).unwrap_err());
        assert_eq!(
            failure.code,
            if pass == Pass::Update {
                ErrorCode::ModuleUnloadRefused
            } else {
                ErrorCode::RestoreAborted
            }
        );
        assert_eq!(failure.params["module"], "nvidia_uvm");
        assert_eq!(
            failure.params["unload_reason"]["stderr"],
            "rmmod: ERROR: Module nvidia_uvm is in use by: holder"
        );
        if pass == Pass::Restore {
            assert_eq!(failure.params["cause"], "unload_refused");
        }
        assert_eq!(
            failure.command,
            Some((vec!["rmmod".into(), "nvidia_uvm".into()], Exit::Code(1)))
        );
        assert!(failure.raw.unwrap().contains("rmmod: ERROR"));
        let commands = argv(&host);
        assert_eq!(commands[0], vec!["rmmod", "nvidia_drm"]);
        assert_eq!(commands[1], vec!["rmmod", "nvidia_modeset"]);
        assert!(
            commands[2..commands.len() - 2]
                .iter()
                .all(|cmd| cmd == &vec!["rmmod".to_string(), "nvidia_uvm".to_string()]),
            "{commands:?}"
        );
        assert_eq!(
            commands[commands.len() - 2],
            vec!["modprobe", "nvidia_modeset"]
        );
        assert_eq!(commands[commands.len() - 1], vec!["modprobe", "nvidia_drm"]);
        let state = persisted(&host);
        let waits = &state.replace[0].unload_waits;
        assert_eq!(waits.len(), 1);
        assert_eq!(waits[0].module, "nvidia_uvm");
        assert_eq!(waits[0].result, task_state::UnloadWaitResult::Refused);
        assert!(waits[0].waited_ms >= 30_000, "{}", waits[0].waited_ms);
        assert!(waits[0].retries >= 1);
        assert_eq!(state.replace[0].unloaded, ["nvidia_drm", "nvidia_modeset"]);
        assert_eq!(state.replace[0].reloaded.len(), 2);
        assert!(!state.replace[0].reloaded[0].ok);
        assert!(state.replace[0].reloaded[0].reason.is_some());
        assert!(state.replace[0].reloaded[1].ok);
        assert_eq!(state.status, Status::Running);
        assert!(log(&host).contains("log.module_unload_refused"));
        assert!(log(&host).contains("log.module_reload_failed"));
        assert!(log(&host).contains("log.module_reloaded"));
    }
}

#[test]
fn timed_out_unload_uses_deadline_code_and_rolls_back() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        host.load_module("nvidia_drm", 0, &[]);
        host.load_module("nvidia_modeset", 0, &[]);
        let gate = host.set_gate("rmmod", &["nvidia_modeset"]);
        let _release_gate = ReleaseGate(gate.clone());
        let thread_host = host.clone();
        let handle = std::thread::spawn(move || {
            let mut ctx = context(
                &thread_host,
                Some(targets(
                    ReloadBasis::LoadedAtStart,
                    &["nvidia_modeset", "nvidia_drm"],
                )),
            );
            let result = replace::unload(&mut ctx, pass);
            (ctx, result)
        });
        let reached_gate = gate.wait_blocked(&handle);
        assert!(
            reached_gate,
            "second rmmod did not reach the fake-host gate"
        );
        host.advance(REPLACE_DEADLINE + Duration::from_secs(1));
        let (ctx, result) = handle.join().unwrap();
        let failure = approved(result.unwrap_err());
        assert_eq!(
            failure.code,
            if pass == Pass::Update {
                ErrorCode::ReplaceDeadlineExceeded
            } else {
                ErrorCode::RestoreNotWritten
            }
        );
        assert_eq!(failure.params["deadline"], REPLACE_DEADLINE_MINUTES);
        if pass == Pass::Restore {
            assert_eq!(failure.params["cause"], "deadline");
        }
        assert_eq!(failure.command.unwrap().1, Exit::TimedOut);
        assert_eq!(
            argv(&host),
            vec![
                vec!["rmmod", "nvidia_drm"],
                vec!["rmmod", "nvidia_modeset"],
                vec!["modprobe", "nvidia_modeset"],
                vec!["modprobe", "nvidia_drm"],
            ]
        );
        let state = persisted(&host);
        assert_eq!(state.replace[0].unloaded, ["nvidia_drm"]);
        assert_eq!(state.replace[0].reloaded[0].module, "nvidia_modeset");
        assert_eq!(state.replace[0].reloaded[1].module, "nvidia_drm");
        assert_eq!(state.status, Status::Running);
        assert!(ctx.state.error.is_none());
        assert!(log(&host).contains("log.deadline_exceeded"));
    }
}

#[test]
fn timed_out_command_keeps_its_real_exit_and_output() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.preset_once(
        "rmmod",
        &["nvidia"],
        FakeOutput {
            exit: Exit::TimedOut,
            ..FakeOutput::stderr("rmmod: partial diagnostic")
        },
    );
    let mut ctx = context(
        &host,
        Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
    );
    let failure = approved(replace::unload(&mut ctx, Pass::Update).unwrap_err());
    assert_eq!(failure.code, ErrorCode::ReplaceDeadlineExceeded);
    assert_eq!(failure.command.unwrap().1, Exit::TimedOut);
    assert!(failure.raw.unwrap().contains("rmmod: partial diagnostic"));
    assert_eq!(host.count("modprobe"), 1);
}

#[test]
fn empty_initial_modules_use_default_targets() {
    let host = FakeHost::new();
    let planned = facts::reload_targets(Some(&[])).unwrap();
    let mut ctx = context(&host, Some(ReloadTargetsJson::from(&planned)));
    let current = replace::unload(&mut ctx, Pass::Update).unwrap();
    assert_eq!(host.count("rmmod"), 0);
    assert_eq!(
        ctx.state.reload_targets.as_ref().unwrap().basis,
        "none_loaded_at_start"
    );
    assert_eq!(
        occupancy_params(&host)["module_readings"]["key"],
        "log.module_readings.none"
    );
    replace::reload_targets(&mut ctx, &current).unwrap();
    assert_eq!(
        argv(&host),
        vec![
            vec!["modprobe", "nvidia"],
            vec!["modprobe", "nvidia_drm"],
            vec!["modprobe", "nvidia_modeset"],
        ]
    );
}

#[test]
fn unknown_initial_reading_derives_targets_after_update_unload() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.load_module("nvidia_drm", 0, &[]);
    let mut ctx = context(&host, None);
    replace::unload(&mut ctx, Pass::Update).unwrap();
    let targets = persisted(&host).reload_targets.unwrap();
    assert_eq!(targets.basis, "unloaded_in_update");
    assert_eq!(targets.modules, ["nvidia", "nvidia_drm"]);
    assert_eq!(host.count("rmmod"), 2);

    let empty_host = FakeHost::new();
    let mut empty_ctx = context(&empty_host, None);
    replace::unload(&mut empty_ctx, Pass::Update).unwrap();
    let targets = persisted(&empty_host).reload_targets.unwrap();
    assert_eq!(targets.basis, "unloaded_in_update");
    assert_eq!(targets.modules, facts::DEFAULT_RELOAD);
    assert_eq!(empty_host.count("rmmod"), 0);
}

#[test]
fn restore_reuses_update_targets_even_when_no_module_is_loaded() {
    let host = FakeHost::new();
    let planned = targets(ReloadBasis::LoadedAtStart, &["nvidia", "nvidia_uvm"]);
    let mut ctx = context(&host, Some(planned.clone()));
    let current = replace::unload(&mut ctx, Pass::Restore).unwrap();
    assert_eq!(host.count("rmmod"), 0);
    replace::reload_targets(&mut ctx, &current).unwrap();
    assert_eq!(
        argv(&host),
        vec![vec!["modprobe", "nvidia"], vec!["modprobe", "nvidia_uvm"]]
    );
    assert_eq!(persisted(&host).reload_targets, Some(planned));
    assert_eq!(persisted(&host).replace[0].pass, Pass::Restore);
    assert_eq!(persisted(&host).phase, Some(Phase::RestoreReload));
}

#[test]
fn later_prewrite_stop_reloads_actual_unloads_outside_deadline() {
    let host = FakeHost::new();
    host.load_module("nvidia_drm", 0, &[]);
    host.load_module("nvidia_modeset", 0, &[]);
    let mut ctx = context(
        &host,
        Some(targets(
            ReloadBasis::LoadedAtStart,
            &["nvidia_modeset", "nvidia_drm"],
        )),
    );
    let current = replace::unload(&mut ctx, Pass::Update).unwrap();
    host.advance(REPLACE_DEADLINE + Duration::from_secs(1));
    host.preset_once(
        "modprobe",
        &["nvidia_modeset"],
        FakeOutput {
            exit: Exit::TimedOut,
            ..FakeOutput::ok()
        },
    );
    replace::rollback_unloaded(&mut ctx, &current);
    assert_eq!(
        argv(&host),
        vec![
            vec!["rmmod", "nvidia_drm"],
            vec!["rmmod", "nvidia_modeset"],
            vec!["modprobe", "nvidia_modeset"],
            vec!["modprobe", "nvidia_drm"],
        ]
    );
    let state = persisted(&host);
    assert_eq!(state.replace[0].reloaded.len(), 2);
    assert_eq!(
        state.replace[0].reloaded[0].reason,
        Some(task_state::ReasonJson::key(
            REASON_TIMED_OUT,
            json!({"seconds": HOST_CMD_TIMEOUT.as_secs()})
        ))
    );
    assert!(state.replace[0].reloaded[1].ok);
    assert_eq!(state.status, Status::Running);
    assert!(host
        .commands()
        .iter()
        .filter(|c| c.program == "modprobe")
        .all(|c| c.timeout == HOST_CMD_TIMEOUT));
}

#[test]
fn restore_without_original_targets_does_not_unload() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    let mut ctx = context(&host, None);
    assert!(matches!(
        replace::unload(&mut ctx, Pass::Restore),
        Err(ModuleError::MissingReloadTargets)
    ));
    assert_eq!(host.count("rmmod"), 0);
    assert_eq!(persisted(&host).status, Status::Running);
}

#[test]
fn signaled_unload_without_stderr_preserves_signal_reason_and_exit() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.preset_once(
        "rmmod",
        &["nvidia"],
        FakeOutput {
            exit: Exit::Signal(9),
            ..FakeOutput::ok()
        },
    );
    let mut ctx = context(
        &host,
        Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
    );
    let failure = approved(replace::unload(&mut ctx, Pass::Update).unwrap_err());
    assert_eq!(failure.code, ErrorCode::ModuleUnloadRefused);
    assert_eq!(
        failure.params["unload_reason"],
        json!({"key": "reason.signal", "params": {"signal": 9}})
    );
    assert_eq!(failure.command.as_ref().unwrap().1, Exit::Signal(9));
    failure.record(&mut ctx);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Running);
    assert_eq!(state.error.unwrap().exit, Some(json!({"signal": 9})));
    assert_eq!(state.replace[0].reloaded[0].module, "nvidia");
    assert!(log(&host).contains("\"signal\":9"));
}

#[test]
fn signal_with_stderr_preserves_signal_exit_and_verbatim_reason() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.preset_once(
        "rmmod",
        &["nvidia"],
        FakeOutput {
            exit: Exit::Signal(15),
            ..FakeOutput::stderr("rmmod interrupted")
        },
    );
    let mut ctx = context(
        &host,
        Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
    );
    let failure = approved(replace::unload(&mut ctx, Pass::Update).unwrap_err());
    assert_eq!(
        failure.params["unload_reason"],
        json!({"stderr": "rmmod interrupted"})
    );
    assert_eq!(failure.command.unwrap().1, Exit::Signal(15));
    assert_eq!(host.count("modprobe"), 1);
}

#[test]
fn rollback_signal_keeps_primary_refusal_and_records_failed_reload() {
    let host = FakeHost::new();
    host.load_module("nvidia_drm", 0, &[]);
    host.load_module("nvidia_modeset", 0, &[]);
    host.refuse_unload("nvidia_modeset");
    host.preset_once(
        "modprobe",
        &["nvidia_drm"],
        FakeOutput {
            exit: Exit::Signal(15),
            ..FakeOutput::ok()
        },
    );
    let mut ctx = context(
        &host,
        Some(targets(
            ReloadBasis::LoadedAtStart,
            &["nvidia_modeset", "nvidia_drm"],
        )),
    );
    let failure = approved(replace::unload(&mut ctx, Pass::Update).unwrap_err());
    assert_eq!(failure.code, ErrorCode::ModuleUnloadRefused);
    assert_eq!(failure.params["module"], "nvidia_modeset");
    assert_eq!(failure.command.as_ref().unwrap().1, Exit::Code(1));
    failure.record(&mut ctx);
    assert_eq!(host.count("modprobe"), 1);
    let state = persisted(&host);
    assert_eq!(state.replace[0].unloaded, ["nvidia_drm"]);
    assert_eq!(state.error.unwrap().exit, Some(json!({"code": 1})));
    assert_eq!(state.replace[0].reloaded[0].module, "nvidia_drm");
    assert!(!state.replace[0].reloaded[0].ok);
    assert_eq!(
        state.replace[0].reloaded[0].reason,
        Some(task_state::ReasonJson::key(
            "reason.signal",
            json!({"signal": 15})
        ))
    );
}

#[test]
fn write_failure_survives_a_signaled_rollback_and_reloads_remaining_modules() {
    let host = FakeHost::new();
    host.put_text(CANONICAL_PATH, "stock image");
    host.load_module("nvidia_drm", 0, &[]);
    host.load_module("nvidia_modeset", 0, &[]);
    let mut ctx = context(
        &host,
        Some(targets(
            ReloadBasis::LoadedAtStart,
            &["nvidia_modeset", "nvidia_drm"],
        )),
    );
    let current = replace::unload(&mut ctx, Pass::Update).unwrap();
    host.preset_once(
        "modprobe",
        &["nvidia_modeset"],
        FakeOutput {
            exit: Exit::Signal(15),
            ..FakeOutput::ok()
        },
    );
    let failure = approved(replace::write(&mut ctx, &current, None).unwrap_err());
    assert_eq!(failure.code, ErrorCode::WriteFailed);
    assert_eq!(
        failure.params["write_reason"]["key"],
        WRITE_SOURCE_UNREADABLE
    );
    failure.record(&mut ctx);
    let state = persisted(&host);
    assert_eq!(state.error.unwrap().code, "WRITE_FAILED");
    assert_eq!(state.replace[0].reloaded.len(), 2);
    assert_eq!(state.replace[0].reloaded[0].module, "nvidia_modeset");
    assert_eq!(
        state.replace[0].reloaded[0].reason,
        Some(task_state::ReasonJson::key(
            "reason.signal",
            json!({"signal": 15})
        ))
    );
    assert_eq!(state.replace[0].reloaded[1].module, "nvidia_drm");
    assert!(state.replace[0].reloaded[1].ok);
    assert_eq!(
        host.file_text(CANONICAL_PATH).as_deref(),
        Some("stock image")
    );
}

#[test]
fn command_exit_is_real_and_failure_record_remains_running() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.preset_once("rmmod", &["nvidia"], FakeOutput::code(17));
    let mut ctx = context(
        &host,
        Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
    );
    let failure = approved(replace::unload(&mut ctx, Pass::Update).unwrap_err());
    assert_eq!(
        failure.params["unload_reason"],
        json!({"key": REASON_EXIT_STATUS, "params": {"code": 17}})
    );
    assert_eq!(
        failure.command,
        Some((vec!["rmmod".into(), "nvidia".into()], Exit::Code(17)))
    );
    assert!(ctx.state.replace[0].unload_waits.is_empty());
    failure.record(&mut ctx);
    assert_eq!(ctx.state.status, Status::Running);
    assert_eq!(ctx.state.finished_at, None);
    let state = persisted(&host);
    assert_eq!(state.status, Status::Running);
    assert_eq!(
        state.error.as_ref().unwrap().exit,
        Some(json!({"code": 17}))
    );
    assert_eq!(
        state.error.as_ref().unwrap().command,
        Some(vec!["rmmod".into(), "nvidia".into()])
    );
}

fn correction_context(host: &FakeHost, pass: Pass) -> (RunCtx, replace::ModulePass) {
    host.put_text(CANONICAL_PATH, "target image");
    host.set_listing(CANONICAL_PATH, &canonical_listing(KERNEL, "580.178.04"));
    host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH);
    host.load_module("nvidia", 0, &[]);
    let mut ctx = context(
        host,
        Some(targets(
            ReloadBasis::LoadedAtStart,
            &["nvidia", "nvidia_modeset", "nvidia_drm"],
        )),
    );
    let current = replace::unload(&mut ctx, pass).unwrap();
    host.load_module("nvidia", 0, &[]);
    host.load_module("nvidia_uvm", 0, &[]);
    host.model_sysext_merge();
    ctx.state.replace[0].file_changed = Some(true);
    ctx.state.replace[0].readback_ok = Some(true);
    assert!(ctx.write_state());
    host.clear_ops();
    host.clear_presets_and_commands();
    (ctx, current)
}

#[test]
fn correction_requires_persisted_entry_and_stops_after_a_failed_action_write() {
    for after_unload in [false, true] {
        let host = FakeHost::new();
        let (mut ctx, current) = correction_context(&host, Pass::Update);
        if after_unload {
            // Entry + current module snapshot persist, the first actual unload cannot persist.
            host.fail_rename_to_suffix_after(&format!("{ID}.json"), 2);
        } else {
            host.fail_state_phase("align_modules");
        }
        assert!(matches!(
            replace::align_modules(&mut ctx, &current, "580.178.04"),
            Err(ModuleError::StateNotPersisted)
        ));
        assert_eq!(host.count("rmmod"), usize::from(after_unload));
        assert_eq!(host.count("modprobe"), 0);
        assert_eq!(persisted(&host).status, Status::Running);
        if after_unload {
            assert!(ctx.state.replace[0]
                .module_correction
                .as_ref()
                .unwrap()
                .unloads[0]
                .reason
                .is_none());
        }
    }
}

#[test]
fn correction_partial_unload_core_refusal_waits_then_stops_without_reload() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        let (mut ctx, current) = correction_context(&host, pass);
        // Invisible to the occupancy read at offset 0, then held for longer than the wait.
        host.sustain_open("nvidia", Duration::from_millis(1), Duration::from_secs(120));
        host.unload_takes("nvidia", Duration::from_millis(1));
        let started = std::time::Instant::now();
        replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        let state = persisted(&host);
        let correction = state.replace[0].module_correction.as_ref().unwrap();
        assert_eq!(
            state.phase,
            Some(if pass == Pass::Update {
                Phase::AlignModules
            } else {
                Phase::RestoreAlignModules
            })
        );
        assert_eq!(correction.modules_before, ["nvidia_uvm", "nvidia"]);
        assert_eq!(
            correction
                .unloads
                .iter()
                .map(|a| a.module.as_str())
                .collect::<Vec<_>>(),
            ["nvidia_uvm", "nvidia"]
        );
        assert_eq!(correction.waits.len(), 1);
        assert_eq!(correction.waits[0].module, "nvidia");
        assert_eq!(
            correction.waits[0].result,
            task_state::UnloadWaitResult::Refused
        );
        assert_eq!(correction.waits[0].retries, 0);
        assert!(
            correction.waits[0].waited_ms >= 30_000,
            "{}",
            correction.waits[0].waited_ms
        );
        assert_eq!(host.count("sleep"), 0);
        assert_eq!(host.count("modprobe"), 0);
        assert!(correction.reloaded.is_empty());
        assert!(correction.stop_reason.is_some());
        assert!(correction.version_after.is_some());
        assert_eq!(state.replace[0].unloaded, ["nvidia"]);
        assert!(host
            .commands()
            .iter()
            .all(|c| c.timeout == HOST_CMD_TIMEOUT));
        // A second invocation is never a second corrective round.
        replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
        assert_eq!(host.count("rmmod"), 2);
    }
}

#[test]
fn correction_handles_missing_but_does_not_unload_unparseable_or_unreadable_versions() {
    use truenas_nvidia_driver_manager::task_state::ModuleCorrectionTrigger;
    for mode in ["missing", "unparseable", "unreadable"] {
        let host = FakeHost::new();
        let (mut ctx, current) = correction_context(&host, Pass::Update);
        match mode {
            "missing" => host.remove(NVRM_VERSION_PATH),
            "unparseable" => host.put_text(NVRM_VERSION_PATH, "unrecognized kernel banner"),
            _ => host.fail_read(NVRM_VERSION_PATH),
        }
        replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
        let correction = &ctx.state.replace[0].module_correction;
        if mode == "missing" {
            assert_eq!(
                correction.as_ref().unwrap().trigger,
                ModuleCorrectionTrigger::VersionMissing
            );
            assert_eq!(host.count("rmmod"), 2);
        } else {
            assert!(correction.is_none());
            assert_eq!(host.count("rmmod"), 0);
        }
    }
}

#[test]
fn correction_checks_current_occupancy_and_unknown_module_lists_without_unloading() {
    for mode in ["busy", "unreadable", "malformed"] {
        let host = FakeHost::new();
        let (mut ctx, current) = correction_context(&host, Pass::Update);
        match mode {
            "busy" => host.open_device("nvidia"),
            "unreadable" => host.fail_read(PROC_MODULES_PATH),
            _ => host.put_text(PROC_MODULES_PATH, "malformed"),
        }
        replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
        assert_eq!(host.count("rmmod"), 0);
        assert_eq!(host.count("modprobe"), 0);
        assert!(ctx.state.replace[0]
            .module_correction
            .as_ref()
            .unwrap()
            .stop_reason
            .is_some());
    }
}

#[test]
fn correction_uses_command_timeouts_after_the_write_deadline_and_never_adds_uvm() {
    let host = FakeHost::new();
    let (mut ctx, current) = correction_context(&host, Pass::Update);
    host.advance(REPLACE_DEADLINE + Duration::from_secs(1));
    replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
    let record = &persisted(&host).replace[0];
    let correction = record.module_correction.as_ref().unwrap();
    assert_eq!(
        correction
            .reloaded
            .iter()
            .map(|r| r.module.as_str())
            .collect::<Vec<_>>(),
        ["nvidia", "nvidia_modeset", "nvidia_drm"]
    );
    assert!(correction.reloaded.iter().all(|r| r.ok));
    assert!(ctx.state.error.is_none());
    assert!(!host
        .argv_log()
        .iter()
        .any(|a| a == &["modprobe", "nvidia_uvm"]));
    assert!(host
        .commands()
        .iter()
        .all(|c| c.timeout == HOST_CMD_TIMEOUT));
}

#[test]
fn core_already_correct_and_same_file_do_not_trigger_correction() {
    for file_changed in [Some(true), Some(false), None] {
        let host = FakeHost::new();
        let (mut ctx, current) = correction_context(&host, Pass::Update);
        host.set_driver_version("580.178.04");
        if file_changed != Some(true) {
            ctx.state.replace[0].module_version_before = Some(task_state::ReadingJson::value(
                String::from_utf8(
                    truenas_nvidia_driver_manager::host::Host::read(
                        &host,
                        std::path::Path::new(NVRM_VERSION_PATH),
                    )
                    .unwrap(),
                )
                .unwrap(),
            ));
            ctx.state.replace[0].file_changed = file_changed;
        }
        replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
        assert!(ctx.state.replace[0].module_correction.is_none());
        assert_eq!(host.count("rmmod"), 0);
    }
}

#[test]
fn correction_state_rejects_unknown_module_names_and_inconsistent_results() {
    let host = FakeHost::new();
    let (mut ctx, current) = correction_context(&host, Pass::Update);
    replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
    let state = ctx.state;
    state.validate().unwrap();
    for surface in [
        "snapshot",
        "unload",
        "reload",
        "unload_result",
        "reload_result",
    ] {
        let mut bad = state.clone();
        let correction = bad.replace[0].module_correction.as_mut().unwrap();
        match surface {
            "snapshot" => correction.modules_before.push("other_module".into()),
            "unload" => correction.unloads[0].module = "other_module".into(),
            "reload" => correction.reloaded[0].module = "other_module".into(),
            "unload_result" => {
                correction.unloads[0].reason =
                    Some(task_state::ReasonJson::stderr("impossible success"))
            }
            _ => correction.reloaded[0].ok = false,
        }
        assert!(bad.validate().is_err(), "{surface}");
    }
}

#[test]
fn correction_non_occupancy_failures_stop_without_retries_or_prewrite_rollback() {
    for exit in [
        Exit::TimedOut,
        Exit::Signal(9),
        Exit::SpawnFailed,
        Exit::Code(1),
    ] {
        let host = FakeHost::new();
        let (mut ctx, current) = correction_context(&host, Pass::Update);
        host.preset_once(
            "rmmod",
            &["nvidia_uvm"],
            FakeOutput {
                exit,
                ..FakeOutput::stderr("different failure")
            },
        );
        let started = std::time::Instant::now();
        replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(host.count("rmmod"), 1);
        assert_eq!(host.count("sleep"), 0);
        assert_eq!(host.count("modprobe"), 0);
        assert!(ctx.state.replace[0]
            .module_correction
            .as_ref()
            .unwrap()
            .waits
            .is_empty());
        assert_eq!(
            ctx.state.replace[0]
                .module_correction
                .as_ref()
                .unwrap()
                .unloads[0]
                .exit,
            exit.to_json()
        );
        assert!(ctx.state.error.is_none());
    }
}

#[test]
fn a_transient_occupancy_refusal_retries_then_finishes_the_single_round() {
    let host = FakeHost::new();
    let (mut ctx, current) = correction_context(&host, Pass::Update);
    host.preset_once(
        "rmmod",
        &["nvidia"],
        FakeOutput::code(1).with_stderr("rmmod: Module nvidia is in use"),
    );
    replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
    let correction = ctx.state.replace[0].module_correction.as_ref().unwrap();
    assert_eq!(
        correction
            .unloads
            .iter()
            .map(|attempt| attempt.module.as_str())
            .collect::<Vec<_>>(),
        ["nvidia_uvm", "nvidia", "nvidia"]
    );
    assert_eq!(host.count("sleep"), 0);
    assert_eq!(correction.waits.len(), 1);
    assert_eq!(correction.waits[0].module, "nvidia");
    assert_eq!(correction.waits[0].retries, 1);
    assert_eq!(
        correction.waits[0].result,
        task_state::UnloadWaitResult::Unloaded
    );
    assert!(correction.waits[0].waited_ms < 5_000);
    assert!(correction.stop_reason.is_none());
    assert!(correction.reloaded.iter().all(|r| r.ok));
    assert!(correction
        .version_after
        .as_ref()
        .unwrap()
        .as_value()
        .unwrap()
        .contains("580.178.04"));
}

#[test]
fn correction_core_reload_failure_still_attempts_every_frozen_target_before_f11() {
    let host = FakeHost::new();
    let (mut ctx, current) = correction_context(&host, Pass::Update);
    host.preset_once(
        "modprobe",
        &["nvidia"],
        FakeOutput::code(1).with_stderr("core load failed"),
    );
    replace::align_modules(&mut ctx, &current, "580.178.04").unwrap();
    let c = ctx.state.replace[0].module_correction.as_ref().unwrap();
    assert_eq!(
        c.reloaded
            .iter()
            .map(|r| r.module.as_str())
            .collect::<Vec<_>>(),
        ["nvidia", "nvidia_modeset", "nvidia_drm"]
    );
    assert!(!c.reloaded[0].ok);
    assert!(c.reloaded[1..].iter().all(|r| r.ok));
    let checked = truenas_nvidia_driver_manager::runner::check::check(
        &mut ctx,
        Pass::Update,
        (
            facts::Reading::Value(1),
            facts::Reading::Value(2),
            None,
            None,
            true,
        ),
        "580.178.04",
    );
    assert!(!checked.passed);
    assert!(!checked.version_ok);
}

fn stock(host: &FakeHost) {
    host.put_text(CANONICAL_PATH, "stock driver image");
}

fn assert_stock(host: &FakeHost) {
    assert_eq!(
        host.file_text(CANONICAL_PATH).as_deref(),
        Some("stock driver image")
    );
    assert_eq!(host.count("mv"), 0);
}

fn transient_nvidia(host: &FakeHost) {
    host.load_module("nvidia", 0, &[]);
    host.sustain_open(
        "nvidia",
        Duration::from_millis(1),
        Duration::from_millis(400),
    );
    host.unload_takes("nvidia", Duration::from_millis(1));
}

/// The fake host keeps the last matching preset. Register the retry first and the in-use
/// answer second, so the first `rmmod` is the in-use refusal and the second is `retry`.
fn script_retry(host: &FakeHost, retry: FakeOutput) {
    host.preset_once("rmmod", &["nvidia"], retry);
    host.preset_once(
        "rmmod",
        &["nvidia"],
        FakeOutput::code(1).with_stderr("rmmod: ERROR: Module nvidia is in use"),
    );
}

#[test]
fn wait_success_state_failure_reloads_the_unloaded_module() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        stock(&host);
        transient_nvidia(&host);
        let mut ctx = context(
            &host,
            Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
        );
        host.fail_rename_to_suffix_after(&format!("{ID}.json"), 2);
        let result = replace::unload(&mut ctx, pass);
        assert!(
            matches!(result, Err(ModuleError::StateNotPersisted)),
            "pass={pass:?} result={result:?} argv={:?} unloaded={:?}",
            argv(&host),
            ctx.state.replace[0].unloaded
        );
        assert_eq!(
            argv(&host),
            vec![
                vec!["rmmod", "nvidia"],
                vec!["rmmod", "nvidia"],
                vec!["modprobe", "nvidia"],
            ]
        );
        assert_eq!(ctx.state.replace[0].unloaded, ["nvidia"]);
        assert_eq!(
            host.displayed_refcount_at("nvidia", Duration::ZERO),
            Some(0)
        );
        assert_stock(&host);
    }
}

#[test]
fn module_missing_during_wait_state_failure_reloads_it() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        stock(&host);
        transient_nvidia(&host);
        // Occupancy is the first reading. The wait's first poll is the second.
        host.drop_module_on_proc_read("nvidia", 2);
        let mut ctx = context(
            &host,
            Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
        );
        host.fail_rename_to_suffix_after(&format!("{ID}.json"), 2);
        let result = replace::unload(&mut ctx, pass);
        assert!(
            matches!(result, Err(ModuleError::StateNotPersisted)),
            "pass={pass:?} result={result:?} argv={:?} unloaded={:?}",
            argv(&host),
            ctx.state.replace[0].unloaded
        );
        assert_eq!(
            argv(&host),
            vec![vec!["rmmod", "nvidia"], vec!["modprobe", "nvidia"]]
        );
        assert_eq!(ctx.state.replace[0].unloaded, ["nvidia"]);
        assert_eq!(
            host.displayed_refcount_at("nvidia", Duration::ZERO),
            Some(0)
        );
        assert_stock(&host);
    }
}

#[test]
fn wait_success_state_failure_reloads_in_reverse_order() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        stock(&host);
        host.load_module("nvidia_drm", 0, &[]);
        transient_nvidia(&host);
        let mut ctx = context(
            &host,
            Some(targets(
                ReloadBasis::LoadedAtStart,
                &["nvidia", "nvidia_drm"],
            )),
        );
        // Phase, replacement record, then the drm unload. The wait record is the one refused.
        host.fail_rename_to_suffix_after(&format!("{ID}.json"), 3);
        let result = replace::unload(&mut ctx, pass);
        assert!(
            matches!(result, Err(ModuleError::StateNotPersisted)),
            "pass={pass:?} result={result:?} argv={:?} unloaded={:?}",
            argv(&host),
            ctx.state.replace[0].unloaded
        );
        assert_eq!(
            argv(&host),
            vec![
                vec!["rmmod", "nvidia_drm"],
                vec!["rmmod", "nvidia"],
                vec!["rmmod", "nvidia"],
                vec!["modprobe", "nvidia"],
                vec!["modprobe", "nvidia_drm"],
            ]
        );
        assert_eq!(
            ctx.state.replace[0].unloaded,
            ["nvidia_drm".to_string(), "nvidia".to_string()]
        );
        assert_stock(&host);
    }
}

#[test]
fn retry_timeout_or_signal_state_failure_reloads_the_uncertain_module() {
    for pass in [Pass::Update, Pass::Restore] {
        for exit in [Exit::TimedOut, Exit::Signal(9)] {
            let host = FakeHost::new();
            stock(&host);
            host.load_module("nvidia", 0, &[]);
            script_retry(
                &host,
                FakeOutput {
                    exit,
                    ..FakeOutput::ok()
                },
            );
            let mut ctx = context(
                &host,
                Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
            );
            host.fail_rename_to_suffix_after(&format!("{ID}.json"), 2);
            let result = replace::unload(&mut ctx, pass);
            assert!(
                matches!(result, Err(ModuleError::StateNotPersisted)),
                "pass={pass:?} exit={exit:?} result={result:?} argv={:?} unloaded={:?}",
                argv(&host),
                ctx.state.replace[0].unloaded
            );
            assert!(
                ctx.state.replace[0].unloaded.is_empty(),
                "an uncertain rmmod is not a confirmed unload: {:?}",
                ctx.state.replace[0].unloaded
            );
            assert_eq!(
                argv(&host),
                vec![
                    vec!["rmmod", "nvidia"],
                    vec!["rmmod", "nvidia"],
                    vec!["modprobe", "nvidia"],
                ]
            );
            assert_stock(&host);
        }
    }
}

#[test]
fn retry_refusal_state_failure_leaves_the_loaded_module() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        stock(&host);
        host.load_module("nvidia", 0, &[]);
        script_retry(
            &host,
            FakeOutput::code(1).with_stderr("rmmod: ERROR: Invalid argument"),
        );
        let mut ctx = context(
            &host,
            Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
        );
        host.fail_rename_to_suffix_after(&format!("{ID}.json"), 2);
        let result = replace::unload(&mut ctx, pass);
        assert!(
            matches!(result, Err(ModuleError::StateNotPersisted)),
            "pass={pass:?} result={result:?} argv={:?}",
            argv(&host)
        );
        assert!(ctx.state.replace[0].unloaded.is_empty());
        assert_eq!(
            argv(&host),
            vec![vec!["rmmod", "nvidia"], vec!["rmmod", "nvidia"]]
        );
        assert_eq!(
            host.displayed_refcount_at("nvidia", Duration::ZERO),
            Some(0)
        );
        assert_stock(&host);
    }
}

#[test]
fn deadline_during_wait_state_failure_leaves_the_loaded_module() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        stock(&host);
        host.load_module("nvidia", 0, &[]);
        // Invisible to the occupancy read, then held across the whole replacement deadline.
        // The first rmmod itself advances the fake clock onto that deadline.
        host.sustain_open(
            "nvidia",
            Duration::from_millis(1),
            REPLACE_DEADLINE + Duration::from_secs(1),
        );
        host.unload_takes("nvidia", REPLACE_DEADLINE);
        let mut ctx = context(
            &host,
            Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
        );
        host.fail_rename_to_suffix_after(&format!("{ID}.json"), 2);
        let result = replace::unload(&mut ctx, pass);
        assert!(
            matches!(result, Err(ModuleError::StateNotPersisted)),
            "pass={pass:?} result={result:?} argv={:?} unloaded={:?}",
            argv(&host),
            ctx.state.replace[0].unloaded
        );
        assert!(ctx.state.replace[0].unloaded.is_empty());
        assert_eq!(argv(&host), vec![vec!["rmmod", "nvidia"]]);
        assert_eq!(host.count("modprobe"), 0);
        assert!(host
            .displayed_refcount_at("nvidia", Duration::from_secs(600))
            .is_some());
        assert_stock(&host);
    }
}

#[test]
fn correction_wait_record_failure_does_not_reload() {
    for pass in [Pass::Update, Pass::Restore] {
        let host = FakeHost::new();
        let (mut ctx, current) = correction_context(&host, pass);
        host.unload_module("nvidia_uvm");
        host.sustain_open(
            "nvidia",
            Duration::from_millis(1),
            Duration::from_millis(400),
        );
        host.unload_takes("nvidia", Duration::from_millis(1));
        // Entry, occupancy snapshot, the in-use attempt, and the successful retry attempt.
        // The wait record is the write that fails.
        host.fail_rename_to_suffix_after(&format!("{ID}.json"), 4);
        let result = replace::align_modules(&mut ctx, &current, "580.178.04");
        let correction = ctx.state.replace[0].module_correction.as_ref().unwrap();
        assert!(
            matches!(result, Err(ModuleError::StateNotPersisted)),
            "pass={pass:?} result={result:?} rmmod={} modprobe={} waits={} unloads={}",
            host.count("rmmod"),
            host.count("modprobe"),
            correction.waits.len(),
            correction.unloads.len()
        );
        assert_eq!(correction.waits.len(), 1);
        assert_eq!(correction.unloads.len(), 2);
        assert_eq!(host.count("rmmod"), 2);
        assert_eq!(host.count("modprobe"), 0);
        assert_eq!(
            host.file_text(CANONICAL_PATH).as_deref(),
            Some("target image")
        );
        assert_eq!(host.count("mv"), 0);
    }
}

/// Occupancy wait record is saved and the unload continues while the task is still running.
/// The `waiting_gpu` marker from that wait must already be gone.
#[test]
fn wait_marker_is_cleared_before_the_next_running_step() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.sustain_open("nvidia", Duration::ZERO, Duration::from_secs(4));
    let mut ctx = context(
        &host,
        Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
    );
    replace::unload(&mut ctx, Pass::Update).unwrap();
    let state = persisted(&host);
    assert_eq!(state.status, Status::Running);
    assert_eq!(state.phase, Some(Phase::Unload));
    assert!(
        state.replace[0].occupancy_waits[0].waited_ms >= 4_000,
        "{}",
        state.replace[0].occupancy_waits[0].waited_ms
    );
    assert_eq!(state.replace[0].unloaded, ["nvidia"]);
    assert!(
        state.waiting_gpu.is_none(),
        "wait marker leaked into the next running step"
    );
}

/// #254: `rmmod` is refused while the occupancy reading is already idle. After that wait is
/// recorded and the module is unloaded, the task is still running and `waiting_gpu` is gone.
#[test]
fn rmmod_in_use_wait_marker_is_cleared_before_the_next_running_step() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    // Idle at the occupancy read. The first `rmmod` steps onto a 4s hold, past the 3s
    // marker, then the reference count drops and the retry unloads.
    host.sustain_open("nvidia", Duration::from_millis(1), Duration::from_secs(4));
    host.unload_takes("nvidia", Duration::from_millis(1));
    let mut ctx = context(
        &host,
        Some(targets(ReloadBasis::LoadedAtStart, &["nvidia"])),
    );
    replace::unload(&mut ctx, Pass::Update).unwrap();
    let state = persisted(&host);
    assert_eq!(state.status, Status::Running);
    assert_eq!(state.phase, Some(Phase::Unload));
    assert!(state.replace[0].occupancy_waits.is_empty());
    let wait = &state.replace[0].unload_waits[0];
    assert_eq!(wait.module, "nvidia");
    assert_eq!(wait.result, task_state::UnloadWaitResult::Unloaded);
    assert!(wait.retries >= 1, "retries {}", wait.retries);
    assert!(wait.waited_ms >= 4_000, "waited_ms {}", wait.waited_ms);
    assert_eq!(state.replace[0].unloaded, ["nvidia"]);
    assert!(
        state.waiting_gpu.is_none(),
        "wait marker leaked into the next running step"
    );
}

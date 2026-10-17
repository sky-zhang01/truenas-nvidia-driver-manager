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
        let failure = approved(replace::unload(&mut ctx, pass).unwrap_err());
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
            let failure = approved(replace::unload(&mut ctx, pass).unwrap_err());
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
            assert_eq!(persisted(&host).status, Status::Running);
        }
    }
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
        assert_eq!(
            argv(&host),
            vec![
                vec!["rmmod", "nvidia_drm"],
                vec!["rmmod", "nvidia_modeset"],
                vec!["rmmod", "nvidia_uvm"],
                vec!["modprobe", "nvidia_modeset"],
                vec!["modprobe", "nvidia_drm"],
            ]
        );
        let state = persisted(&host);
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

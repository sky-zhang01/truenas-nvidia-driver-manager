//! Module handling shared by the update and restore replacement passes.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::{io, io::Read};

use serde_json::{json, Value};

use crate::codes::*;
use crate::drivers;
use crate::facts;
use crate::host::{At, Cmd, Create, Exit, Output};
use crate::runner::{original::OriginalRow, RunCtx};
use crate::store::{self, JsonRead};
use crate::task_state::{
    Pass, Phase, ReadingJson, ReasonJson, ReloadTargetsJson, ReloadedRecord, ReplaceRecord, Rootfs,
};

/// The pass record and deadline used by the later write and reload steps.
#[derive(Debug, Clone, Copy)]
pub struct ModulePass {
    pub pass: Pass,
    pub record_index: usize,
    pub deadline: Instant,
}

/// A failure that can be represented by the approved task-state contract.
#[derive(Debug, Clone)]
pub struct ModuleFailure {
    pub code: ErrorCode,
    pub params: Value,
    pub raw: Option<String>,
    pub command: Option<(Vec<String>, Exit)>,
}

#[derive(Debug, Clone)]
pub enum ModuleError {
    Failure(ModuleFailure),
    MissingReloadTargets,
    WriteOutcomeUnknown,
    StateNotPersisted,
}

impl From<ModuleFailure> for ModuleError {
    fn from(failure: ModuleFailure) -> ModuleError {
        ModuleError::Failure(failure)
    }
}

impl ModuleFailure {
    /// Keep the task RUNNING until the final observations are complete.
    pub fn record(self, ctx: &mut RunCtx) {
        ctx.fail_with_command(self.code, self.params, self.raw, self.command);
        let _ = ctx.write_state();
    }
}

fn record(pass: Pass) -> ReplaceRecord {
    ReplaceRecord {
        pass,
        unloaded: Vec::new(),
        reloaded: Vec::new(),
        refresh_exit: None,
        ldconfig_exit: None,
        readback_ok: None,
        readonly_after: None,
    }
}

fn read_reason(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => REASON_FILE_MISSING,
        std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
        _ => REASON_IO_ERROR,
    }
}

fn module_readings(modules: &[facts::ModuleLine]) -> Value {
    let readings = modules
        .iter()
        .filter(|module| module.name.starts_with("nvidia"))
        .map(|module| {
            json!({
                "key": "log.module_readings.entry",
                "params": {
                    "module": module.name,
                    "refcount": module.refcount,
                    "dependents": module.deps.len(),
                }
            })
        })
        .collect::<Vec<_>>();
    if readings.is_empty() {
        json!({"key": "log.module_readings.none", "params": {}})
    } else {
        json!(readings)
    }
}

fn occupancy_failure(pass: Pass, reason: Option<&'static str>) -> ModuleFailure {
    let (code, params) = match (pass, reason) {
        (Pass::Update, None) => (ErrorCode::GpuInUse, json!({"occupancy": "busy"})),
        (Pass::Restore, None) => (ErrorCode::RestoreAborted, json!({"cause": "busy"})),
        (Pass::Update, Some(key)) => (
            ErrorCode::GpuInUse,
            json!({"occupancy": "unreadable", "read_reason": {"key": key, "params": {}}}),
        ),
        (Pass::Restore, Some(key)) => (
            ErrorCode::RestoreAborted,
            json!({"cause": "unreadable", "read_reason": {"key": key, "params": {}}}),
        ),
    };
    ModuleFailure {
        code,
        params,
        raw: None,
        command: None,
    }
}

fn deadline_failure(
    pass: Pass,
    command: Option<(Vec<String>, Exit)>,
    raw: Option<String>,
) -> ModuleFailure {
    let (code, params) = match pass {
        Pass::Update => (
            ErrorCode::ReplaceDeadlineExceeded,
            json!({"deadline": REPLACE_DEADLINE_MINUTES}),
        ),
        Pass::Restore => (
            ErrorCode::RestoreNotWritten,
            json!({"cause": "deadline", "deadline": REPLACE_DEADLINE_MINUTES}),
        ),
    };
    ModuleFailure {
        code,
        params,
        raw,
        command,
    }
}

fn command_reason(out: &Output, timeout: std::time::Duration) -> ReasonJson {
    match out.exit {
        Exit::TimedOut => ReasonJson::key(REASON_TIMED_OUT, json!({"seconds": timeout.as_secs()})),
        Exit::SpawnFailed => ReasonJson::key(REASON_SPAWN_FAILED, json!({})),
        _ if !out.stderr.is_empty() => ReasonJson::stderr(out.stderr_text()),
        Exit::Code(code) => ReasonJson::key(REASON_EXIT_STATUS, json!({"code": code})),
        Exit::Signal(signal) => ReasonJson::key(REASON_SIGNAL, json!({"signal": signal})),
    }
}

fn command_raw(out: &Output) -> Option<String> {
    let raw = out.raw_tail(ERROR_RAW_LINES);
    if raw.trim().is_empty() {
        None
    } else {
        Some(raw)
    }
}

fn refusal_failure(
    pass: Pass,
    module: &str,
    reason: &ReasonJson,
    cmd: &Cmd,
    out: &Output,
) -> ModuleFailure {
    let (code, params) = match pass {
        Pass::Update => (
            ErrorCode::ModuleUnloadRefused,
            json!({"module": module, "unload_reason": reason}),
        ),
        Pass::Restore => (
            ErrorCode::RestoreAborted,
            json!({"cause": "unload_refused", "module": module, "unload_reason": reason}),
        ),
    };
    ModuleFailure {
        code,
        params,
        raw: command_raw(out),
        command: Some((cmd.argv(), out.exit)),
    }
}

fn reload(ctx: &mut RunCtx, index: usize, modules: &[String]) {
    for module in modules {
        let cmd = Cmd::new(At::Host, "modprobe", HOST_CMD_TIMEOUT).arg(module);
        let out = ctx.cmd(&cmd);
        if out.exit.ok() {
            ctx.state.replace[index].reloaded.push(ReloadedRecord {
                module: module.clone(),
                ok: true,
                reason: None,
            });
            ctx.log(LogKey::ModuleReloaded, json!({"module": module}));
        } else {
            let reason = command_reason(&out, cmd.timeout);
            ctx.state.replace[index].reloaded.push(ReloadedRecord {
                module: module.clone(),
                ok: false,
                reason: Some(reason.clone()),
            });
            ctx.log(
                LogKey::ModuleReloadFailed,
                json!({"module": module, "raw": reason}),
            );
        }
        let _ = ctx.write_state();
    }
}

/// Start a replacement pass by checking occupancy and unloading modules. A failure rolls back
/// successfully unloaded modules.
pub fn unload(ctx: &mut RunCtx, pass: Pass) -> Result<ModulePass, ModuleError> {
    let phase = match pass {
        Pass::Update => Phase::Unload,
        Pass::Restore => Phase::RestoreUnload,
    };
    ctx.enter(phase);
    let index = ctx.state.replace.len();
    ctx.state.replace.push(record(pass));
    if !ctx.write_state() {
        return Err(ModuleError::StateNotPersisted);
    }
    if pass == Pass::Restore && ctx.state.reload_targets.is_none() {
        return Err(ModuleError::MissingReloadTargets);
    }
    // The window starts with the occupancy reading, not with the preceding phase and
    // replace-record writes.
    let deadline = ctx.host.now() + REPLACE_DEADLINE;
    let current = ModulePass {
        pass,
        record_index: index,
        deadline,
    };

    let reading = match ctx.host.read(Path::new(PROC_MODULES_PATH)) {
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) => facts::parse_proc_modules(text).map_err(|_| REASON_FORMAT_UNRECOGNIZED),
            Err(_) => Err(REASON_FORMAT_UNRECOGNIZED),
        },
        Err(error) => Err(read_reason(&error)),
    };
    let modules = match reading {
        Ok(modules) => modules,
        Err(reason) => {
            ctx.log(
                LogKey::Occupancy,
                json!({
                    "usage_result": "unreadable",
                    "module_readings": {"key": "log.usage.unreadable", "params": {}},
                }),
            );
            if ctx.host.now() >= deadline {
                return stop_at_deadline(ctx, &current, None, None, None);
            }
            return Err(ModuleError::Failure(occupancy_failure(pass, Some(reason))));
        }
    };
    let busy = facts::occupancy(&modules);
    ctx.log(
        LogKey::Occupancy,
        json!({
            "usage_result": if busy { "busy" } else { "not_in_use" },
            "module_readings": module_readings(&modules),
        }),
    );
    if ctx.host.now() >= deadline {
        return stop_at_deadline(ctx, &current, None, None, None);
    }
    if busy {
        return Err(ModuleError::Failure(occupancy_failure(pass, None)));
    }

    for module in facts::unload_plan(&modules) {
        let remaining = deadline.saturating_duration_since(ctx.host.now());
        if remaining.is_zero() {
            return stop_at_deadline(ctx, &current, None, None, None);
        }
        let cmd = Cmd::new(At::Host, "rmmod", remaining).arg(module);
        let out = ctx.cmd(&cmd);
        if out.exit == Exit::TimedOut {
            return stop_at_deadline(
                ctx,
                &current,
                Some((cmd.argv(), out.exit)),
                command_raw(&out),
                Some(module),
            );
        }
        if matches!(out.exit, Exit::Signal(_)) {
            let reason = command_reason(&out, cmd.timeout);
            ctx.log(
                LogKey::ModuleUnloadRefused,
                json!({"module": module, "unload_reason": reason}),
            );
            return after_rollback(
                ctx,
                &current,
                refusal_failure(pass, module, &reason, &cmd, &out),
                Some(module),
            );
        }
        if out.exit.ok() {
            ctx.state.replace[index].unloaded.push(module.to_string());
            if !ctx.write_state() {
                rollback_unloaded(ctx, &current);
                return Err(ModuleError::StateNotPersisted);
            }
            ctx.log(LogKey::ModuleUnloaded, json!({"module": module}));
        }
        if !out.exit.ok() {
            let reason = command_reason(&out, cmd.timeout);
            ctx.log(
                LogKey::ModuleUnloadRefused,
                json!({"module": module, "unload_reason": reason}),
            );
            let failure = refusal_failure(pass, module, &reason, &cmd, &out);
            return after_rollback(ctx, &current, failure, None);
        }
        if ctx.host.now() >= deadline {
            return stop_at_deadline(ctx, &current, None, None, None);
        }
    }
    if ctx.host.now() >= deadline {
        return stop_at_deadline(ctx, &current, None, None, None);
    }
    if pass == Pass::Update && ctx.state.reload_targets.is_none() {
        let unloaded = &ctx.state.replace[index].unloaded;
        let names = unloaded.iter().map(String::as_str).collect::<Vec<_>>();
        let targets = facts::reload_targets_from_unloaded(&names);
        ctx.state.reload_targets = Some(ReloadTargetsJson::from(&targets));
        if !ctx.write_state() {
            rollback_unloaded(ctx, &current);
            return Err(ModuleError::StateNotPersisted);
        }
    }
    Ok(current)
}

fn after_rollback(
    ctx: &mut RunCtx,
    current: &ModulePass,
    failure: ModuleFailure,
    uncertain_module: Option<&str>,
) -> Result<ModulePass, ModuleError> {
    if let Some(module) = uncertain_module {
        rollback_with_uncertain(ctx, current, module)
    } else {
        rollback_unloaded(ctx, current)
    };
    Err(ModuleError::Failure(failure))
}

fn stop_at_deadline(
    ctx: &mut RunCtx,
    current: &ModulePass,
    command: Option<(Vec<String>, Exit)>,
    raw: Option<String>,
    uncertain_module: Option<&str>,
) -> Result<ModulePass, ModuleError> {
    ctx.log(
        LogKey::DeadlineExceeded,
        json!({"deadline": REPLACE_DEADLINE_MINUTES}),
    );
    after_rollback(
        ctx,
        current,
        deadline_failure(current.pass, command, raw),
        uncertain_module,
    )
}

/// Undo only this pass's actual unloads, in reverse order. The rollback is
/// outside the 600-second execution window and attempts every module.
pub fn rollback_unloaded(ctx: &mut RunCtx, current: &ModulePass) {
    let modules = ctx.state.replace[current.record_index]
        .unloaded
        .iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>();
    reload(ctx, current.record_index, &modules)
}

fn rollback_with_uncertain(ctx: &mut RunCtx, current: &ModulePass, module: &str) {
    // A killed rmmod can have completed its kernel side effect. Its result is
    // not a confirmed unload, but modprobe is safe even when it stayed loaded.
    let mut modules = vec![module.to_string()];
    modules.extend(
        ctx.state.replace[current.record_index]
            .unloaded
            .iter()
            .rev()
            .cloned(),
    );
    reload(ctx, current.record_index, &modules)
}

/// After the canonical write, reload the task's original target set.
/// Restore uses the same stored targets; no pass recomputes them here.
pub fn reload_targets(ctx: &mut RunCtx, current: &ModulePass) -> Result<(), ModuleError> {
    let Some(targets) = ctx.state.reload_targets.as_ref() else {
        return Err(ModuleError::MissingReloadTargets);
    };
    let modules = targets.modules.clone();
    ctx.enter(match current.pass {
        Pass::Update => Phase::Reload,
        Pass::Restore => Phase::RestoreReload,
    });
    reload(ctx, current.record_index, &modules);
    Ok(())
}

struct Input {
    path: PathBuf,
    sha256: String,
    size: u64,
}

// `copy_hashed` deliberately handles either stream; retain which side failed
// so the approved write reason distinguishes an unreadable source from a
// failed temporary-file write.
struct SourceRead<'a> {
    file: &'a mut dyn Read,
    failed: bool,
}

impl Read for SourceRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let result = self.file.read(buf);
        if result
            .as_ref()
            .is_err_and(|e| e.kind() != io::ErrorKind::Interrupted)
        {
            self.failed = true;
        }
        result
    }
}

fn input(ctx: &RunCtx, pass: Pass, original: Option<&OriginalRow>) -> Result<Input, String> {
    if pass == Pass::Restore {
        let row = original.ok_or("restore original missing")?;
        return Ok(Input {
            path: row.path.clone(),
            sha256: row.sha256.clone(),
            size: row.size,
        });
    }
    let plan = ctx.state.plan.as_ref().ok_or("plan missing")?;
    let (row_id, planned_sha, kind, fingerprint) = match &plan.source {
        crate::task_state::PlanSource::Local {
            row_id,
            row_kind,
            fingerprint,
            sha256,
        } => (
            row_id.clone(),
            Some(sha256.as_str()),
            *row_kind,
            fingerprint.as_deref(),
        ),
        crate::task_state::PlanSource::Official => (
            format!("{}-build", ctx.id),
            None,
            crate::task_state::RowKind::Official,
            None,
        ),
        crate::task_state::PlanSource::Upload { fingerprint, .. } => (
            format!("{}-build", ctx.id),
            None,
            crate::task_state::RowKind::Upload,
            Some(fingerprint.as_str()),
        ),
    };
    let record_path = ctx.paths.row_dir(&row_id).join("record.json");
    let record = match store::read_json::<drivers::Record>(ctx.host.as_ref(), &record_path) {
        JsonRead::Ok(record) if record.kind_matches(&row_id) => record,
        _ => {
            return Err(format!(
                "cannot read driver record {}",
                record_path.display()
            ))
        }
    };
    if record.version != plan.version
        || record.target_kernel != plan.kernel
        || record.kind != kind
        || record.fingerprint.as_deref() != fingerprint
        || (kind == crate::task_state::RowKind::Original
            && !record.original_of.as_ref().is_some_and(|original| {
                original.kernel == plan.kernel && original.truenas_version == plan.truenas_version
            }))
        || planned_sha.is_some_and(|sha| sha != record.sha256)
    {
        return Err("driver record differs from the submitted plan".into());
    }
    Ok(Input {
        path: ctx.paths.row_dir(&row_id).join("nvidia.raw"),
        sha256: planned_sha.unwrap_or(&record.sha256).to_string(),
        size: record.size,
    })
}

fn write_failure(
    pass: Pass,
    reason: &'static str,
    raw: Option<String>,
    command: Option<(Vec<String>, Exit)>,
) -> ModuleFailure {
    let write_reason = json!({"key": reason, "params": {}});
    let (code, params) = match pass {
        Pass::Update => (
            ErrorCode::WriteFailed,
            json!({"write_reason": write_reason}),
        ),
        Pass::Restore => (
            ErrorCode::RestoreNotWritten,
            json!({"cause": "write_failed", "write_reason": write_reason}),
        ),
    };
    ModuleFailure {
        code,
        params,
        raw,
        command,
    }
}

fn remaining(ctx: &RunCtx, current: &ModulePass) -> Result<std::time::Duration, ModuleFailure> {
    let left = current.deadline.saturating_duration_since(ctx.host.now());
    if left.is_zero() {
        ctx.log(
            LogKey::DeadlineExceeded,
            json!({"deadline": REPLACE_DEADLINE_MINUTES}),
        );
        Err(deadline_failure(current.pass, None, None))
    } else {
        Ok(left)
    }
}

fn timed_command_failure(
    ctx: &RunCtx,
    current: &ModulePass,
    cmd: &Cmd,
    out: &Output,
) -> ModuleFailure {
    ctx.log(
        LogKey::DeadlineExceeded,
        json!({"deadline": REPLACE_DEADLINE_MINUTES}),
    );
    deadline_failure(current.pass, Some((cmd.argv(), out.exit)), command_raw(out))
}

fn mount_lines(ctx: &RunCtx) -> Option<Vec<facts::MountLine>> {
    let bytes = ctx.host.read(Path::new("/proc/self/mounts")).ok()?;
    Some(facts::parse_mounts(&String::from_utf8_lossy(&bytes)))
}

fn restore_readonly(ctx: &mut RunCtx, current: &ModulePass) {
    let Some(rootfs) = ctx.state.rootfs.clone() else {
        return;
    };
    let set = Cmd::new(At::Host, "zfs", HOST_CMD_TIMEOUT).args([
        "set",
        &format!("readonly={}", rootfs.readonly_before),
        &rootfs.dataset,
    ]);
    let _ = ctx.cmd(&set); // the exit alone does not say whether it applied.
    let get = Cmd::new(At::Host, "zfs", HOST_CMD_TIMEOUT).args([
        "get",
        "-H",
        "-o",
        "value",
        "readonly",
        &rootfs.dataset,
    ]);
    let out = ctx.cmd(&get);
    let value = out.stdout_text().trim().to_string();
    let reading = if out.exit.ok() && (value == "on" || value == "off") {
        ReadingJson::value(value)
    } else {
        ReadingJson::unknown(REASON_FORMAT_UNRECOGNIZED)
    };
    ctx.state.replace[current.record_index].readonly_after = Some(reading.clone());
    let _ = ctx.write_state();
    ctx.log(
        LogKey::ReadonlyRestored,
        json!({"dataset": rootfs.dataset, "value": rootfs.readonly_before, "reading": reading}),
    );
}

fn unmount_and_remove(ctx: &RunCtx, mounted: bool) {
    let unmounted = !mounted
        || ctx
            .cmd(&Cmd::new(At::Host, "umount", HOST_CMD_TIMEOUT).arg(RW_MOUNT))
            .exit
            .ok();
    // `remove_dir_all` would recurse into the live /usr dataset if umount
    // failed. An unknown mount table is equally unsafe for deletion.
    if unmounted
        && mount_lines(ctx).is_some_and(|lines| {
            !lines
                .iter()
                .any(|line| Path::new(&line.mount_point).starts_with(RW_MOUNT))
        })
    {
        let _ = ctx.host.remove_dir_all(Path::new(RW_MOUNT));
    }
}

fn stopped(
    ctx: &mut RunCtx,
    current: &ModulePass,
    failure: ModuleFailure,
    temp_created: bool,
    mounted: bool,
) -> Result<(), ModuleError> {
    let temp_removed = !temp_created
        || match ctx.host.remove_file(Path::new(RW_TMP_PATH)) {
            Ok(()) => true,
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        };
    if temp_removed {
        unmount_and_remove(ctx, mounted);
    }
    restore_readonly(ctx, current);
    if failure.code != ErrorCode::ReplaceDeadlineExceeded
        && !(failure.code == ErrorCode::RestoreNotWritten && failure.params["cause"] == "deadline")
    {
        ctx.log(
            LogKey::WriteStopped,
            json!({"write_reason": failure.params["write_reason"].clone()}),
        );
    }
    rollback_unloaded(ctx, current);
    Err(ModuleError::Failure(failure))
}

/// Success means `mv -T` has written the canonical file; failures
/// before it restore the mount and actual unloads.
pub fn write(
    ctx: &mut RunCtx,
    current: &ModulePass,
    original: Option<&OriginalRow>,
) -> Result<(), ModuleError> {
    ctx.enter(match current.pass {
        Pass::Update => Phase::Write,
        Pass::Restore => Phase::RestoreWrite,
    });
    let mut temp_created = false;
    let mut mounted = false;
    let mut rename_uncertain = false;
    let attempt = (|| -> Result<Input, ModuleError> {
        let input = input(ctx, current.pass, original)
            .map_err(|raw| write_failure(current.pass, WRITE_SOURCE_UNREADABLE, Some(raw), None))?;
        if ctx.state.rootfs.is_none() {
            let dataset = mount_lines(ctx)
                .and_then(|lines| {
                    let mut matches = lines
                        .into_iter()
                        .filter(|line| line.fstype == "zfs" && line.mount_point == "/usr");
                    let first = matches.next()?;
                    matches.next().is_none().then_some(first.source)
                })
                .ok_or_else(|| write_failure(current.pass, WRITE_DATASET_UNKNOWN, None, None))?;
            let cmd = Cmd::new(At::Host, "zfs", remaining(ctx, current)?)
                .args(["get", "-H", "-o", "value", "readonly", &dataset]);
            let out = ctx.cmd(&cmd);
            if out.exit == Exit::TimedOut {
                return Err(timed_command_failure(ctx, current, &cmd, &out).into());
            }
            let value = out.stdout_text().trim().to_string();
            if !out.exit.ok() || (value != "on" && value != "off") {
                return Err(write_failure(
                    current.pass,
                    WRITE_READONLY_UNREADABLE,
                    command_raw(&out),
                    (!out.exit.ok()).then(|| (cmd.argv(), out.exit)),
                )
                .into());
            }
            ctx.state.rootfs = Some(Rootfs {
                dataset,
                readonly_before: value,
            });
            if !ctx.write_state() {
                return Err(ModuleError::StateNotPersisted);
            }
        }
        let rootfs = ctx.state.rootfs.as_ref().unwrap().clone();
        // A leftover or foreign mount at this fixed path is not this pass's
        // writable view. Never write through it or tear it down here.
        if mount_lines(ctx)
            .is_none_or(|lines| lines.iter().any(|line| line.mount_point == RW_MOUNT))
        {
            return Err(
                write_failure(current.pass, WRITE_RW_MOUNT_NOT_CONFIRMED, None, None).into(),
            );
        }
        let set = Cmd::new(At::Host, "zfs", remaining(ctx, current)?).args([
            "set",
            "readonly=off",
            &rootfs.dataset,
        ]);
        let out = ctx.cmd(&set);
        if out.exit == Exit::TimedOut {
            return Err(timed_command_failure(ctx, current, &set, &out).into());
        }
        ctx.host
            .create_dir(Path::new(RW_MOUNT), 0o755)
            .map_err(|e| {
                write_failure(
                    current.pass,
                    WRITE_RW_MOUNT_NOT_CONFIRMED,
                    Some(e.to_string()),
                    None,
                )
            })?;
        let mount = Cmd::new(At::Host, "mount", remaining(ctx, current)?).args([
            "-t",
            "zfs",
            "-o",
            "zfsutil,rw",
            &rootfs.dataset,
            RW_MOUNT,
        ]);
        let out = ctx.cmd(&mount);
        if out.exit == Exit::TimedOut {
            mounted = mount_lines(ctx).unwrap_or_default().iter().any(|line| {
                line.mount_point == RW_MOUNT
                    && line.fstype == "zfs"
                    && line.source == rootfs.dataset
            });
            return Err(timed_command_failure(ctx, current, &mount, &out).into());
        }
        let lines = mount_lines(ctx).unwrap_or_default();
        mounted = lines.iter().any(|line| {
            line.mount_point == RW_MOUNT && line.fstype == "zfs" && line.source == rootfs.dataset
        });
        let confirmed = lines.iter().any(|line| {
            line.mount_point == RW_MOUNT
                && line.is_rw()
                && line.fstype == "zfs"
                && line.source == rootfs.dataset
        });
        if !confirmed {
            return Err(write_failure(
                current.pass,
                WRITE_RW_MOUNT_NOT_CONFIRMED,
                command_raw(&out),
                (!out.exit.ok()).then(|| (mount.argv(), out.exit)),
            )
            .into());
        }
        ctx.log(
            LogKey::RwMountConfirmed,
            json!({"dataset": rootfs.dataset, "value": rootfs.readonly_before}),
        );

        let mut src = ctx.host.open(&input.path).map_err(|e| {
            write_failure(
                current.pass,
                WRITE_SOURCE_UNREADABLE,
                Some(e.to_string()),
                None,
            )
        })?;
        let mut dst = ctx
            .host
            .create(Path::new(RW_TMP_PATH), Create::Truncate, 0o644)
            .map_err(|e| {
                write_failure(current.pass, WRITE_TEMP_FAILED, Some(e.to_string()), None)
            })?;
        temp_created = true;
        let mut expired = false;
        let mut source = SourceRead {
            file: &mut src,
            failed: false,
        };
        let copied = store::copy_hashed(&mut source, &mut dst, |_| {
            expired = ctx.host.now() >= current.deadline;
            if expired {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .map_err(|e| {
            write_failure(
                current.pass,
                if source.failed {
                    WRITE_SOURCE_UNREADABLE
                } else {
                    WRITE_TEMP_FAILED
                },
                Some(e.to_string()),
                None,
            )
        })?;
        if expired || ctx.host.now() >= current.deadline {
            return Err(remaining(ctx, current).unwrap_err().into());
        }
        dst.sync_all().map_err(|e| {
            write_failure(current.pass, WRITE_TEMP_FAILED, Some(e.to_string()), None)
        })?;
        if copied.sha256 != input.sha256 || copied.size != input.size {
            return Err(write_failure(current.pass, WRITE_TEMP_MISMATCH, None, None).into());
        }
        ctx.log(
            LogKey::TempVerified,
            json!({"checksum": copied.sha256, "bytes": copied.size}),
        );
        let left = remaining(ctx, current)?;
        ctx.log(LogKey::DeadlineOk, json!({"seconds": left.as_secs()}));
        let mv = Cmd::new(At::Host, "mv", left).args(["-T", RW_TMP_PATH, RW_CANONICAL_PATH]);
        let out = ctx.cmd(&mv);
        if matches!(out.exit, Exit::TimedOut | Exit::Signal(_)) {
            match ctx.host.lstat(Path::new(RW_TMP_PATH)) {
                Ok(_) => {
                    return Err(if out.exit == Exit::TimedOut {
                        timed_command_failure(ctx, current, &mv, &out)
                    } else {
                        write_failure(
                            current.pass,
                            WRITE_RENAME_FAILED,
                            command_raw(&out),
                            Some((mv.argv(), out.exit)),
                        )
                    }
                    .into())
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => rename_uncertain = true,
            }
        } else if !out.exit.ok() {
            return Err(write_failure(
                current.pass,
                WRITE_RENAME_FAILED,
                command_raw(&out),
                Some((mv.argv(), out.exit)),
            )
            .into());
        }
        Ok(input)
    })();
    let input = match attempt {
        Ok(input) => input,
        Err(ModuleError::Failure(failure)) => {
            return stopped(ctx, current, failure, temp_created, mounted);
        }
        Err(ModuleError::StateNotPersisted) => {
            rollback_unloaded(ctx, current);
            return Err(ModuleError::StateNotPersisted);
        }
        Err(other) => return Err(other),
    };
    if !rename_uncertain {
        ctx.log(LogKey::CanonicalWritten, json!({}));
    }
    // the new canonical bytes are only durable when the directory entry is synced and the
    // record that claims the write is on disk. Neither failure may be swallowed, or a later
    // reader would treat an unpersisted write as a finished replacement.
    let fsync_ok = ctx
        .host
        .fsync_dir(Path::new(RW_CANONICAL_PATH).parent().unwrap())
        .is_ok();
    // A hash failure is not a mismatch: it leaves the outcome unknown (`None`,
    // shown as UNKNOWN), while a successful read that differs is a real
    // mismatch (`Some(false)`).
    let readback_ok = match store::hash_file(ctx.host.as_ref(), Path::new(RW_CANONICAL_PATH)) {
        Ok(copy) => Some(copy.sha256 == input.sha256),
        Err(_) => None,
    };
    ctx.state.replace[current.record_index].readback_ok = readback_ok;
    let state_ok = ctx.write_state();
    ctx.log(LogKey::Readback, json!({"readback_result": readback_ok}));
    unmount_and_remove(ctx, mounted);
    restore_readonly(ctx, current);
    if !state_ok {
        rollback_unloaded(ctx, current);
        return Err(ModuleError::StateNotPersisted);
    }
    if !fsync_ok || rename_uncertain {
        if fsync_ok && readback_ok == Some(true) {
            ctx.log(LogKey::CanonicalWritten, json!({}));
        } else {
            rollback_unloaded(ctx, current);
            return Err(ModuleError::WriteOutcomeUnknown);
        }
    } else if readback_ok != Some(true) {
        // The rename was certain, but the image now at the canonical path is not the image
        // that was validated: a checksum that could not be read (`None`) or one that differs
        // (`Some(false)`) is never success. The unloaded modules are rolled back and the run
        // stays INTERRUPTED, so the record and its inputs survive for recovery.
        rollback_unloaded(ctx, current);
        return Err(ModuleError::WriteOutcomeUnknown);
    }
    Ok(())
}

/// Confirm an unmerge, then always attempt one merge. The confirmation constrains
/// the check result, never the recovery action.
pub fn refresh(ctx: &mut RunCtx, current: &ModulePass) -> crate::runner::check::RefreshReadings {
    ctx.enter(match current.pass {
        Pass::Update => Phase::Refresh,
        Pass::Restore => Phase::RestoreRefresh,
    });
    let (before, before_reason) = crate::runner::check::sysext_since(ctx);
    let _ = ctx.cmd(&Cmd::new(At::Host, "systemd-sysext", HOST_CMD_TIMEOUT).arg("unmerge"));
    let unmerge_confirmed = matches!(
        crate::runner::check::sysext_unmerged(ctx),
        facts::Reading::Value(true)
    );
    let merge = ctx.cmd(&Cmd::new(At::Host, "systemd-sysext", HOST_CMD_TIMEOUT).arg("merge"));
    ctx.state.replace[current.record_index].refresh_exit = Some(merge.exit.to_json());
    let _ = ctx.write_state();
    let (after, after_reason) = crate::runner::check::sysext_since(ctx);
    let since_json = |reading: &crate::facts::Reading<u64>| match reading {
        crate::facts::Reading::Value(value) => ReadingJson::value(*value),
        crate::facts::Reading::Unknown(reason) => ReadingJson::unknown(reason),
    };
    ctx.log(
        LogKey::Since,
        json!({"before": since_json(&before), "after": since_json(&after)}),
    );
    let ldconfig = ctx.cmd(&Cmd::new(At::Host, "ldconfig", HOST_CMD_TIMEOUT));
    ctx.state.replace[current.record_index].ldconfig_exit = Some(ldconfig.exit.to_json());
    let _ = ctx.write_state();
    (
        before,
        after,
        before_reason,
        after_reason,
        unmerge_confirmed,
    )
}

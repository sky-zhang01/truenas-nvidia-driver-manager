//! The runner: one update task, executed in a transient unit on the host.

pub mod build;
pub mod build_env;
pub mod check;
pub mod download;
pub mod original;
pub mod replace;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::codes::ErrorCode;
use crate::codes::*;
use crate::facts::{self, Reading};
use crate::host::{Cmd, Host, Output, RealHost};
use crate::store::{self, Paths};
use crate::task_state::{
    self, Pass, Phase, ReloadTargetsJson, Restore, RestoreDecision, RestoreOriginal, Status,
    TaskState,
};

/// Shared state of one run.
pub struct RunCtx {
    pub host: Arc<dyn Host>,
    pub paths: Paths,
    pub id: String,
    pub state: TaskState,
    /// Monotonic deadline of the download+build phase, when set.
    pub build_deadline: Option<Instant>,
}

impl RunCtx {
    pub fn data_dir(&self) -> PathBuf {
        self.paths.root.clone()
    }

    pub fn task_dir(&self) -> PathBuf {
        self.paths.task_dir(&self.id)
    }

    pub fn log(&self, key: LogKey, params: serde_json::Value) {
        task_state::log(self.host.as_ref(), &self.paths, &self.id, key, params);
    }

    pub fn write_state(&self) -> bool {
        task_state::write(self.host.as_ref(), &self.paths, &self.state).is_ok()
    }

    pub fn enter(&mut self, phase: Phase) {
        self.state.phase = Some(phase);
        let _ = self.write_state();
        self.log(LogKey::PhaseEnter, json!({"phase": phase.as_str()}));
    }

    /// Start the download+build window and log `log.build_deadline_set`: the wall-clock
    /// instant it ends, which the interface renders as `{time}`, plus the configured minutes.
    pub fn set_build_deadline(&mut self) {
        let minutes = self
            .state
            .plan
            .as_ref()
            .map(|p| p.build_timeout_minutes)
            .unwrap_or(0);
        let window = Duration::from_secs(60 * minutes as u64);
        // The host clock, not `Instant::now`: the fake host's clock can be advanced past the
        // deadline, and production hosts use the same monotonic clock.
        self.build_deadline = Some(self.host.now() + window);
        let deadline_ms = self.host.wall_ms() + window.as_millis() as u64;
        self.log(
            LogKey::BuildDeadlineSet,
            json!({"minutes": minutes, "deadline_ms": deadline_ms}),
        );
    }

    /// Run a host command and record it in the task log.
    pub fn cmd(&self, cmd: &Cmd) -> Output {
        let out = self.host.run(cmd);
        let params = if cmd.program == "systemd-sysext" {
            // The full stdout above is used for parsing. The log retains only
            // bounded verbatim diagnostics from each stream.
            json!({
                "argv": cmd.argv(),
                "exit": out.exit.to_json(),
                "ms": out.elapsed.as_millis() as u64,
                "stdout": tail_lines(&out.stdout),
                "stderr": tail_lines(&out.stderr),
            })
        } else {
            json!({
                "argv": cmd.argv(),
                "exit": out.exit.to_json(),
                "ms": out.elapsed.as_millis() as u64,
            })
        };
        self.log(LogKey::Cmd, params);
        out
    }

    /// Record a failure; `finish` commits the terminal state after the final observations.
    pub fn fail(&mut self, code: ErrorCode, params: serde_json::Value, raw: Option<String>) {
        self.fail_with_command(code, params, raw, None);
    }

    /// [`RunCtx::fail`] with the host command that failed, so the terminal state carries
    /// `error.command` and `error.exit`.
    pub fn fail_with_command(
        &mut self,
        code: ErrorCode,
        params: serde_json::Value,
        raw: Option<String>,
        command: Option<(Vec<String>, crate::host::Exit)>,
    ) {
        let mut record = crate::task_state::ErrorRecord::new(code, params);
        if let Some(raw) = raw {
            record = record.with_raw(raw);
        }
        if let Some((argv, exit)) = command {
            record = record.with_command(argv, exit);
        }
        self.state.error = Some(record);
    }
}

fn tail_lines(bytes: &[u8]) -> String {
    let mut lines = bytes.split(|b| *b == b'\n').collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let from = lines.len().saturating_sub(ERROR_RAW_LINES);
    String::from_utf8_lossy(&lines[from..].join(&b'\n')).into_owned()
}

/// Entry point of `task run <id>`.
pub fn run(id: &str) -> std::process::ExitCode {
    let data_dir = match crate::host::data_dir_from_exe() {
        Ok(d) => d,
        Err(_) => return std::process::ExitCode::FAILURE,
    };
    run_with(Arc::new(RealHost::on_host()), Paths::new(data_dir), id)
}

/// Entry point used by tests with an injected host.
pub fn run_with(host: Arc<dyn Host>, paths: Paths, id: &str) -> std::process::ExitCode {
    run_inner(host, paths, id, None)
}

/// [`run_with`] with a build environment the caller already prepared: the build stage uses it
/// instead of preparing the pinned environment. The tests use it to exercise the build stage
/// and everything after it (preparing the pinned environment has its own cases in
/// `tests/build_env.rs`); the production entry [`run`] always prepares the pinned environment
/// itself.
pub fn run_with_env(
    host: Arc<dyn Host>,
    paths: Paths,
    id: &str,
    prepared: build_env::BuildEnv,
) -> std::process::ExitCode {
    run_inner(host, paths, id, Some(prepared))
}

fn run_inner(
    host: Arc<dyn Host>,
    paths: Paths,
    id: &str,
    prepared: Option<build_env::BuildEnv>,
) -> std::process::ExitCode {
    if !store::task_id_ok(id) {
        return std::process::ExitCode::FAILURE;
    }
    // The execution lock is the only host-global lock a runner takes. It is
    // held until the terminal state is written and every cleanup step has
    // finished.
    let mut execution = match crate::admission::try_execution_lock(&*host) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            refuse(&crate::admission::Blocked::new(
                crate::codes::RUNNER_DUPLICATE_EXECUTION,
            ));
            return std::process::ExitCode::FAILURE;
        }
        Err(blocked) => {
            refuse(&blocked);
            return std::process::ExitCode::FAILURE;
        }
    };
    // Read and validate the state **while the lock is held**: a late second
    // runner cannot replay a task the first one already finished.
    let state = match task_state::read(&*host, &paths, id) {
        store::JsonRead::Ok(s) => s,
        _ => {
            refuse(&crate::admission::Blocked::new(
                crate::codes::RUNNER_STATE_NOT_RUNNING,
            ));
            let _ = execution.release();
            return std::process::ExitCode::FAILURE;
        }
    };
    if state.status != Status::Running || state.plan.is_none() {
        refuse(&crate::admission::Blocked::new(
            crate::codes::RUNNER_STATE_NOT_RUNNING,
        ));
        let _ = execution.release();
        return std::process::ExitCode::FAILURE;
    }
    // `new_state` leaves this empty; the first, purely read-only stage fills it
    // in and persists it before any host side effect. Seeing a value here means
    // the task already started (including a death right after an unload), so it
    // must not be executed a second time.
    if state.loaded_version_start.is_some() {
        refuse(&crate::admission::Blocked::new(
            crate::codes::RUNNER_ALREADY_STARTED,
        ));
        let _ = execution.release();
        return std::process::ExitCode::FAILURE;
    }
    let mut ctx = RunCtx {
        host,
        paths,
        id: id.to_string(),
        state,
        build_deadline: None,
    };
    let terminal = execute(&mut ctx, prepared);
    let code = match terminal {
        Ok(()) | Err(FlowStop::Terminal) => std::process::ExitCode::SUCCESS,
        Err(FlowStop::Interrupted) => std::process::ExitCode::FAILURE,
    };
    let _ = execution.release();
    code
}

/// A runner refusal is logged with a machine-readable reason and never written
/// into the task state.
fn refuse(blocked: &crate::admission::Blocked) {
    crate::log::error_json(
        Event::AdmissionBlocked,
        &[("reason", blocked.to_json()), ("process", json!("runner"))],
    );
}

/// Why a stage stopped the phase table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowStop {
    /// The stage wrote a terminal state; the flow is over.
    Terminal,
    /// No terminal state was written: the runner exits and reconciliation
    /// records INTERRUPTED instead.
    Interrupted,
}

/// The phase table. Returns Ok when a terminal state was written, Err when the runner must
/// exit without one (reconcile records INTERRUPTED instead).
fn execute(ctx: &mut RunCtx, prepared: Option<build_env::BuildEnv>) -> Result<(), FlowStop> {
    ctx.enter(Phase::ReadLoadedVersion);
    if let Err(()) = stage_read_loaded_version(ctx) {
        return Err(FlowStop::Interrupted);
    }

    let plan_source = ctx.state.plan.as_ref().unwrap().source.clone();
    let original_choice = ctx.state.plan.as_ref().unwrap().original;
    let is_official = matches!(plan_source, crate::task_state::PlanSource::Official);
    let is_upload = matches!(plan_source, crate::task_state::PlanSource::Upload { .. });

    // This must precede official driver downloads as well as image/package preparation.
    let compiler = if is_official || is_upload {
        match build_env::compiler(ctx) {
            Ok(compiler) => Some(compiler),
            Err(failure) => {
                ctx.fail_with_command(failure.code, failure.params, failure.raw, failure.command);
                return finish(ctx).map_err(|_| FlowStop::Interrupted);
            }
        }
    } else {
        None
    };

    if original_choice == crate::task_state::OriginalChoice::Save {
        ctx.enter(Phase::SaveOriginal);
        match original::save(ctx) {
            Ok(()) => {}
            Err(failure) => {
                // The command's argv/exit travel with the failure so the terminal state
                // carries `error.command`/`error.exit` like every other command failure.
                ctx.fail_with_command(
                    ErrorCode::SaveOriginalFailed,
                    json!({"save_reason": {"key": failure.reason, "params": failure.params}}),
                    failure.raw,
                    failure.command,
                );
                return finish(ctx).map_err(|_| FlowStop::Interrupted);
            }
        }
    }

    if is_official {
        ctx.enter(Phase::Download);
        ctx.state.phase = Some(Phase::Download);
        let _ = ctx.write_state();
        ctx.set_build_deadline();
        let deadline = ctx.build_deadline.expect("the deadline was just set");
        match download::all(ctx, deadline) {
            Ok(()) => {}
            Err(failure) => {
                ctx.fail_with_command(failure.code, failure.params, failure.raw, failure.command);
                return finish(ctx).map_err(|_| FlowStop::Interrupted);
            }
        }
    }

    // build (official and upload). Both compiling sources prepare the pinned build
    // environment first: the official image, the fixed Debian package closure and the
    // embedded build script. An operation that does not compile never reaches this stage.
    if is_official || is_upload {
        stage_build(ctx, prepared, compiler.as_deref().unwrap())?;
    }

    let update = replace::unload(ctx, Pass::Update).or_else(|e| module_error(ctx, e))?;
    replace::write(ctx, &update, None).or_else(|e| module_error(ctx, e))?;
    let readings = replace::refresh(ctx, &update);
    replace::reload_targets(ctx, &update).or_else(|e| module_error(ctx, e))?;
    let expected = ctx.state.plan.as_ref().unwrap().version.clone();
    replace::align_modules(ctx, &update, &expected).or_else(|e| module_error(ctx, e))?;
    ctx.enter(Phase::Check);
    let checked = check::check(ctx, Pass::Update, readings, &expected);
    let passed = checked.passed;
    ctx.state.checks.push(checked);
    let _ = ctx.write_state();
    if passed {
        return finish_as(ctx, Status::Succeeded).map_err(|_| FlowStop::Interrupted);
    }

    let original = match original::usable_original(ctx) {
        Ok(row) => row,
        Err(missing) => {
            ctx.state.restore = Some(Restore {
                decision: RestoreDecision::None,
                reason: Some(missing.reason.to_string()),
                read_reason: missing.read_reason.map(str::to_string),
                original: None,
            });
            let _ = ctx.write_state();
            ctx.log(
                LogKey::RestoreNone,
                json!({
                    "reason": missing.reason,
                    "read_reason": missing.read_reason,
                }),
            );
            ctx.fail(ErrorCode::NoUsableOriginal, json!({}), None);
            return finish(ctx).map_err(|_| FlowStop::Interrupted);
        }
    };
    ctx.state.restore = Some(Restore {
        decision: RestoreDecision::Restore,
        reason: None,
        read_reason: None,
        original: Some(RestoreOriginal {
            row_id: original.row_id.clone(),
            version: original.version.clone(),
            saved_at: original.saved_at,
        }),
    });
    let _ = ctx.write_state();
    ctx.log(
        LogKey::RestoreStart,
        json!({"original": original.version, "date": original.saved_at}),
    );

    let restore = replace::unload(ctx, Pass::Restore).or_else(|e| module_error(ctx, e))?;
    replace::write(ctx, &restore, Some(&original)).or_else(|e| module_error(ctx, e))?;
    let readings = replace::refresh(ctx, &restore);
    replace::reload_targets(ctx, &restore).or_else(|e| module_error(ctx, e))?;
    replace::align_modules(ctx, &restore, &original.version).or_else(|e| module_error(ctx, e))?;
    ctx.enter(Phase::RestoreCheck);
    let checked = check::check(ctx, Pass::Restore, readings, &original.version);
    let passed = checked.passed;
    ctx.state.checks.push(checked);
    let _ = ctx.write_state();
    if passed {
        finish_as(ctx, Status::StockRestored).map_err(|_| FlowStop::Interrupted)
    } else {
        ctx.fail(ErrorCode::RestoreCheckFailed, json!({}), None);
        finish(ctx).map_err(|_| FlowStop::Interrupted)
    }
}

/// prepare the pinned build environment and run the build container. Both compiling
/// sources — the official download and the uploaded package — come through here, so neither
/// can skip the environment; a task that does not compile never calls it. `prepared` is an
/// environment the caller already prepared (tests, [`run_with_env`]); `None` prepares the
/// pinned environment itself.
pub fn stage_build(
    ctx: &mut RunCtx,
    prepared: Option<build_env::BuildEnv>,
    compiler: &str,
) -> Result<(), FlowStop> {
    ctx.enter(Phase::Build);
    if ctx.build_deadline.is_none() {
        ctx.set_build_deadline();
    }
    let deadline = ctx.build_deadline.expect("the deadline was just set");
    let acquired = match prepared {
        Some(env) => Ok(env),
        None => build_env::acquire(ctx, deadline),
    };
    let env = match acquired {
        Ok(env) => env,
        Err(failure) => {
            ctx.fail_with_command(failure.code, failure.params, failure.raw, failure.command);
            finish(ctx).map_err(|_| FlowStop::Interrupted)?;
            return Err(FlowStop::Terminal);
        }
    };
    let built = build::build(ctx, &env, compiler);
    // The build is over whatever happened (success, failure, timeout alike): an
    // image this task pulled is removed again before the terminal state lands.
    build_env::release(ctx, &env);
    if let Err(failure) = built {
        ctx.fail_with_command(failure.code, failure.params, failure.raw, failure.command);
        finish(ctx).map_err(|_| FlowStop::Interrupted)?;
        return Err(FlowStop::Terminal);
    }
    Ok(())
}

fn module_error<T>(ctx: &mut RunCtx, error: replace::ModuleError) -> Result<T, FlowStop> {
    match error {
        replace::ModuleError::Failure(failure) => {
            failure.record(ctx);
            finish(ctx).map_err(|_| FlowStop::Interrupted)?;
            Err(FlowStop::Terminal)
        }
        // An unconfirmed write or missing required state stays RUNNING for reconciliation.
        replace::ModuleError::MissingReloadTargets
        | replace::ModuleError::WriteOutcomeUnknown
        | replace::ModuleError::StateNotPersisted => Err(FlowStop::Interrupted),
    }
}

/// read the loaded driver version and the reload targets.
fn stage_read_loaded_version(ctx: &mut RunCtx) -> Result<(), ()> {
    let modules = match ctx.host.read(std::path::Path::new(PROC_MODULES_PATH)) {
        Ok(bytes) => facts::parse_proc_modules(&String::from_utf8_lossy(&bytes)).ok(),
        Err(_) => None,
    };
    let modules_reading = match &modules {
        Some(m) => Reading::Value(m.clone()),
        None => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
    };
    match facts::loaded_version(ctx.host.as_ref(), Some(&modules_reading)) {
        facts::LoadedVersion::Value(v) => {
            ctx.state.loaded_version_start =
                Some(crate::task_state::ReadingJson::value(v.text().to_string()));
            ctx.log(LogKey::LoadedVersion, json!({"version": v.text()}));
        }
        facts::LoadedVersion::NotLoaded => {
            ctx.state.loaded_version_start =
                Some(crate::task_state::ReadingJson::NotLoaded { not_loaded: true });
            ctx.log(LogKey::LoadedVersionNone, json!({}));
        }
        facts::LoadedVersion::Unknown(reason) => {
            ctx.state.loaded_version_start = Some(crate::task_state::ReadingJson::unknown(reason));
            ctx.log(LogKey::LoadedVersionUnknown, json!({"read_reason": reason}));
        }
    }
    if let Some(targets) = facts::reload_targets(modules.as_deref()) {
        if targets.basis == facts::ReloadBasis::NoneLoadedAtStart {
            ctx.log(LogKey::ReloadTargetDefault, json!({}));
        }
        ctx.state.reload_targets = Some(ReloadTargetsJson::from(&targets));
    }
    // The persisted start reading is the marker that this task has begun; it is
    // a hard prerequisite for every later host side effect.
    if ctx.write_state() {
        Ok(())
    } else {
        Err(())
    }
}

/// read the loaded version again, take the GPU users, write the terminal state and
/// clean up.
#[allow(clippy::result_unit_err)]
pub fn finish(ctx: &mut RunCtx) -> Result<(), ()> {
    finish_terminal(ctx, None)
}

#[allow(clippy::result_unit_err)]
fn finish_as(ctx: &mut RunCtx, status: Status) -> Result<(), ()> {
    finish_terminal(ctx, Some(status))
}

#[allow(clippy::result_unit_err)]
fn finish_terminal(ctx: &mut RunCtx, terminal: Option<Status>) -> Result<(), ()> {
    let terminal = terminal.or((ctx.state.status != Status::Running).then_some(ctx.state.status));
    // The final version observation may be interrupted. Until its final atomic write
    // the persisted state must remain RUNNING with no finished_at.
    ctx.state.status = Status::Running;
    ctx.enter(Phase::ReadLoadedVersionEnd);
    let modules = match ctx.host.read(std::path::Path::new(PROC_MODULES_PATH)) {
        Ok(bytes) => match facts::parse_proc_modules(&String::from_utf8_lossy(&bytes)) {
            Ok(m) => Some(Reading::Value(m)),
            Err(_) => Some(Reading::Unknown(REASON_FORMAT_UNRECOGNIZED)),
        },
        Err(_) => Some(Reading::Unknown(REASON_IO_ERROR)),
    };
    match facts::loaded_version(ctx.host.as_ref(), modules.as_ref()) {
        facts::LoadedVersion::Value(v) => {
            ctx.state.loaded_version_end =
                Some(crate::task_state::ReadingJson::value(v.text().to_string()));
        }
        facts::LoadedVersion::NotLoaded => {
            ctx.state.loaded_version_end =
                Some(crate::task_state::ReadingJson::NotLoaded { not_loaded: true });
        }
        facts::LoadedVersion::Unknown(reason) => {
            ctx.state.loaded_version_end = Some(crate::task_state::ReadingJson::unknown(reason));
        }
    }

    // The GPU user list is needed for GPU occupancy, unload/restore failures and version-change notices.
    let needs_users = matches!(
        ctx.state.error.as_ref().map(|e| ErrorCode::parse(&e.code)),
        Some(Some(ErrorCode::GpuInUse))
            | Some(Some(ErrorCode::ModuleUnloadRefused))
            | Some(Some(ErrorCode::RestoreAborted))
    ) || {
        let start = ctx
            .state
            .loaded_version_start
            .as_ref()
            .and_then(|r| r.as_value())
            .cloned();
        let end = ctx
            .state
            .loaded_version_end
            .as_ref()
            .and_then(|r| r.as_value())
            .cloned();
        matches!((start, end), (Some(a), Some(b)) if a != b)
    };
    if needs_users {
        // runs on the host: its object queries use the runner's command timeout, not the web
        // process's query timeout.
        let users = crate::gpu_users::list(ctx.host.as_ref(), HOST_CMD_TIMEOUT);
        // `log.gpu_users`: each application carries its type and each unreadable category is
        // reported as such.
        ctx.log(
            LogKey::GpuUsers,
            json!({
                "applications": users
                    .users
                    .iter()
                    .map(|u| json!({"name": u.name, "kind": u.kind.as_str()}))
                    .collect::<Vec<_>>(),
                "errors": users
                    .errors
                    .iter()
                    .map(|e| json!({"kind": e.kind.as_str()}))
                    .collect::<Vec<_>>(),
            }),
        );
        ctx.state.gpu_users = Some(crate::task_state::GpuUsersRecord {
            users: users
                .users
                .iter()
                .map(|u| crate::task_state::GpuUserRecord {
                    name: u.name.clone(),
                    kind: u.kind.as_str().to_string(),
                })
                .collect(),
            errors: users
                .errors
                .iter()
                .map(|e| crate::task_state::GpuUserError {
                    kind: e.kind.as_str().to_string(),
                    reason: e.reason.clone(),
                })
                .collect(),
        });
    }

    if ctx.state.finished_at.is_none() {
        ctx.state.finished_at = Some(ctx.host.wall_ms());
    }
    ctx.state.status = terminal.unwrap_or(if ctx.state.error.is_some() {
        Status::Failed
    } else {
        Status::Running
    });
    if ctx.state.phase != Some(Phase::ReadLoadedVersionEnd) {
        ctx.state.phase = Some(Phase::ReadLoadedVersionEnd);
    }
    ctx.state.waiting_gpu = None;
    let written = ctx.write_state();
    if written {
        ctx.log(
            LogKey::Finished,
            json!({"status": ctx.state.status.as_str()}),
        );
    }

    // Cleanup. A task whose driver-changing responsibility is not proven settled keeps its
    // task directory, state, rootfs and unique inputs: a green cleanup does not prove the
    // host is safe to discard. `written` must be true, otherwise the record does not even
    // describe this run.
    let rootfs = ctx.state.rootfs.clone();
    let settled =
        written && crate::tasks::driver_settled(ctx.host.as_ref(), &ctx.paths, &ctx.state).is_ok();
    if !settled {
        // The driver responsibility is not proven settled, so the task
        // directory, its inputs and its record stay as the recovery entry. The
        // host is put back all the same: the writable mount and the read-only
        // property must not be left behind for the next task to trip over.
        for r in crate::tasks::restore_host(ctx.host.as_ref(), rootfs.as_ref()) {
            if !r.ok {
                ctx.log(
                    LogKey::CleanupFailed,
                    json!({
                        "cleanup_step": crate::codes::Event::cleanup_step_code(r.step),
                        "reason": r.log_reason.clone().unwrap_or_else(|| {
                            crate::log::reason_object(
                                REASON_IO_ERROR,
                                json!({}),
                                r.reason.as_deref(),
                            )
                        }),
                    }),
                );
            }
        }
        ctx.log(
            LogKey::CleanupFailed,
            json!({
                "cleanup_step": crate::codes::Event::cleanup_step_code(
                    crate::codes::CleanupStep::DeleteTaskDir,
                ),
                "reason": crate::log::reason_object(
                    REASON_RECORD_UNREADABLE,
                    json!({}),
                    Some("the driver responsibility of this task is not settled; its inputs are kept"),
                ),
            }),
        );
        return if written { Ok(()) } else { Err(()) };
    }
    let results = crate::tasks::cleanup(ctx.host.as_ref(), &ctx.paths, &ctx.id, rootfs.as_ref());
    let mut all_ok = true;
    for r in &results {
        if !r.ok {
            all_ok = false;
            ctx.log(
                LogKey::CleanupFailed,
                json!({
                    "cleanup_step": crate::codes::Event::cleanup_step_code(r.step),
                    "reason": r.log_reason.clone().unwrap_or_else(|| {
                        crate::log::reason_object(
                            REASON_IO_ERROR,
                            json!({}),
                            r.reason.as_deref(),
                        )
                    }),
                }),
            );
        }
    }
    if all_ok {
        ctx.log(LogKey::Cleanup, json!({}));
    }
    if written {
        Ok(())
    } else {
        Err(())
    }
}

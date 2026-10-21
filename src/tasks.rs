//! Runner staging, task units, reconciliation and cleanup.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::json;

use crate::codes::*;
use crate::facts;
use crate::host::{At, Cmd, Exit, Host, Output};
use crate::store::{self, JsonRead, Paths};
use crate::task_state::{self, Phase, Status, TaskState};

pub fn unit_name(id: &str) -> String {
    format!("nvm-task-{id}.service")
}

pub fn build_container_name(id: &str) -> String {
    format!("nvm-build-{id}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartResult {
    Started,
    NeverRan {
        reason: &'static str,
        raw: String,
        code: Option<i32>,
    },
    Unknown,
}

/// Stage the current executable for the host task unit.
pub fn stage_runner(h: &dyn Host, p: &Paths) -> std::io::Result<()> {
    h.create_dir(&p.bin(), 0o755)?;
    store::copy_file_verified(h, Path::new("/proc/self/exe"), &p.runner_bin(), 0o755).map(|_| ())
}

pub fn start(h: &dyn Host, host_data_dir: &Path, id: &str) -> StartResult {
    let runner = host_data_dir
        .join("manager")
        .join("bin")
        .join("truenas-nvidia-driver-manager");
    let out = h.run(
        &Cmd::new(At::Host, "systemd-run", HOST_QUERY_TIMEOUT)
            .arg(format!("--unit={}", unit_name(id)))
            .arg(format!(
                "--description=truenas-nvidia-driver-manager task {id}"
            ))
            .arg("--property=Type=exec")
            .arg("--expand-environment=no")
            .arg("--quiet")
            .arg("--")
            .arg(runner.as_os_str())
            .arg("task")
            .arg("run")
            .arg(id),
    );
    if out.exit.ok() {
        return StartResult::Started;
    }
    // task start failure check: classify the failed start.
    let show = h.run(
        &Cmd::new(At::Host, "systemctl", HOST_QUERY_TIMEOUT)
            .arg("show")
            .arg(unit_name(id))
            .arg("--property=LoadState,ActiveState,SubState,Result,ExecMainStatus"),
    );
    let raw = if !out.stderr.is_empty() {
        out.stderr_text()
    } else {
        show.stdout_text()
    };
    if !show.exit.ok() {
        return StartResult::Unknown;
    }
    let props = parse_show(&show.stdout_text());
    // `LoadState=not-found` alone is not proof that no child ran: the unit may have completed
    // and been unloaded, or the request may have been retried after a lost response. Only a
    // positive native EXEC failure proves it, and only a plain non-zero exit code can carry
    // that proof: a timeout, a signal or a spawn failure says nothing about the child.

    if matches!(out.exit, Exit::Code(_))
        && props.get("Result").map(|s| s.as_str()) == Some("exit-code")
        && props.get("ExecMainStatus").map(|s| s.as_str()) == Some("203")
    {
        return StartResult::NeverRan {
            reason: SUBMIT_EXEC_FAILED,
            raw,
            code: out.exit.code(),
        };
    }
    StartResult::Unknown
}

/// One row of `systemctl list-units`.#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitRow {
    pub unit: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub task_id: String,
}

/// unit inventory parsing: first four columns, only `nvm-task-<valid id>.service`.
pub fn parse_list_units(s: &str) -> Vec<UnitRow> {
    let mut out = Vec::new();
    for line in s.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let unit = f[0];
        let Some(id) = unit
            .strip_prefix("nvm-task-")
            .and_then(|r| r.strip_suffix(".service"))
        else {
            continue;
        };
        if !store::task_id_ok(id) {
            continue;
        }
        out.push(UnitRow {
            unit: unit.to_string(),
            load: f[1].to_string(),
            active: f[2].to_string(),
            sub: f[3].to_string(),
            task_id: id.to_string(),
        });
    }
    out
}

/// `systemctl show` output as key/value pairs.
pub fn parse_show(s: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in s.lines() {
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

/// True when the unit row means "not running".
pub fn unit_stopped(row: &UnitRow) -> bool {
    matches!(row.active.as_str(), "inactive" | "failed")
}

/// One `systemctl list-jobs` row kept for the pre-inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub unit: String,
    pub raw: String,
}

/// Parse `systemctl list-jobs --no-legend`. Every row is kept; a row that
/// cannot be explained is reported as unknown by the caller instead of being
/// skipped.
pub fn parse_list_jobs(s: &str) -> Vec<JobRow> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let unit = l
                .split_whitespace()
                .find(|f| f.starts_with("nvm-task-") || f.ends_with(".service"))
                .unwrap_or("")
                .to_string();
            JobRow {
                unit,
                raw: l.to_string(),
            }
        })
        .collect()
}

/// A job belongs to this program when its unit names one of our task units.
pub fn job_is_ours(row: &JobRow) -> bool {
    row.unit
        .strip_prefix("nvm-task-")
        .and_then(|r| r.strip_suffix(".service"))
        .map(store::task_id_ok)
        .unwrap_or(false)
}

/// True when a job row mentions our program at all, even in a shape we cannot
/// explain: such a row must block a cleanup rather than be ignored.
pub fn job_mentions_us(row: &JobRow) -> bool {
    row.raw.contains("nvm-task-")
}

/// The approved `{key, params}` reason of a host command that did not end with
/// exit code 0.
fn command_reason_value(
    exit: Exit,
    timeout: std::time::Duration,
) -> crate::task_state::ReasonValue {
    match exit {
        Exit::TimedOut => crate::task_state::ReasonValue::new(
            REASON_TIMED_OUT,
            json!({"seconds": timeout.as_secs()}),
        ),
        Exit::SpawnFailed => crate::task_state::ReasonValue::new(REASON_SPAWN_FAILED, json!({})),
        Exit::Code(code) => {
            crate::task_state::ReasonValue::new(REASON_EXIT_STATUS, json!({"code": code}))
        }
        Exit::Signal(signal) => {
            crate::task_state::ReasonValue::new(REASON_SIGNAL, json!({"signal": signal}))
        }
    }
}

/// Every container name the daemon reports, from a complete exact listing.
pub fn list_container_names(h: &dyn Host) -> std::io::Result<BTreeSet<String>> {
    let out = h.run(
        &Cmd::new(At::Host, "docker", HOST_CMD_TIMEOUT)
            .arg("ps")
            .arg("-a")
            .arg("--no-trunc")
            .arg("--format")
            .arg("{{.Names}}"),
    );
    if !out.exit.ok() {
        return Err(std::io::Error::other(out.raw_tail(5)));
    }
    Ok(out
        .stdout_text()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledTask {
    pub id: String,
    pub status: Option<Status>,
    pub phase: Option<Phase>,
    pub state: Option<TaskState>,
    pub state_read_error: Option<String>,
    pub state_note: Option<crate::task_state::StateNote>,
}

impl Reconciled {
    /// The shape of a collection that could not be read inside the response budget: every
    /// list is unknown and nothing claims to be complete. The worker that produced it keeps
    /// running in the background, so its locks stay held until the work really ends.
    pub fn unreadable(reason: crate::task_state::ReasonValue) -> Self {
        Reconciled {
            unit_query_error: Some(reason.clone()),
            tasks: Vec::new(),
            active: ActiveState::Unreadable { reason },
            collection_complete: false,
            cleanup_pending: Vec::new(),
            host_residue: None,
        }
    }
}

impl ReconciledTask {
    /// A task whose state file was read; its status and phase are the
    /// record's own.
    pub fn with_state(id: &str, state: TaskState) -> ReconciledTask {
        ReconciledTask {
            id: id.to_string(),
            status: Some(state.status),
            phase: state.phase,
            state: Some(state),
            state_read_error: None,
            state_note: None,
        }
    }

    /// A task whose unreadable or missing record was replaced by a fresh
    /// INTERRUPTED stub, keeping why it was rewritten and where the original
    /// bytes are.
    pub fn interrupted(
        id: &str,
        state: TaskState,
        read_error_key: String,
        note: crate::task_state::StateNote,
    ) -> ReconciledTask {
        ReconciledTask {
            state_read_error: Some(read_error_key),
            state_note: Some(note),
            ..ReconciledTask::with_state(id, state)
        }
    }

    /// A task whose record could not be read at all: nothing is claimed
    /// beyond the read failure named by `read_error_key`.
    pub fn unknown(id: &str, read_error_key: &str) -> ReconciledTask {
        ReconciledTask {
            id: id.to_string(),
            status: None,
            phase: None,
            state: None,
            state_read_error: Some(read_error_key.to_string()),
            state_note: None,
        }
    }

    pub fn status_for_ui(&self) -> String {
        match self.status {
            Some(s) => s.as_str().to_string(),
            None => "UNKNOWN".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveState {
    None,
    Running {
        task_ids: Vec<String>,
    },
    Unreadable {
        reason: crate::task_state::ReasonValue,
    },
}

/// What an [`ActiveState`] means for an operation that may not run while a
/// task may be unfinished: the ids of the tasks that are running, or the
/// reason the task state could not be read. `None` means nothing blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker<'a> {
    Running {
        task_ids: &'a [String],
    },
    Unreadable {
        reason: &'a crate::task_state::ReasonValue,
    },
}

impl ActiveState {
    pub fn blocker(&self) -> Option<Blocker<'_>> {
        match self {
            ActiveState::None => None,
            ActiveState::Running { task_ids } => Some(Blocker::Running { task_ids }),
            ActiveState::Unreadable { reason } => Some(Blocker::Unreadable { reason }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Reconciled {
    pub unit_query_error: Option<crate::task_state::ReasonValue>,
    pub tasks: Vec<ReconciledTask>,
    pub active: ActiveState,
    /// False when the state directory, the unit list, or the task directory
    /// could not be listed. An incomplete set is not an empty set.
    pub collection_complete: bool,
    /// Task ids whose cleanup did not finish every step. `active` keeps its
    /// meaning; this field is what makes the cleanup outcome consumable.
    pub cleanup_pending: Vec<String>,
    /// Set when the host still shows a change this tool made and could not put
    /// back: the writable view of the `/usr` dataset is still mounted, or the
    /// read-only property does not read back as the value a kept record says it
    /// had. A new task must not start on such a host, or it would record the
    /// leftover value as its own baseline.
    pub host_residue: Option<crate::task_state::ReasonValue>,
}

/// Which task the recorded start time selects, before any interrupt test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatestTask<'a> {
    /// The collection was read completely and contains no tasks.
    Empty,
    /// A tie, a recovery record, a task with no state, or an incomplete set.
    /// A recovery record's `created_at` is the recovery clock, not the
    /// original start, so any `state_note` makes the selection unknown
    /// instead of ranking that clock against real start times.
    Unknown,
    Selected(&'a ReconciledTask),
}

/// Times safe to show as execution times. Recovery `now` and the moment an
/// interrupt was discovered are not projected as start or end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectedTimes {
    pub created_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub interrupt_discovered_at: Option<u64>,
}

impl Reconciled {
    pub fn state_of(&self, id: &str) -> Option<&ReconciledTask> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Select by the original `TaskState.created_at`, then let the caller
    /// test that task's status. The id suffix, `finished_at`, recovery `now`,
    /// and file metadata do not break ties.
    pub fn latest_recorded(&self) -> LatestTask<'_> {
        if !self.collection_complete {
            return LatestTask::Unknown;
        }
        if self.tasks.is_empty() {
            return LatestTask::Empty;
        }
        let mut best_at: Option<u64> = None;
        let mut best: Option<&ReconciledTask> = None;
        let mut tie = false;
        for task in &self.tasks {
            let Some(state) = task.state.as_ref() else {
                return LatestTask::Unknown;
            };
            // `state_note` means this persisted `created_at` is not an original
            // start time (recovery writes `now`). It cannot be ordered.
            if state.state_note.is_some() || task.state_note.is_some() {
                return LatestTask::Unknown;
            }
            match best_at {
                None => {
                    best_at = Some(state.created_at);
                    best = Some(task);
                }
                Some(at) if state.created_at > at => {
                    best_at = Some(state.created_at);
                    best = Some(task);
                    tie = false;
                }
                Some(at) if state.created_at == at => tie = true,
                _ => {}
            }
        }
        if tie {
            return LatestTask::Unknown;
        }
        match best {
            Some(task) => LatestTask::Selected(task),
            None => LatestTask::Unknown,
        }
    }
}

/// Project persisted timestamps for home, task list, and task detail.
pub fn project_task_times(
    status: Option<Status>,
    state: Option<&crate::task_state::TaskState>,
) -> ProjectedTimes {
    let none = ProjectedTimes {
        created_at: None,
        finished_at: None,
        interrupt_discovered_at: None,
    };
    let Some(state) = state else {
        return none;
    };
    if state.state_note.is_some() {
        return none;
    }
    let interrupted = status == Some(Status::Interrupted) || state.status == Status::Interrupted;
    if interrupted {
        ProjectedTimes {
            created_at: Some(state.created_at),
            finished_at: None,
            interrupt_discovered_at: state.finished_at,
        }
    } else {
        ProjectedTimes {
            created_at: Some(state.created_at),
            finished_at: state.finished_at,
            interrupt_discovered_at: None,
        }
    }
}

fn list_state_ids(h: &dyn Host, p: &Paths) -> Result<Vec<String>, &'static str> {
    let dir = p.state_dir();
    match h.read_dir(&dir) {
        Ok(names) => Ok(names
            .iter()
            .filter_map(|n| {
                let text = n.to_string_lossy();
                task_state::state_file_id(&text).map(|s| s.to_string())
            })
            .collect()),
        Err(e) => Err(match e.kind() {
            std::io::ErrorKind::NotFound => "missing",
            std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
            _ => REASON_IO_ERROR,
        }),
    }
}

fn list_task_dirs(h: &dyn Host, p: &Paths) -> Result<Vec<String>, &'static str> {
    let mut out = Vec::new();
    match h.read_dir(&p.manager().join("tasks")) {
        Ok(names) => {
            for n in names {
                let text = n.to_string_lossy().into_owned();
                if store::task_id_ok(&text) {
                    out.push(text);
                }
            }
            Ok(out)
        }
        // A manager that has never stored a task has no directory. That is a
        // complete empty set, not a failed listing.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(match e.kind() {
            std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
            _ => REASON_IO_ERROR,
        }),
    }
}

/// unit inventory + the state-file table of and the cleanup trigger. Reconciliation as every
/// caller must run it: the control lock is taken here, the real inventory is read while it is
/// held, and only then is the execution lock attempted. The order matters: a child that has
/// already been `exec`ed but has not taken the execution lock yet would otherwise be killed
/// by the runner's `busy => FAILURE` rule.
pub fn reconcile_managed(h: &dyn Host, p: &Paths) -> Reconciled {
    // the real class of a failed lock acquisition is preserved. A refused directory, an
    // unsupported lock or a real I/O error is not a timeout, and the five real GETs must
    // report the class they actually met. Only the machine keys travel: the raw diagnostic
    // stays out of the public JSON.
    let control = match crate::admission::control_lock(h, crate::admission::LOCK_WAIT) {
        Ok(control) => control,
        Err(blocked) => return unreadable(blocked.key),
    };
    let mut control = control;
    let out = reconcile(h, p);
    let _ = control.release();
    out
}

fn unreadable(key: &'static str) -> Reconciled {
    Reconciled::unreadable(crate::task_state::ReasonValue::new(key, json!({})))
}

/// The complete pre-inventory of one reconciliation attempt: the unit list
/// and the job queue, both read to the end. `error` is set for a failed
/// query, a pending job of this program, or a row that names our units in a
/// shape this program cannot explain.
struct Inventory {
    error: Option<crate::task_state::ReasonValue>,
    running: BTreeSet<String>,
}

fn inventory(h: &dyn Host) -> Inventory {
    let units_out = h.run(
        &Cmd::new(At::Host, "systemctl", HOST_QUERY_TIMEOUT)
            .arg("list-units")
            .arg("--all")
            .arg("--plain")
            .arg("--no-legend")
            .arg("--no-pager")
            .arg("nvm-task-*"),
    );
    if !units_out.exit.ok() {
        return Inventory {
            error: Some(command_reason_value(units_out.exit, HOST_QUERY_TIMEOUT)),
            running: BTreeSet::new(),
        };
    }
    let mut running: BTreeSet<String> = BTreeSet::new();
    for line in units_out
        .stdout_text()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        match parse_list_units(line).first() {
            Some(row) => {
                if !unit_stopped(row) {
                    running.insert(row.task_id.clone());
                }
            }
            None if line.contains("nvm-task-") => {
                return Inventory {
                    error: Some(crate::task_state::ReasonValue::new(
                        DRAIN_UNEXPLAINED_UNIT_ROW,
                        json!({"row": line}),
                    )),
                    running,
                };
            }
            None => {}
        }
    }
    let jobs_out = h.run(
        &Cmd::new(At::Host, "systemctl", HOST_QUERY_TIMEOUT)
            .arg("list-jobs")
            .arg("--no-legend"),
    );
    if !jobs_out.exit.ok() {
        return Inventory {
            error: Some(command_reason_value(jobs_out.exit, HOST_QUERY_TIMEOUT)),
            running,
        };
    }
    for row in parse_list_jobs(&jobs_out.stdout_text()) {
        if job_is_ours(&row) {
            return Inventory {
                error: Some(crate::task_state::ReasonValue::new(
                    DRAIN_PENDING_JOB,
                    json!({"unit": row.unit}),
                )),
                running,
            };
        }
        if job_mentions_us(&row) {
            return Inventory {
                error: Some(crate::task_state::ReasonValue::new(
                    DRAIN_UNEXPLAINED_UNIT_ROW,
                    json!({"row": row.raw}),
                )),
                running,
            };
        }
    }
    Inventory {
        error: None,
        running,
    }
}

/// The fixed facts every phase of one reconciliation runs by.
struct Facts {
    running: BTreeSet<String>,
    unit_query_error: Option<crate::task_state::ReasonValue>,
    /// A unit-query failure makes every read report UNKNOWN instead of
    /// rebuilding anything.
    query_failed: bool,
    /// True only while this call holds the execution lock and both inventory
    /// readings said the host was idle: only then may a record be rewritten
    /// or anything be cleaned up.
    may_change: bool,
    state_dir_unreadable: Option<&'static str>,
    task_dirs_unreadable: Option<&'static str>,
    task_dirs: Vec<String>,
    now: u64,
}

/// What the settle phase read and did.
struct Settled {
    cleanup_pending: Vec<String>,
    host_residue: Option<crate::task_state::ReasonValue>,
    /// State material no rule explains; it stops the cleanup and keeps the
    /// collection incomplete.
    unexplained: Vec<String>,
}

/// The reconciliation core. The caller must already hold the control lock; the
/// execution lock is attempted here, and both stay held until the cleanup and
/// the post-change readings are finished.
pub fn reconcile(h: &dyn Host, p: &Paths) -> Reconciled {
    // The pre-inventory of every reconcile caller: it decides whether the execution lock may
    // be attempted at all.
    let pre = inventory(h);
    let pre_idle = pre.error.is_none() && pre.running.is_empty();
    // A live runner holds the execution lock; when it is busy nothing here may
    // rewrite a state or clean anything up, and `active` must say "undetermined"
    // instead of falling back to "no task".
    let owned: Option<Box<dyn crate::host::HostLock + Send>> = if pre_idle {
        match crate::admission::try_execution_lock(h) {
            Ok(Some(lock)) => Some(lock),
            Ok(None) | Err(_) => None,
        }
    } else {
        None
    };
    let locked = owned.is_some();
    // Recheck the complete inventory under the execution lock before changing an idle host.
    let post = if locked { inventory(h) } else { pre };
    let unit_query_error = post.error.clone();
    let running = post.running.clone();
    let post_idle = unit_query_error.is_none() && running.is_empty();
    let may_change = locked && pre_idle && post_idle;
    // Every cleanup caller protects the ids whose driver-changing responsibility is not
    // proven settled: deleting their task directory would destroy the only recovery input.
    // Computing it here covers startup, GET, precheck, submit, driver deletion and the runner
    // alike.
    let preserved: RecoveryUnion = recovery_union(h, p);
    let state_ids = list_state_ids(h, p);
    let mut facts = Facts {
        running,
        query_failed: unit_query_error.is_some(),
        unit_query_error,
        may_change,
        state_dir_unreadable: state_ids.as_ref().err().copied(),
        task_dirs_unreadable: None,
        task_dirs: Vec::new(),
        now: crate::host::Host::wall_ms(h),
    };
    let (mut tasks, recovered_rootfs) = read_state_files(
        h,
        p,
        state_ids.as_ref().unwrap_or(&Vec::new()),
        &facts,
        &preserved,
    );
    // Units running without a state file.
    for id in &facts.running {
        if !tasks.iter().any(|t| t.id == *id) {
            tasks.push(ReconciledTask::unknown(id, "missing"));
        }
    }
    let task_dirs_result = list_task_dirs(h, p);
    facts.task_dirs_unreadable = task_dirs_result.as_ref().err().copied();
    facts.task_dirs = task_dirs_result.unwrap_or_default();
    let settled = settle_tasks(h, p, &facts, &preserved, &mut tasks, &recovered_rootfs);

    // "Is there an unfinished task?"
    let active = if !facts.running.is_empty() {
        let mut ids: Vec<String> = facts.running.iter().cloned().collect();
        ids.sort();
        ActiveState::Running { task_ids: ids }
    } else if facts.unit_query_error.is_some() {
        let any_running_state = tasks.iter().any(|t| t.status == Some(Status::Running));
        if any_running_state {
            ActiveState::Running {
                task_ids: tasks
                    .iter()
                    .filter(|t| t.status == Some(Status::Running))
                    .map(|t| t.id.clone())
                    .collect(),
            }
        } else {
            ActiveState::Unreadable {
                reason: facts.unit_query_error.clone().unwrap(),
            }
        }
    } else if let Some(_reason) = facts.state_dir_unreadable {
        ActiveState::Unreadable {
            reason: crate::task_state::ReasonValue::new(
                if facts.state_dir_unreadable == Some(REASON_PERMISSION_DENIED) {
                    REASON_PERMISSION_DENIED
                } else {
                    REASON_IO_ERROR
                },
                json!({}),
            ),
        }
    } else if !settled.unexplained.is_empty() {
        // Material no rule explains: the host is not "no task", it is
        // undetermined, and the cleanup above was stopped for the same reason.
        ActiveState::Unreadable {
            reason: crate::task_state::ReasonValue::new(
                crate::codes::DRAIN_STATE_SCAN_FAILED,
                json!({"name": settled.unexplained[0]}),
            ),
        }
    } else if !facts.may_change && tasks.iter().any(|t| t.status == Some(Status::Running)) {
        // The execution lock is busy, so a runner is live: the persisted
        // RUNNING states are the accurate answer (and still refuse submission).
        ActiveState::Running {
            task_ids: tasks
                .iter()
                .filter(|t| t.status == Some(Status::Running))
                .map(|t| t.id.clone())
                .collect(),
        }
    } else if !facts.may_change {
        // Busy without any RUNNING state is genuinely undetermined: it must not
        // fall back to "no task".
        ActiveState::Unreadable {
            reason: crate::task_state::ReasonValue::new(
                crate::codes::ADMISSION_LOCK_BUSY,
                json!({}),
            ),
        }
    } else if tasks.iter().any(|t| t.status.is_none()) {
        ActiveState::Running {
            task_ids: tasks
                .iter()
                .filter(|t| t.status.is_none())
                .map(|t| t.id.clone())
                .collect(),
        }
    } else {
        ActiveState::None
    };

    let collection_complete = facts.unit_query_error.is_none()
        && facts.state_dir_unreadable.is_none()
        && facts.task_dirs_unreadable.is_none()
        && settled.unexplained.is_empty();
    let mut cleanup_pending = settled.cleanup_pending;
    cleanup_pending.sort();
    cleanup_pending.dedup();
    Reconciled {
        unit_query_error: facts.unit_query_error,
        tasks,
        active,
        collection_complete,
        cleanup_pending,
        host_residue: settled.host_residue,
    }
}

/// Read every state file of the state directory, rewrite a RUNNING record whose unit is gone
/// as INTERRUPTED, and keep an unreadable record aside under its sidecar name before a fresh
/// INTERRUPTED stub replaces it. The `rootfs` a bad file still carries is collected so the
/// cleanup can restore the ZFS property.
#[allow(clippy::type_complexity)]
fn read_state_files(
    h: &dyn Host,
    p: &Paths,
    ids: &[String],
    facts: &Facts,
    preserved: &RecoveryUnion,
) -> (
    Vec<ReconciledTask>,
    BTreeMap<String, crate::task_state::Rootfs>,
) {
    let mut tasks: Vec<ReconciledTask> = Vec::new();
    let mut recovered_rootfs: BTreeMap<String, crate::task_state::Rootfs> = BTreeMap::new();
    for id in ids {
        let read = task_state::read(h, p, id);
        let unit_running = facts.running.contains(id);
        match read {
            JsonRead::Ok(state) => {
                if !facts.query_failed
                    && facts.may_change
                    && state.status == Status::Running
                    && !unit_running
                {
                    let mut updated = state.clone();
                    updated.status = Status::Interrupted;
                    updated.finished_at = Some(facts.now);
                    updated.waiting_gpu = None;
                    if task_state::write(h, p, &updated).is_ok() {
                        crate::log::info(
                            Event::TaskInterrupted,
                            &[
                                ("task_id", id.as_str()),
                                (
                                    "phase",
                                    updated.phase.map(|p| p.as_str()).unwrap_or("UNKNOWN"),
                                ),
                            ],
                        );
                        tasks.push(ReconciledTask::with_state(id, updated));
                    } else {
                        tasks.push(ReconciledTask::with_state(id, state));
                    }
                } else {
                    tasks.push(ReconciledTask::with_state(id, state));
                }
            }
            other => {
                let state_read_error = store::state_read_error_key(&other)
                    .unwrap_or("invalid")
                    .to_string();
                // A rebuild renames and rewrites files: it may only happen while the
                // execution lock is really held and the host is confirmed idle. Otherwise the
                // record is reported UNKNOWN and left exactly as it is.
                if unit_running || facts.query_failed || !facts.may_change {
                    tasks.push(ReconciledTask::unknown(id, &state_read_error));
                } else {
                    // Keep what the file still says about the read-only
                    // property before the original may be moved aside.
                    let mut original: Option<Vec<u8>> = None;
                    if let Ok(bytes) = h.read(&p.state(id)) {
                        original = Some(bytes.clone());
                        if let Some(rootfs) = recovery_rootfs(&bytes) {
                            recovered_rootfs.insert(id.clone(), rootfs);
                        }
                    }
                    // The proven recovery information of the protected material (sidecar or
                    // recovery copy) is persisted with the rebuilt record, so a crash between
                    // the rename and the fresh state does not lose it.
                    if let Some(rootfs) = preserved.rootfs.get(id) {
                        recovered_rootfs.insert(id.clone(), rootfs.clone());
                    }
                    // Keep the original bytes under the sidecar name before anything may
                    // overwrite them. A refused rename falls back to a synced, verified copy;
                    // when neither works the original stays untouched and no new INTERRUPTED
                    // record is written over the only copy.
                    let kept = format!("{id}.json.unreadable");
                    // A permanent sidecar from an earlier corruption is never overwritten:
                    // with different bytes already kept the current `.json` stays as it is
                    // and is reported UNKNOWN.
                    let sidecar_conflict = state_sidecar_conflict(h, p, id, original.as_deref());
                    let renamed = !sidecar_conflict
                        && h.rename(&p.state(id), &p.state_unreadable(id)).is_ok();
                    let kept_original = renamed
                        || (!sidecar_conflict
                            && original
                                .as_deref()
                                .map(|bytes| preserve_state_original(h, p, id, bytes))
                                .unwrap_or(false));
                    if !kept_original {
                        tasks.push(ReconciledTask::unknown(id, &state_read_error));
                    } else {
                        let note = crate::task_state::StateNote {
                            state_read_error: state_read_error.clone(),
                            kept_as: Some(kept.clone()),
                        };
                        let fresh = TaskState::interrupted_stub(
                            id,
                            facts.now,
                            recovered_rootfs.get(id).cloned().filter(|r| r.valid()),
                            note.clone(),
                        );
                        if task_state::write(h, p, &fresh).is_ok() {
                            crate::log::info(
                                Event::StateFileKept,
                                &[("task_id", id.as_str()), ("file", kept.as_str())],
                            );
                            tasks.push(ReconciledTask::interrupted(
                                id,
                                fresh,
                                state_read_error,
                                note,
                            ));
                        } else {
                            if renamed {
                                let _ = h.rename(&p.state_unreadable(id), &p.state(id));
                            }
                            tasks.push(ReconciledTask::unknown(id, &state_read_error));
                        }
                    }
                }
            }
        }
    }
    (tasks, recovered_rootfs)
}

/// Rewrite a fresh record for a task directory whose state file is gone, reset
/// the host a kept record still owes, and remove the task directories whose
/// driver-changing responsibility is settled.
fn settle_tasks(
    h: &dyn Host,
    p: &Paths,
    facts: &Facts,
    preserved: &RecoveryUnion,
    tasks: &mut Vec<ReconciledTask>,
    recovered_rootfs: &BTreeMap<String, crate::task_state::Rootfs>,
) -> Settled {
    let mut cleanup_pending: Vec<String> = Vec::new();
    // Missing state file but a task directory remains. Only a successful unit query and a
    // readable `manager/state/` may mutate anything here. A failed task-directory listing is
    // not an empty set: nothing is created, and the latest-task read stays unknown. A
    // reconstruction writes a new record: it may only happen while the execution lock is
    // really held and the host was confirmed idle twice.
    if facts.may_change
        && facts.state_dir_unreadable.is_none()
        && facts.unit_query_error.is_none()
        && facts.task_dirs_unreadable.is_none()
    {
        for id in &facts.task_dirs {
            if tasks.iter().any(|t| t.id == *id) || facts.running.contains(id) {
                continue;
            }
            // The protected material of this id may be the only evidence of
            // the read-only property it needs restored: the proven rootfs is
            // persisted with the reconstructed record, never dropped.
            let recovered = preserved
                .rootfs
                .get(id)
                .cloned()
                .filter(|r| r.valid())
                .or_else(|| recovered_rootfs.get(id).cloned());
            // Whatever bytes really exist for this id (a bad file or the protected material
            // the store would restore into it) are the only evidence of what the task did:
            // they are kept under the permanent sidecar name before a fresh record may
            // supersede them, and a refused keep leaves the bytes exactly as they are.
            let material: Option<Vec<u8>> = h
                .read(&p.state(id))
                .ok()
                .or_else(|| recovery_materials(h, p, id).into_iter().next());
            let kept_name = format!("{id}.json.unreadable");
            let mut kept_as: Option<String> = None;
            let mut keep_refused = false;
            if let Some(bytes) = material {
                match h.read(&p.state_unreadable(id)) {
                    Ok(have) if have == bytes => kept_as = Some(kept_name.clone()),
                    Ok(_) => keep_refused = true,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        if store::write_atomic(h, &p.state_unreadable(id), &bytes, 0o644)
                            .and_then(|()| h.read(&p.state_unreadable(id)))
                            .map(|kept| kept == bytes)
                            .unwrap_or(false)
                        {
                            kept_as = Some(kept_name.clone());
                        } else {
                            keep_refused = true;
                        }
                    }
                    Err(_) => keep_refused = true,
                }
            }
            if keep_refused {
                // The only copy cannot be protected: no fresh record is written
                // over it, the id is reported UNKNOWN and nothing is deleted.
                tasks.push(ReconciledTask::unknown(id, "missing"));
                continue;
            }
            let note = crate::task_state::StateNote {
                state_read_error: "missing".to_string(),
                kept_as: kept_as.clone(),
            };
            let fresh = TaskState::interrupted_stub(id, facts.now, recovered, note.clone());
            if task_state::write(h, p, &fresh).is_ok() {
                if let Some(name) = &kept_as {
                    crate::log::info(
                        Event::StateFileKept,
                        &[("task_id", id.as_str()), ("file", name.as_str())],
                    );
                }
                tasks.push(ReconciledTask::interrupted(
                    id,
                    fresh,
                    "missing".to_string(),
                    note,
                ));
            }
        }
    }

    tasks.sort_by(|a, b| a.id.cmp(&b.id));

    // Re-read units, jobs and records immediately before cleanup to protect inputs from a stale snapshot.
    let check = inventory(h);
    let fresh_states = list_state_ids(h, p);
    let fresh_dirs = list_task_dirs(h, p);
    // Unexplained state material makes this snapshot unusable for a cleanup:
    // the material is the only evidence and must survive, so the reconciliation
    // neither deletes a task directory nor claims a complete collection.
    let unexplained = unexplained_state_names(h, p);
    let fresh_idle = check.error.is_none()
        && check.running.is_empty()
        && fresh_states.is_ok()
        && fresh_dirs.is_ok()
        && unexplained.is_empty();
    let mut preserved_now = recovery_union(h, p);
    let mut host_residue: Option<crate::task_state::ReasonValue> = None;
    if facts.may_change
        && fresh_idle
        && facts.unit_query_error.is_none()
        && facts.state_dir_unreadable.is_none()
        && facts.task_dirs_unreadable.is_none()
        && facts.running.is_empty()
    {
        // A record whose driver-changing responsibility is not proven settled
        // keeps its task directory, its inputs and its state file: they are the
        // only way back to the host. The host itself is put back regardless, or
        // the writable mount and the read-only value this task left behind
        // would block every later update. The reset is idempotent and only
        // touches the mount and the property this tool owns.
        for (id, rootfs) in &preserved_now.rootfs {
            if !preserved_now.unresolved.contains(id) {
                continue;
            }
            let results = restore_host(h, Some(rootfs));
            if !log_cleanup_steps(id, &results) {
                cleanup_pending.push(id.clone());
            }
        }
        // The readback decides, not the attempt: whatever the reset reported,
        // a host that still shows this tool's leftovers must not start a task.
        host_residue = host_leftovers(h, &preserved_now);
        let mut proofs = BTreeMap::new();
        let ids: Vec<String> = preserved_now.unresolved.iter().cloned().collect();
        for id in ids {
            if let Ok(state) = release_kept_record(
                h,
                p,
                &preserved_now,
                &id,
                host_residue.as_ref(),
                &mut proofs,
            ) {
                preserved_now.unresolved.remove(&id);
                preserved_now.states.insert(id.clone(), state.clone());
                if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
                    task.state = Some(state);
                }
            }
        }
        for id in &facts.task_dirs {
            // An unresolved driver responsibility keeps its task directory:
            // the recovery entry is the only way back to the host state. The
            // judgement uses the fresh union, not the one from before the
            // reconciliation rewrote records.
            if preserved_now.unresolved.contains(id) {
                continue;
            }
            let rootfs = recovered_rootfs.get(id).cloned().or_else(|| {
                tasks
                    .iter()
                    .find(|t| t.id == *id)
                    .and_then(|t| t.state.as_ref())
                    .and_then(|s| s.rootfs.clone())
            });
            let results = cleanup(h, p, id, rootfs.as_ref());
            if !log_cleanup_steps(id, &results) {
                cleanup_pending.push(id.clone());
            }
        }
    }

    Settled {
        cleanup_pending,
        host_residue,
        unexplained,
    }
}

/// Keep the bytes of an unreadable state file at its sidecar name without consuming the
/// original, and read both back. Used when the rename aside was refused: the fresh
/// INTERRUPTED record may only be written once the only copy of the original is confirmed
/// saved.
fn preserve_state_original(h: &dyn Host, p: &Paths, id: &str, bytes: &[u8]) -> bool {
    if store::write_atomic(h, &p.state_unreadable(id), bytes, 0o644).is_err() {
        return false;
    }
    h.read(&p.state_unreadable(id))
        .map(|kept| kept == bytes)
        .unwrap_or(false)
        && h.read(&p.state(id))
            .map(|now| now == bytes)
            .unwrap_or(false)
}

/// True when the permanent `.json.unreadable` sidecar already holds different bytes than the
/// state file in hand. The first kept original must not be overwritten by a second
/// corruption.
fn state_sidecar_conflict(h: &dyn Host, p: &Paths, id: &str, original: Option<&[u8]>) -> bool {
    match h.read(&p.state_unreadable(id)) {
        Ok(have) => original.map(|bytes| have != bytes).unwrap_or(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        // The sidecar cannot be read: treat the name as taken rather than
        // risk overwriting the only kept original.
        Err(_) => true,
    }
}

/// Minimal parse used to keep the read-only property of an unreadable state file.
#[derive(serde::Deserialize)]
struct RootfsOnly {
    #[serde(default)]
    rootfs: Option<crate::task_state::Rootfs>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    pub step: CleanupStep,
    pub ok: bool,
    /// True only when this call observed an actual removal or property change.
    pub changed: bool,
    pub reason: Option<String>,
    /// The `{key, params}` (plus optional raw) reason of the `docker logs`
    /// `task_cleanup_step_failed` line. `None` on a passing step.
    pub log_reason: Option<serde_json::Value>,
    pub reading: Option<String>,
}

fn step_ok(step: CleanupStep) -> StepResult {
    StepResult {
        step,
        ok: true,
        changed: false,
        reason: None,
        log_reason: None,
        reading: None,
    }
}

/// A failed step whose error is an operating-system operation on the host: the general reason
/// key, with the OS text as the raw of the same object.
fn step_failed_io(step: CleanupStep, e: std::io::Error) -> StepResult {
    let text = e.to_string();
    StepResult {
        step,
        ok: false,
        changed: false,
        reason: Some(text.clone()),
        log_reason: Some(crate::log::reason_object(
            crate::log::io_reason_key(&e),
            json!({}),
            Some(&text),
        )),
        reading: None,
    }
}

/// A failed step whose reason is a host command that did not end with exit code 0: the
/// command's own `Exit` keeps its classification and required parameters, and the stderr
/// stays the raw of the same `{key, params}` object. A missing exit code is never written as
/// `null`.
fn step_failed_cmd(step: CleanupStep, out: &Output, timeout: std::time::Duration) -> StepResult {
    let text = out.stderr_text();
    let (key, params) = match out.exit {
        Exit::TimedOut => (REASON_TIMED_OUT, json!({"seconds": timeout.as_secs()})),
        Exit::SpawnFailed => (REASON_SPAWN_FAILED, json!({})),
        Exit::Code(code) => (REASON_EXIT_STATUS, json!({"code": code})),
        Exit::Signal(signal) => (REASON_SIGNAL, json!({"signal": signal})),
    };
    StepResult {
        step,
        ok: false,
        changed: false,
        reason: Some(text.clone()),
        log_reason: Some(crate::log::reason_object(key, params, Some(&text))),
        reading: None,
    }
}

/// A failed step with an internal judgement text (for example a readback that still differs
/// from the recorded value): the general I/O phrase stays the object's key and the text is
/// its raw.
fn step_failed(step: CleanupStep, e: impl ToString) -> StepResult {
    let text = e.to_string();
    StepResult {
        step,
        ok: false,
        changed: false,
        reason: Some(text.clone()),
        log_reason: Some(crate::log::reason_object(
            REASON_IO_ERROR,
            json!({}),
            Some(&text),
        )),
        reading: None,
    }
}

/// The host-restoring half of the shared cleanup:
/// remove this tool's writable mount of the `/usr` dataset and its temporary
/// file, then restore the read-only property the record kept. Each step is
/// judged by a readback, never by an exit code. The task directory, its inputs
/// and its state file are not touched, so a record whose driver responsibility
/// is not settled can be reset on the host and still keep every recovery entry.
/// Every step is idempotent: a second run on an already reset host passes.
pub fn restore_host(h: &dyn Host, rootfs: Option<&crate::task_state::Rootfs>) -> Vec<StepResult> {
    let mut out = Vec::new();

    // only the writable view of this /usr dataset belongs to this
    // cleanup. A mount of another source at the fixed path is not ours.
    match h.mount_table() {
        Err(error) => {
            out.push(step_failed_io(CleanupStep::UnmountWritableMount, error));
        }
        Ok(table) => {
            // Check mounts at and below this path before removal; a foreign child mount could
            // redirect the unlink onto another filesystem.
            let rw = std::path::Path::new(RW_MOUNT);
            let mounts = table
                .iter()
                .filter(|m| {
                    let point = std::path::Path::new(&m.mount_point);
                    point == rw || point.starts_with(rw)
                })
                .collect::<Vec<_>>();
            if !mounts.is_empty() {
                let usr_source = if let Some(rootfs) = rootfs {
                    Some(rootfs.dataset.as_str())
                } else {
                    let mut usr = table
                        .iter()
                        .filter(|m| m.mount_point == "/usr" && m.fstype == "zfs");
                    let first = usr.next();
                    first
                        .filter(|_| usr.next().is_none())
                        .map(|m| m.source.as_str())
                };
                let at_fixed_path = mounts.iter().filter(|m| m.mount_point == RW_MOUNT).count();
                let foreign = mounts
                    .iter()
                    .any(|m| m.fstype != "zfs" || usr_source != Some(m.source.as_str()));
                if at_fixed_path != 1 || foreign {
                    out.push(step_failed(
                        CleanupStep::UnmountWritableMount,
                        "the mount inventory at or below the writable path is not this task's /usr dataset",
                    ));
                } else {
                    // A refused temp deletion must retain the mount for a
                    // later cleanup attempt, not hide the file by umounting.
                    let tmp = h.view(Path::new(RW_TMP_PATH));
                    let mut delete = step_ok(CleanupStep::DeleteTempFile);
                    let mut removed = false;
                    match h.remove_file(&tmp) {
                        Ok(()) => {
                            removed = true;
                            if let Err(e) = h.fsync_dir(tmp.parent().unwrap_or(Path::new("/"))) {
                                delete = step_failed_io(CleanupStep::DeleteTempFile, e);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => delete = step_failed_io(CleanupStep::DeleteTempFile, e),
                    }
                    delete.changed = removed;
                    let deleted = delete.ok;
                    out.push(delete);
                    if deleted {
                        let umount =
                            h.run(&Cmd::new(At::Host, "umount", HOST_CMD_TIMEOUT).arg(RW_MOUNT));
                        if !umount.exit.ok() {
                            out.push(step_failed_cmd(
                                CleanupStep::UnmountWritableMount,
                                &umount,
                                HOST_CMD_TIMEOUT,
                            ));
                        } else {
                            match h.mount_table() {
                                Ok(after)
                                    if after.iter().any(|m| {
                                        Path::new(&m.mount_point).starts_with(RW_MOUNT)
                                    }) =>
                                {
                                    out.push(step_failed(
                                        CleanupStep::UnmountWritableMount,
                                        "writable mount remains after umount",
                                    ));
                                }
                                Err(error) => {
                                    out.push(step_failed_io(
                                        CleanupStep::UnmountWritableMount,
                                        error,
                                    ));
                                }
                                Ok(_) => {
                                    let dir = h.view(Path::new(RW_MOUNT));
                                    let parent =
                                        h.view(Path::new("/run/truenas-nvidia-driver-manager"));
                                    let mut unmount = step_ok(CleanupStep::UnmountWritableMount);
                                    match h.remove_dir_all(&dir) {
                                        Ok(()) => {}
                                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                        Err(e) => {
                                            unmount =
                                                step_failed_io(CleanupStep::UnmountWritableMount, e)
                                        }
                                    }
                                    if unmount.ok {
                                        match h.read_dir(&parent) {
                                            Ok(names) if names.is_empty() => {
                                                if let Err(e) = h.remove_dir_all(&parent) {
                                                    if e.kind() != std::io::ErrorKind::NotFound {
                                                        unmount = step_failed_io(
                                                            CleanupStep::UnmountWritableMount,
                                                            e,
                                                        );
                                                    }
                                                }
                                            }
                                            Ok(_) => {}
                                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                            Err(e) => {
                                                unmount = step_failed_io(
                                                    CleanupStep::UnmountWritableMount,
                                                    e,
                                                )
                                            }
                                        }
                                    }
                                    unmount.changed = true;
                                    out.push(unmount);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(rootfs) = rootfs {
        // Compare observed values so an idempotent reset does not claim a change.
        let query = Cmd::new(At::Host, "zfs", HOST_CMD_TIMEOUT)
            .arg("get")
            .arg("-H")
            .arg("-o")
            .arg("value")
            .arg("readonly")
            .arg(&rootfs.dataset);
        let before = h.run(&query);
        // The `zfs set` result is intentionally not part of the judgement: run it, then
        // decide from the `zfs get` readback below.
        let _ = h.run(
            &Cmd::new(At::Host, "zfs", HOST_CMD_TIMEOUT)
                .arg("set")
                .arg(format!("readonly={}", rootfs.readonly_before))
                .arg(&rootfs.dataset),
        );
        let get = h.run(&query);
        // The judgement is the post-state reading, not the exit code. A `zfs get` that failed
        // or reads back a value other than the recorded `readonly_before` is a failed step,
        // with the error and the full readback kept; the `zfs set` exit code and text are
        // only recorded.
        let reading = get.stdout_text().trim().to_string();
        if get.exit.ok() && reading == rootfs.readonly_before {
            let mut r = step_ok(CleanupStep::RestoreReadonly);
            let previous = before.stdout_text();
            r.changed = before.exit.ok()
                && matches!(previous.trim(), "on" | "off")
                && previous.trim() != reading;
            r.reading = Some(reading);
            out.push(r);
        } else {
            let mut r = if !get.exit.ok() {
                step_failed_cmd(CleanupStep::RestoreReadonly, &get, HOST_CMD_TIMEOUT)
            } else {
                step_failed(
                    CleanupStep::RestoreReadonly,
                    format!(
                        "readonly reads back as {reading}, not {}",
                        rootfs.readonly_before
                    ),
                )
            };
            r.reading = Some(reading);
            out.push(r);
        }
    }

    out
}

/// Log observed host changes and each failure, then one completion only when
/// the task directory was removed. Returns true when every step passed.
fn log_cleanup_steps(id: &str, results: &[StepResult]) -> bool {
    let mut all_ok = true;
    for r in results {
        if r.changed
            && matches!(
                r.step,
                CleanupStep::DeleteTempFile
                    | CleanupStep::UnmountWritableMount
                    | CleanupStep::RestoreReadonly
            )
        {
            crate::log::info(
                Event::HostReset,
                &[("task_id", id), ("step", Event::cleanup_step_code(r.step))],
            );
        }
        if !r.ok {
            all_ok = false;
            crate::log::error_json(
                Event::TaskCleanupStepFailed,
                &[
                    ("task_id", json!(id)),
                    ("step", json!(Event::cleanup_step_code(r.step))),
                    (
                        "reason",
                        r.log_reason.clone().unwrap_or_else(|| {
                            crate::log::reason_object(
                                REASON_IO_ERROR,
                                json!({}),
                                r.reason.as_deref(),
                            )
                        }),
                    ),
                ],
            );
        }
    }
    if results
        .iter()
        .any(|r| r.step == CleanupStep::DeleteTaskDir && r.ok && r.changed)
    {
        crate::log::info(Event::TaskCleanup, &[("task_id", id)]);
    }
    all_ok
}

/// What this tool's own writable view of the `/usr` dataset still shows after
/// the host reset. Only a record whose driver responsibility is not settled
/// carries a read-only value this tool promised to put back, so nothing is
/// judged when there is no such record. A host that cannot be read is reported
/// as a leftover too: the property cannot be proven clean.
fn host_leftovers(h: &dyn Host, union: &RecoveryUnion) -> Option<crate::task_state::ReasonValue> {
    let kept: Vec<&crate::task_state::Rootfs> = union
        .rootfs
        .iter()
        .filter(|(id, _)| union.unresolved.contains(*id))
        .map(|(_, rootfs)| rootfs)
        .collect();
    match h.mount_table() {
        Ok(table) => {
            if table
                .iter()
                .any(|m| Path::new(&m.mount_point).starts_with(RW_MOUNT))
            {
                return Some(crate::task_state::ReasonValue::new(
                    HOST_RW_MOUNT_LEFTOVER,
                    json!({"mount": RW_MOUNT}),
                ));
            }
        }
        Err(_) => {
            return Some(crate::task_state::ReasonValue::new(
                HOST_RW_MOUNT_LEFTOVER,
                json!({"mount": RW_MOUNT}),
            ))
        }
    }
    for rootfs in kept {
        let out = h.run(
            &Cmd::new(At::Host, "zfs", HOST_CMD_TIMEOUT)
                .arg("get")
                .arg("-H")
                .arg("-o")
                .arg("value")
                .arg("readonly")
                .arg(&rootfs.dataset),
        );
        let reading = out.stdout_text().trim().to_string();
        if !out.exit.ok() || reading != rootfs.readonly_before {
            return Some(crate::task_state::ReasonValue::new(
                HOST_READONLY_LEFTOVER,
                json!({
                    "dataset": rootfs.dataset,
                    "reading": if reading.is_empty() { "UNKNOWN".to_string() } else { reading },
                    "expected": rootfs.readonly_before,
                }),
            ));
        }
    }
    None
}

/// A refusal to release retained task inputs; the API localizes the stable key.
#[derive(Debug)]
pub struct ReleaseRefusal {
    pub key: &'static str,
    pub raw: Option<String>,
}

impl ReleaseRefusal {
    fn new(key: &'static str) -> Self {
        Self { key, raw: None }
    }

    fn detail(key: &'static str, raw: impl ToString) -> Self {
        Self {
            key,
            raw: Some(raw.to_string()),
        }
    }
}

/// Release a chosen record while the caller holds the control lock. The execution
/// lock remains held through the proof, durable marker and directory cleanup.
pub fn release_recovery(h: &dyn Host, p: &Paths, id: &str) -> Result<TaskState, ReleaseRefusal> {
    let check_idle = || {
        let check = inventory(h);
        if check.error.is_some() {
            Err(ReleaseRefusal::new("release.state_unknown"))
        } else if !check.running.is_empty() {
            Err(ReleaseRefusal::new("release.task_running"))
        } else {
            Ok(())
        }
    };
    check_idle()?;
    let _execution = crate::admission::try_execution_lock(h)
        .map_err(|_| ReleaseRefusal::new("release.state_unknown"))?
        .ok_or_else(|| ReleaseRefusal::new("release.lock_busy"))?;
    check_idle()?;
    if list_state_ids(h, p).is_err()
        || list_task_dirs(h, p).is_err()
        || !unexplained_state_names(h, p).is_empty()
    {
        return Err(ReleaseRefusal::new("release.state_unknown"));
    }
    let union = recovery_union(h, p);
    // Unlike reconciliation this endpoint never rebuilds a RUNNING record: it
    // cannot establish that the chosen task is terminal by writing it so.
    if !union.running.is_empty() {
        return Err(ReleaseRefusal::new("release.task_running"));
    }
    for (id, rootfs) in &union.rootfs {
        if union.unresolved.contains(id) {
            let results = restore_host(h, Some(rootfs));
            log_cleanup_steps(id, &results);
        }
    }
    let residue = host_leftovers(h, &union);
    let state = release_kept_record(h, p, &union, id, residue.as_ref(), &mut BTreeMap::new())?;
    let results = cleanup(h, p, id, state.rootfs.as_ref());
    if !log_cleanup_steps(id, &results) {
        return Err(ReleaseRefusal::new("release.cleanup_failed"));
    }
    Ok(state)
}

/// The common release decision for the idle reconciliation and the explicit
/// action. Both callers hold the control and execution locks and a complete,
/// fresh idle inventory. Failed proofs are cached only within that locked call.
fn release_kept_record(
    h: &dyn Host,
    p: &Paths,
    union: &RecoveryUnion,
    id: &str,
    residue: Option<&crate::task_state::ReasonValue>,
    proofs: &mut BTreeMap<String, Result<(), String>>,
) -> Result<TaskState, ReleaseRefusal> {
    let state = union
        .states
        .get(id)
        .ok_or_else(|| ReleaseRefusal::new("release.record_unreadable"))?;
    if recovery_material_present(h, p, id) {
        return Err(ReleaseRefusal::new("release.recovery_pending"));
    }
    if state.state_note.is_some() || state.plan.is_none() {
        return Err(ReleaseRefusal::new("release.record_unreadable"));
    }
    if state.status == Status::Running {
        return Err(ReleaseRefusal::new("release.not_terminal"));
    }
    if let Some(reason) = residue {
        return Err(ReleaseRefusal::detail("release.host_residue", &reason.key));
    }
    // A durable marker also makes a failed cleanup retryable without depending
    // on the later library row continuing to exist.
    if state.released_by.is_some() {
        return Ok(state.clone());
    }
    let mut later: Vec<&TaskState> = union
        .states
        .values()
        .filter(|later| {
            later.created_at > state.created_at
                && later.task_id != state.task_id
                && later.plan.is_some()
                && normally_closed(later)
                && !later.replace.is_empty()
                && !union.unresolved.contains(&later.task_id)
        })
        .collect();
    later.sort_by_key(|later| std::cmp::Reverse(later.created_at));
    if later.is_empty() {
        return Err(ReleaseRefusal::new("release.no_later_success"));
    }
    let mut mismatch = String::new();
    for later in later {
        let proof = proofs.entry(later.task_id.clone()).or_insert_with(|| {
            let last = later
                .replace
                .last()
                .expect("the candidate has a replace pass");
            let target = match last.pass {
                crate::task_state::Pass::Restore => later
                    .restore
                    .as_ref()
                    .and_then(|restore| restore.original.as_ref())
                    .map(|original| original.version.as_str()),
                crate::task_state::Pass::Update => {
                    later.plan.as_ref().map(|plan| plan.version.as_str())
                }
            }
            .ok_or("the later task has no target version")?;
            // Cheap physical reads precede both image hashes. A different live
            // version cannot release anything, regardless of image sizes.
            match facts::loaded_version(h, None) {
                facts::LoadedVersion::Value(current) if current.text() == target => {}
                _ => return Err("the loaded kernel driver version is not the target".into()),
            }
            match facts::nvidia_smi_version_now(h) {
                facts::Reading::Value(current) if current == target => {}
                _ => return Err("nvidia-smi does not report the target version".into()),
            }
            canonical_proof(h, p, later, last, false)
        });
        if let Err(reason) = proof {
            mismatch.clone_from(reason);
            continue;
        }
        let mut released = state.clone();
        released.released_by = Some(later.task_id.clone());
        released.released_at = Some(h.wall_ms());
        task_state::write(h, p, &released)
            .map_err(|e| ReleaseRefusal::detail("release.write_failed", e))?;
        crate::log::info(
            Event::TaskReleased,
            &[("task_id", id), ("released_by", &later.task_id)],
        );
        return Ok(released);
    }
    Err(ReleaseRefusal::detail("release.driver_mismatch", mismatch))
}

/// The shared cleanup.
pub fn cleanup(
    h: &dyn Host,
    p: &Paths,
    id: &str,
    rootfs: Option<&crate::task_state::Rootfs>,
) -> Vec<StepResult> {
    let mut out = restore_host(h, rootfs);
    let mut keep_task_dir = out.iter().any(|r| !r.ok);

    // remove the build container. The judgement is a complete exact
    // listing, never the `docker rm` exit code: a removal that reports failure
    // because the container is already gone is a success, and a listing that
    // cannot be trusted is a failure. The command's own exit classification is
    // still kept as the reason when the container really is still there.
    let name = build_container_name(id);
    match list_container_names(h) {
        Ok(mut names) => {
            let mut listing_failed = false;
            let mut removal: Option<Output> = None;
            if names.contains(&name) {
                removal = Some(
                    h.run(
                        &Cmd::new(At::Host, "docker", HOST_CMD_TIMEOUT)
                            .arg("rm")
                            .arg("-f")
                            .arg(&name),
                    ),
                );
                match list_container_names(h) {
                    Ok(after) => names = after,
                    Err(e) => {
                        out.push(step_failed_io(CleanupStep::RemoveBuildContainer, e));
                        keep_task_dir = true;
                        listing_failed = true;
                    }
                }
            }
            if !listing_failed {
                if names.contains(&name) {
                    match removal {
                        // A failed removal keeps the command's exit classification and its
                        // raw text.
                        Some(rm) if !rm.exit.ok() => out.push(step_failed_cmd(
                            CleanupStep::RemoveBuildContainer,
                            &rm,
                            HOST_CMD_TIMEOUT,
                        )),
                        _ => out.push(step_failed(
                            CleanupStep::RemoveBuildContainer,
                            "the build container still exists after removal",
                        )),
                    }
                    keep_task_dir = true;
                } else {
                    // Either it was already gone, or the removal reported a
                    // failure because it was gone: the listing is the proof.
                    out.push(step_ok(CleanupStep::RemoveBuildContainer));
                }
            }
        }
        Err(e) => {
            out.push(step_failed_io(CleanupStep::RemoveBuildContainer, e));
            keep_task_dir = true;
        }
    }

    // delete the state file's temporary files. A refused removal or an unreadable
    // state directory is a failed step, not a success.
    let state_dir = p.state_dir();
    let mut delete_temps = step_ok(CleanupStep::DeleteStateTemps);
    match h.read_dir(&state_dir) {
        Ok(names) => {
            let mut first_error: Option<std::io::Error> = None;
            for name in names {
                let text = name.to_string_lossy();
                // Preserve unknown names; only the shared temporary-file classifier authorizes deletion.
                if is_recognized_state_temp(&text, id) {
                    match h.remove_file(&state_dir.join(&name)) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            if first_error.is_none() {
                                first_error = Some(e);
                            }
                        }
                    }
                }
            }
            if let Some(e) = first_error {
                delete_temps = step_failed_io(CleanupStep::DeleteStateTemps, e);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => delete_temps = step_failed_io(CleanupStep::DeleteStateTemps, e),
    }
    out.push(delete_temps);

    // delete the task directory (the data directory is the same files in the
    // container and on the host). A submission that never ran and could not move its upload
    // group back keeps a marker recording the destination; the only copy of that upload is
    // inside this directory, so the rollback must complete before the directory may be
    // deleted.
    let dir = p.task_dir(id);
    match crate::upload::pending_rollback(h, p, id) {
        Ok(None) => {}
        Ok(Some(group)) => {
            if let Err(e) = crate::upload::finish_pending_rollback(h, p, id, &group) {
                out.push(step_failed_io(CleanupStep::DeleteTaskDir, e));
                return out;
            }
        }
        Err(e) => {
            out.push(step_failed_io(CleanupStep::DeleteTaskDir, e));
            return out;
        }
    }
    // Any necessary recovery step that failed keeps the retry entry: the task
    // directory is the only queue `reconcile` walks, so deleting it would drop
    // the retry for good.
    if out.iter().any(|r| !r.ok) {
        keep_task_dir = true;
    }
    if keep_task_dir {
        out.push(step_failed(
            CleanupStep::DeleteTaskDir,
            "an earlier cleanup step did not complete",
        ));
        return out;
    }
    match h.remove_dir_all(&dir) {
        Ok(()) => {
            let mut deleted = step_ok(CleanupStep::DeleteTaskDir);
            deleted.changed = true;
            out.push(deleted);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.push(step_ok(CleanupStep::DeleteTaskDir))
        }
        Err(e) => out.push(step_failed_io(CleanupStep::DeleteTaskDir, e)),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_units_parses_only_our_units() {
        let s = "nvm-task-20260928T101530Z-3f9a0c12.service loaded active running x\n\
                 nvm-task-bad.service loaded active running x\n\
                 other.service loaded active running x\n\
                 nvm-verify-true.service loaded inactive dead x\n";
        let rows = parse_list_units(s);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].task_id, "20260928T101530Z-3f9a0c12");
        assert!(!unit_stopped(&rows[0]));
    }

    #[test]
    fn show_parsing() {
        let props = parse_show(
            "LoadState=not-found\nActiveState=inactive\nSubState=dead\nResult=success\nExecMainStatus=0\n",
        );
        assert_eq!(props.get("LoadState").unwrap(), "not-found");
        assert_eq!(props.get("ExecMainStatus").unwrap(), "0");
    }
}

/// What one entry of the state directory is. A name this program cannot
/// explain is a leftover that must block a cleanup; it is never deleted.
enum StateEntry {
    Main(String),
    Unreadable(String),
    /// A protected `.{id}.json.recovery-<16 hex>` copy. It is the *only*
    /// evidence of that task when the published record is gone, so its id
    /// enters the union and its responsibility is never treated as closed.
    Recovery(String),
    /// A recognized temporary file of one record (`cleanup` owns it) or a log.
    Ignored,
    Unknown,
}

fn is_sixteen_hex(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// True only for the temporary names of `id` that [`classify_state_entry`] itself recognizes:
/// `.{id}.json.tmp-<16 hex>`, the retired `.{id}.json.tmp-superseded-<16 hex>` copy and their
/// `.log` variants. Every other shape is unknown material and is never deleted by a cleanup.
fn is_recognized_state_temp(name: &str, id: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some(pos) = rest.rfind(".tmp-") else {
        return false;
    };
    let base = &rest[..pos];
    let suffix = &rest[pos + ".tmp-".len()..];
    let hex = suffix.strip_prefix("superseded-").unwrap_or(suffix);
    let base_ok = base == format!("{id}.json") || base == format!("{id}.log");
    base_ok && is_sixteen_hex(hex)
}

/// The names in the state directory that no classification rule explains. As
/// long as one exists the collection is incomplete, the answer is undetermined and no cleanup
/// may run: the raw bytes are the only evidence of what the host was doing. An unreadable
/// directory is handled separately (there is nothing to classify then).
fn unexplained_state_names(h: &dyn Host, p: &Paths) -> Vec<String> {
    match h.read_dir(&p.state_dir()) {
        Ok(names) => names
            .into_iter()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| matches!(classify_state_entry(n), StateEntry::Unknown))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn classify_state_entry(name: &str) -> StateEntry {
    if let Some(id) = task_state::state_file_id(name) {
        return StateEntry::Main(id.to_string());
    }
    if let Some(base) = name.strip_suffix(".json.unreadable") {
        return if store::task_id_ok(base) {
            StateEntry::Unreadable(base.to_string())
        } else {
            StateEntry::Unknown
        };
    }
    if let Some(id) = name.strip_suffix(".log") {
        return if store::task_id_ok(id) {
            StateEntry::Ignored
        } else {
            StateEntry::Unknown
        };
    }
    if let Some(rest) = name.strip_prefix('.') {
        if let Some(pos) = rest.rfind(".recovery-") {
            let base = &rest[..pos];
            let hex = &rest[pos + ".recovery-".len()..];
            if is_sixteen_hex(hex) {
                if let Some(id) = task_state::state_file_id(base) {
                    return StateEntry::Recovery(id.to_string());
                }
                if base.strip_suffix(".log").is_some_and(store::task_id_ok) {
                    return StateEntry::Ignored;
                }
            }
            return StateEntry::Unknown;
        }
        if let Some(pos) = rest.rfind(".tmp-") {
            let base = &rest[..pos];
            let suffix = &rest[pos + ".tmp-".len()..];
            // `.tmp-superseded-<16 hex>` is an ordinary retired copy.
            let hex = suffix.strip_prefix("superseded-").unwrap_or(suffix);
            let base_ok = task_state::state_file_id(base).is_some()
                || base.strip_suffix(".log").is_some_and(store::task_id_ok);
            return if is_sixteen_hex(hex) && base_ok {
                StateEntry::Ignored
            } else {
                StateEntry::Unknown
            };
        }
    }
    StateEntry::Unknown
}

/// The proven recovery information of one id and whether its driver-changing
/// responsibility is resolved. It is computed from the real material in the
/// state directory, never from file names alone.
#[derive(Debug, Default)]
pub struct RecoveryUnion {
    /// Every id any resource class reported (records, sidecars, recovery
    /// copies, task directories, build containers).
    pub ids: BTreeSet<String>,
    /// Ids whose driver-changing responsibility is not proven resolved.
    pub unresolved: BTreeSet<String>,
    /// Material exists but its bytes could not be verified: the id stays
    /// unresolved, the bytes are never guessed from and never deleted.
    pub unverified: BTreeSet<String>,
    /// The `rootfs` recovery information that the actual material proves.
    pub rootfs: BTreeMap<String, crate::task_state::Rootfs>,
    /// Why an id is unresolved, for the operator.
    pub reasons: BTreeMap<String, String>,
    /// Records that are readable right now.
    pub states: BTreeMap<String, TaskState>,
    /// Ids whose readable record says RUNNING.
    pub running: BTreeSet<String>,
}

/// Every protected byte sequence of one record, read from the real files: the
/// `.json.unreadable` sidecar first, then the `.{id}.json.recovery-*` copies
/// (newest name first). A name alone proves nothing, and every candidate is
/// returned so the caller can look for the one that really carries the recovery
/// information instead of stopping at the first unreadable file.
fn recovery_materials(h: &dyn Host, p: &Paths, id: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if let Ok(bytes) = h.read(&p.state_unreadable(id)) {
        out.push(bytes);
    }
    let prefix = format!(".{id}.json.recovery-");
    let mut names: Vec<String> = h
        .read_dir(&p.state_dir())
        .unwrap_or_default()
        .iter()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix))
        .collect();
    names.sort();
    for name in names.iter().rev() {
        if let Ok(bytes) = h.read(&p.state_dir().join(name)) {
            out.push(bytes);
        }
    }
    out
}

/// The recovery information the material really carries. A full record and the
/// reduced `RootfsOnly` shape are both accepted; anything else is unverified.
fn recovery_rootfs(bytes: &[u8]) -> Option<crate::task_state::Rootfs> {
    let only: RootfsOnly = serde_json::from_slice(bytes).ok()?;
    only.rootfs.filter(|r| r.valid())
}

/// Resolve every candidate id from the material that really exists. Record
/// reads and the driver proof come after the complete union is known, so an
/// id that only a build container names is never silently dropped.
pub fn resolve_responsibilities(
    h: &dyn Host,
    p: &Paths,
    candidates: &BTreeSet<String>,
    material: &BTreeSet<String>,
    containers: &BTreeSet<String>,
) -> RecoveryUnion {
    let mut union = RecoveryUnion {
        ids: candidates.clone(),
        ..RecoveryUnion::default()
    };
    for id in candidates {
        match task_state::read(h, p, id) {
            JsonRead::Ok(state) => {
                if state.status == Status::Running {
                    union.running.insert(id.clone());
                }
                if let Some(rootfs) = state.rootfs.as_ref().filter(|r| r.valid()) {
                    union.rootfs.insert(id.clone(), rootfs.clone());
                }
                if let Err(reason) = driver_settled_with(h, p, &state, material.contains(id)) {
                    union.unresolved.insert(id.clone());
                    union.reasons.insert(id.clone(), reason);
                }
                union.states.insert(id.clone(), state);
            }
            _ => {
                // The record itself is not trustworthy. Its protected material
                // may still prove the rootfs recovery information: every real
                // candidate is read and validated, and only when none of them
                // carries a valid rootfs is the id unverified. Unreadable bytes
                // are kept and never guessed from.
                let materials = recovery_materials(h, p, id);
                match materials.iter().find_map(|bytes| recovery_rootfs(bytes)) {
                    Some(rootfs) => {
                        union.rootfs.insert(id.clone(), rootfs);
                    }
                    None => {
                        union.unverified.insert(id.clone());
                    }
                }
                union.unresolved.insert(id.clone());
                union
                    .reasons
                    .entry(id.clone())
                    .or_insert_with(|| "the record is not readable".to_string());
            }
        }
    }
    // A build container names an id of its own: the container is part of the
    // union, and deleting it proves nothing about the driver or rootfs
    // responsibility of an id that has no trustworthy record.
    for name in containers {
        let Some(id) = name.strip_prefix("nvm-build-") else {
            continue;
        };
        if !store::task_id_ok(id) {
            continue;
        }
        union.ids.insert(id.to_string());
        if !union.states.contains_key(id) && union.unresolved.insert(id.to_string()) {
            union.reasons.insert(
                id.to_string(),
                "only a build container names this id".to_string(),
            );
        }
    }
    union
}

/// The recovery union of a host, from one complete enumeration of the state
/// directory, the task directories and the build containers.
pub fn recovery_union(h: &dyn Host, p: &Paths) -> RecoveryUnion {
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    let mut material: BTreeSet<String> = BTreeSet::new();
    if let Ok(names) = h.read_dir(&p.state_dir()) {
        for name in names {
            match classify_state_entry(&name.to_string_lossy()) {
                StateEntry::Main(id) => {
                    candidates.insert(id);
                }
                StateEntry::Unreadable(id) => {
                    candidates.insert(id.clone());
                    material.insert(id);
                }
                StateEntry::Recovery(id) => {
                    candidates.insert(id.clone());
                    material.insert(id);
                }
                StateEntry::Ignored | StateEntry::Unknown => {}
            }
        }
    }
    if let Ok(ids) = list_task_dirs(h, p) {
        candidates.extend(ids);
    }
    let containers: BTreeSet<String> = list_container_names(h).unwrap_or_default();
    resolve_responsibilities(h, p, &candidates, &material, &containers)
}

/// A protected recovery copy of this task's record, or its unreadable sidecar.
/// Both mean the readable record is not the only evidence, so the driver state
/// cannot be proven from it.
fn recovery_material_present(h: &dyn Host, p: &Paths, id: &str) -> bool {
    if h.lstat(&p.state_unreadable(id)).is_ok() {
        return true;
    }
    let prefix = format!(".{id}.json.recovery-");
    h.read_dir(&p.state_dir())
        .map(|names| {
            names
                .iter()
                .any(|n| n.to_string_lossy().starts_with(&prefix))
        })
        .unwrap_or(true)
}

/// The module whose unload command has an unknown effect: the command timed
/// out, was signalled or could not be started, so its kernel side effect cannot
/// be read from the record alone.
fn uncertain_unload_module(state: &TaskState) -> Option<String> {
    let error = state.error.as_ref()?;
    let command = error.command.as_ref()?;
    let program = command.first()?;
    let module = command.last()?;
    if !program.ends_with("rmmod") || command.len() < 2 {
        return None;
    }
    let exit = error.exit.as_ref()?;
    let uncertain = exit.get("timed_out").is_some()
        || exit.get("signal").is_some()
        || exit.get("spawn_failed").is_some();
    let name_ok = !module.is_empty()
        && !module.starts_with('-')
        && module
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    (uncertain && name_ok).then(|| module.clone())
}

/// A task that closed its own flow with a verified effective pass. Its history must not be
/// compared with the current version again: a later normal task is allowed to
/// move the host on. This exempts only already-proven history, never an unfinished
/// responsibility.
fn normally_closed(state: &TaskState) -> bool {
    if state.state_note.is_some() {
        return false;
    }
    if !matches!(state.status, Status::Succeeded | Status::StockRestored) {
        return false;
    }
    match state.replace.last() {
        // No replacement pass at all: nothing was ever touched.
        None => true,
        // A settled pass needs a passing check and positive image readback, bound to the
        // planned update version or the saved original's restore version.
        Some(last) => {
            let Some(check) = state.checks.iter().rfind(|c| c.pass == last.pass) else {
                return false;
            };
            if !check.passed || last.readback_ok != Some(true) {
                return false;
            }
            match last.pass {
                crate::task_state::Pass::Restore => state
                    .restore
                    .as_ref()
                    .and_then(|r| r.original.as_ref())
                    .is_some_and(|original| original.version == check.expected),
                _ => state
                    .plan
                    .as_ref()
                    .is_some_and(|plan| plan.version == check.expected),
            }
        }
    }
}

/// The drain's driver judgement: three positive proofs, all read-only. `Err` means the state
/// cannot be proven settled, so the gate must stay closed and every recovery entry must be
/// kept. It is `pub` because the runner asks the same question about the task it just
/// finished.
pub fn driver_settled(h: &dyn Host, p: &Paths, state: &TaskState) -> Result<(), String> {
    driver_settled_with(h, p, state, false)
}

/// The same judgement when the caller already knows whether protected recovery
/// material exists for this id (the recovery union reads the directory once).
fn driver_settled_with(
    h: &dyn Host,
    p: &Paths,
    state: &TaskState,
    has_recovery_material: bool,
) -> Result<(), String> {
    // The common trust precondition of every path.
    if state.plan.is_none() {
        return Err("the record has no plan".to_string());
    }
    if state.state_note.is_some() {
        return Err("the record was rebuilt from an unreadable file".to_string());
    }
    if has_recovery_material || recovery_material_present(h, p, &state.task_id) {
        return Err("recovery material exists for this record".to_string());
    }
    if state.released_by.is_some() {
        return Ok(());
    }

    // (0) History that already closed normally is kept, not re-judged against
    // the current target: a later normal task may have moved the host on.
    if normally_closed(state) {
        return Ok(());
    }

    // (1) A trusted record with no replacement pass: `unload` persists the
    // record before the first `rmmod`, so this proves the driver was never
    // touched.
    if state.replace.is_empty() {
        return Ok(());
    }
    let last = state.replace.last().expect("the vector is not empty");
    let contradictory = state.replace.iter().any(|r| {
        r.readback_ok.is_some()
            || r.refresh_exit.is_some()
            || r.ldconfig_exit.is_some()
            || r.module_correction.is_some()
    }) || !state.checks.is_empty()
        || state.restore.is_some();

    // (2) Only unloaded, the permanent image untouched.
    if state.rootfs.is_none() && !contradictory {
        return unloaded_only_proof(h, state, last);
    }

    // (3) The permanent image may have been written.
    canonical_proof(h, p, state, last, true)
}

/// The proof for a pass an occupancy reading refused before the first `rmmod`:
/// the pass holds no unloaded and no reloaded module, the recorded error names
/// no uncertain `rmmod` outcome, and the loaded driver version still equals the
/// trusted start reading.
fn stopped_before_unload(
    h: &dyn Host,
    state: &TaskState,
    last: &crate::task_state::ReplaceRecord,
) -> Result<(), String> {
    if !last.unloaded.is_empty() || !last.reloaded.is_empty() {
        return Err("the pass recorded module work before the refusal".to_string());
    }
    if uncertain_unload_module(state).is_some() {
        return Err("the pass holds an unload whose outcome is unknown".to_string());
    }
    let start = match &state.loaded_version_start {
        Some(crate::task_state::ReadingJson::Value { value }) => value.clone(),
        _ => return Err("the record has no trusted start version".to_string()),
    };
    match facts::loaded_version(h, None) {
        facts::LoadedVersion::Value(current) if current.text() == start => Ok(()),
        _ => Err("the loaded driver version is not the recorded start".to_string()),
    }
}

/// The proof for a pass that only unloaded modules: every module of the
/// recovery set is loaded again, and the trusted start version still matches a
/// current reading.
fn unloaded_only_proof(
    h: &dyn Host,
    state: &TaskState,
    last: &crate::task_state::ReplaceRecord,
) -> Result<(), String> {
    if last.unloaded.is_empty() {
        // An occupancy refusal stops the pass before its first `rmmod`:
        // nothing was unloaded, so the target set cannot be in doubt. The
        // proof is the empty pass, the absent uncertain `rmmod` and the
        // unchanged loaded version — never the error code alone.
        if state.status == Status::Failed
            && state
                .error
                .as_ref()
                .and_then(|e| ErrorCode::parse(&e.code))
                .is_some_and(|c| matches!(c, ErrorCode::GpuInUse))
        {
            return stopped_before_unload(h, state, last);
        }
        // `rmmod` succeeded but the record was not written yet: the target set
        // is unknown, so this window cannot be released.
        return Err("the pass unloaded nothing and its target set is unknown".to_string());
    }
    let mut recovery: Vec<String> = last.unloaded.clone();
    if let Some(module) = uncertain_unload_module(state) {
        if !recovery.contains(&module) {
            recovery.push(module);
        }
    }
    for module in &recovery {
        let reloaded = last.reloaded.iter().rfind(|r| r.module == *module);
        match reloaded {
            Some(record) if record.ok => {}
            _ => {
                return Err(format!(
                    "the module {module} was not confirmed loaded again"
                ))
            }
        }
    }
    let modules = match facts::proc_modules_now(h) {
        facts::Reading::Value(modules) => modules,
        facts::Reading::Unknown(_) => {
            return Err("the current module list is unreadable".to_string())
        }
    };
    for module in &recovery {
        if !modules.iter().any(|line| line.name == *module) {
            return Err(format!("the module {module} is not loaded"));
        }
    }
    match &state.loaded_version_start {
        Some(crate::task_state::ReadingJson::Value { value }) => {
            let modules_reading = facts::Reading::Value(modules);
            match facts::loaded_version(h, Some(&modules_reading)) {
                facts::LoadedVersion::Value(current) if current.text() == value => Ok(()),
                _ => Err("the loaded driver version is not the recorded start".to_string()),
            }
        }
        _ => Err("the record has no trusted start version".to_string()),
    }
}

/// The proof for a pass whose permanent-image write may have happened: the
/// target row, the actual file, the loaded module set, the version readings
/// and the merge-time binding must all agree.
fn canonical_proof(
    h: &dyn Host,
    p: &Paths,
    state: &TaskState,
    last: &crate::task_state::ReplaceRecord,
    bind_merge_time: bool,
) -> Result<(), String> {
    use crate::task_state::{Pass, PlanSource, ReadingJson, RestoreDecision, RowKind};

    let plan = state
        .plan
        .as_ref()
        .expect("the trust precondition checked it");
    // The effective last intent, not every historical pass.
    let target: (
        String,
        String,
        RowKind,
        Option<String>,
        Option<u64>,
        Option<String>,
    ) = match last.pass {
        Pass::Restore => {
            let restore = state
                .restore
                .as_ref()
                .ok_or("the last pass is a restore but no restore record exists")?;
            if restore.decision != RestoreDecision::Restore {
                return Err("the restore record does not record a restore decision".to_string());
            }
            let original = restore
                .original
                .as_ref()
                .ok_or("the restore record has no original row")?;
            (
                original.row_id.clone(),
                original.version.clone(),
                RowKind::Original,
                None,
                Some(original.saved_at),
                None,
            )
        }
        Pass::Update => match &plan.source {
            PlanSource::Local {
                row_id,
                row_kind,
                fingerprint,
                sha256,
            } => (
                row_id.clone(),
                plan.version.clone(),
                *row_kind,
                fingerprint.clone(),
                None,
                Some(sha256.clone()),
            ),
            PlanSource::Official => (
                format!("{}-build", state.task_id),
                plan.version.clone(),
                RowKind::Official,
                None,
                None,
                None,
            ),
            PlanSource::Upload { fingerprint, .. } => (
                format!("{}-build", state.task_id),
                plan.version.clone(),
                RowKind::Upload,
                Some(fingerprint.clone()),
                None,
                None,
            ),
        },
    };
    let (row_id, version, kind, fingerprint, saved_at, planned_sha) = target;

    // The target row, read by its exact id through the existing row path.
    let record_path = p.row_dir(&row_id).join("record.json");
    let record = match store::read_json::<crate::drivers::Record>(h, &record_path) {
        JsonRead::Ok(record) if record.kind_matches(&row_id) => record,
        _ => return Err("the target driver record is missing or unreadable".to_string()),
    };
    if record.version != version
        || record.kind != kind
        || record.fingerprint != fingerprint
        || saved_at.is_some_and(|saved| record.created_at != saved)
        || planned_sha.is_some_and(|sha| record.sha256 != sha)
    {
        return Err("the target driver record differs from the recorded intent".to_string());
    }
    // The target image itself must still hash and measure as its record says.
    let row_path = p.row_dir(&row_id).join("nvidia.raw");
    match store::hash_file(h, &row_path) {
        Ok(copied) if copied.sha256 == record.sha256 && copied.size == record.size => {}
        Ok(_) => return Err("the target driver image differs from its record".to_string()),
        Err(_) => return Err("the target driver image is unreadable".to_string()),
    }
    // The permanent image must be exactly that verified target.
    match facts::canonical_sha256(h) {
        facts::Reading::Value(sha) if sha == record.sha256 => {}
        _ => return Err("the permanent image is not the verified target".to_string()),
    }

    // The persisted reload target set must be valid and currently loaded.
    let modules = match facts::proc_modules_now(h) {
        facts::Reading::Value(modules) => modules,
        facts::Reading::Unknown(_) => {
            return Err("the current module list is unreadable".to_string())
        }
    };
    let targets = state
        .reload_targets
        .as_ref()
        .ok_or("the record has no reload target set")?;
    if targets.modules.is_empty() || facts::ReloadBasis::parse(&targets.basis).is_none() {
        return Err("the reload target set is empty or invalid".to_string());
    }
    for module in &targets.modules {
        if !modules.iter().any(|line| line.name == *module) {
            return Err(format!("the reload target {module} is not loaded"));
        }
    }

    // The current physical readings must show the target version.
    let modules_reading = facts::Reading::Value(modules);
    match facts::loaded_version(h, Some(&modules_reading)) {
        facts::LoadedVersion::Value(current) if current.text() == record.version => {}
        _ => return Err("the loaded kernel driver version is not the target".to_string()),
    }
    match facts::nvidia_smi_version_now(h) {
        facts::Reading::Value(current) if current == record.version => {}
        _ => return Err("nvidia-smi does not report the target version".to_string()),
    }

    // The effective pass's own check: a confirmed refresh with two different
    // merge times, and the current extension merge time is exactly the second
    // one.
    let check = state
        .checks
        .iter()
        .rfind(|c| c.pass == last.pass)
        .ok_or("the effective pass has no check record")?;
    if !check.refresh_ok || check.expected != record.version {
        return Err("the effective pass did not confirm the target version".to_string());
    }
    if last.pass == Pass::Restore && !check.passed {
        // A restore is proven by its own passing check; the decision, the original row and a
        // refresh alone are not success.
        return Err("the restore pass has no passing check record".to_string());
    }
    let (ReadingJson::Value { value: before }, ReadingJson::Value { value: after }) =
        (&check.since_before, &check.since_after)
    else {
        return Err("the refresh merge times are not both readings".to_string());
    };
    if before == after {
        return Err("the two merge times are identical".to_string());
    }
    // A normally closed later task already proved its refresh. Releasing an earlier
    // task needs the same image and live driver, even after a reboot re-merged it.
    if !bind_merge_time {
        return Ok(());
    }
    match facts::sysext_nvidia_since_now(h) {
        facts::Reading::Value(current) if current == *after => Ok(()),
        _ => Err("the current extension merge time is not the recorded one".to_string()),
    }
}

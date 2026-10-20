//! Task state files and task logs.
//!
//! The state file is the only authority for a task's outcome. Submission writes the initial
//! RUNNING record atomically; the runner records progress and results. Reconciliation records
//! interruptions and recovery stubs. Recovery release updates retained records after a later
//! task is verified.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::KernelModuleType;
use crate::codes::*;
use crate::facts::ReloadTargets;
use crate::host::{Exit, Host};
use crate::store::{self, JsonRead, Paths, Validate};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Running,
    Succeeded,
    StockRestored,
    Failed,
    Interrupted,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "RUNNING",
            Status::Succeeded => "SUCCEEDED",
            Status::StockRestored => "STOCK_RESTORED",
            Status::Failed => "FAILED",
            Status::Interrupted => "INTERRUPTED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    ReadLoadedVersion,
    SaveOriginal,
    Download,
    Build,
    Unload,
    Write,
    Refresh,
    Reload,
    AlignModules,
    Check,
    RestoreUnload,
    RestoreWrite,
    RestoreRefresh,
    RestoreReload,
    RestoreAlignModules,
    RestoreCheck,
    ReadLoadedVersionEnd,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::ReadLoadedVersion => "read_loaded_version",
            Phase::SaveOriginal => "save_original",
            Phase::Download => "download",
            Phase::Build => "build",
            Phase::Unload => "unload",
            Phase::Write => "write",
            Phase::Refresh => "refresh",
            Phase::Reload => "reload",
            Phase::AlignModules => "align_modules",
            Phase::Check => "check",
            Phase::RestoreUnload => "restore_unload",
            Phase::RestoreWrite => "restore_write",
            Phase::RestoreRefresh => "restore_refresh",
            Phase::RestoreReload => "restore_reload",
            Phase::RestoreAlignModules => "restore_align_modules",
            Phase::RestoreCheck => "restore_check",
            Phase::ReadLoadedVersionEnd => "read_loaded_version_end",
        }
    }

    pub fn parse(s: &str) -> Option<Phase> {
        Some(match s {
            "read_loaded_version" => Phase::ReadLoadedVersion,
            "save_original" => Phase::SaveOriginal,
            "download" => Phase::Download,
            "build" => Phase::Build,
            "unload" => Phase::Unload,
            "write" => Phase::Write,
            "refresh" => Phase::Refresh,
            "reload" => Phase::Reload,
            "align_modules" => Phase::AlignModules,
            "check" => Phase::Check,
            "restore_unload" => Phase::RestoreUnload,
            "restore_write" => Phase::RestoreWrite,
            "restore_refresh" => Phase::RestoreRefresh,
            "restore_reload" => Phase::RestoreReload,
            "restore_align_modules" => Phase::RestoreAlignModules,
            "restore_check" => Phase::RestoreCheck,
            "read_loaded_version_end" => Phase::ReadLoadedVersionEnd,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    Update,
    Restore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    Official,
    Upload,
    Original,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginalChoice {
    Save,
    Skip,
    AlreadySaved,
}

/// `{key, params}` reason objects used across state files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasonValue {
    pub key: String,
    #[serde(default)]
    pub params: Value,
}

impl ReasonValue {
    pub fn new(key: &str, params: Value) -> ReasonValue {
        ReasonValue {
            key: key.to_string(),
            params,
        }
    }
}

/// The two shapes of a command-failure reason: a keyed phrase or the verbatim stderr of the
/// failing command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReasonJson {
    Keyed {
        key: String,
        #[serde(default)]
        params: Value,
    },
    Stderr {
        stderr: String,
    },
}

impl ReasonJson {
    pub fn key(key: &str, params: Value) -> ReasonJson {
        ReasonJson::Keyed {
            key: key.to_string(),
            params,
        }
    }

    pub fn stderr(text: impl Into<String>) -> ReasonJson {
        ReasonJson::Stderr {
            stderr: text.into(),
        }
    }
}

/// A reading of a string value, as stored in state files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReadingJson<T> {
    Value { value: T },
    NotLoaded { not_loaded: bool },
    Unknown { unknown: ReasonValue },
}

impl<T> ReadingJson<T> {
    pub fn value(value: T) -> ReadingJson<T> {
        ReadingJson::Value { value }
    }

    pub fn unknown(key: &str) -> ReadingJson<T> {
        ReadingJson::Unknown {
            unknown: ReasonValue::new(key, json!({})),
        }
    }

    /// An unknown reading whose reason carries placeholders.
    pub fn unknown_with(key: &str, params: Value) -> ReadingJson<T> {
        ReadingJson::Unknown {
            unknown: ReasonValue::new(key, params),
        }
    }

    pub fn as_value(&self) -> Option<&T> {
        match self {
            ReadingJson::Value { value } => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanGpu {
    pub address: String,
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PlanSource {
    Official,
    Upload {
        run_sha256: String,
        fingerprint: String,
        deb_sha256: BTreeMap<String, String>,
    },
    Local {
        row_id: String,
        row_kind: RowKind,
        fingerprint: Option<String>,
        sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub source: PlanSource,
    pub version: String,
    pub kernel: String,
    pub truenas_version: String,
    pub original: OriginalChoice,
    pub image_id: String,
    pub build_timeout_minutes: u32,
    #[serde(default)]
    pub gpus: Vec<PlanGpu>,
    #[serde(default)]
    pub kernel_module_type: Option<KernelModuleType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub done_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rootfs {
    pub dataset: String,
    pub readonly_before: String,
}

impl Rootfs {
    /// Validation of (`dataset` character set and the two accepted `readonly` values).
    pub fn valid(&self) -> bool {
        dataset_ok(&self.dataset) && (self.readonly_before == "on" || self.readonly_before == "off")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadedRecord {
    pub module: String,
    pub ok: bool,
    pub reason: Option<ReasonJson>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleCorrectionTrigger {
    VersionMismatch,
    VersionMissing,
    UnchangedModule,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleUnloadAttempt {
    pub module: String,
    pub exit: Value,
    pub reason: Option<ReasonJson>,
}

/// How a wait for an in-use module ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnloadWaitResult {
    Unloaded,
    Refused,
    Unreadable,
    Other,
}

impl UnloadWaitResult {
    pub fn as_str(self) -> &'static str {
        match self {
            UnloadWaitResult::Unloaded => "unloaded",
            UnloadWaitResult::Refused => "refused",
            UnloadWaitResult::Unreadable => "unreadable",
            UnloadWaitResult::Other => "other",
        }
    }
}

/// Result of a wait that started because the occupancy reading saw the GPU in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccupancyWaitResult {
    Released,
    StillInUse,
}

impl OccupancyWaitResult {
    pub fn as_str(self) -> &'static str {
        match self {
            OccupancyWaitResult::Released => "released",
            OccupancyWaitResult::StillInUse => "still_in_use",
        }
    }
}

/// One occupancy wait. Written only when the first reading was occupied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccupancyWaitRecord {
    pub waited_ms: u64,
    pub result: OccupancyWaitResult,
}

/// One wait after `rmmod` was refused because the module was in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleUnloadWait {
    pub module: String,
    pub waited_ms: u64,
    pub retries: u64,
    pub result: UnloadWaitResult,
}

/// One bounded, post-write correction. It never changes the first round's accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleCorrectionRecord {
    pub trigger: ModuleCorrectionTrigger,
    pub expected: String,
    pub version_before: ReadingJson<String>,
    pub modules_before: Vec<String>,
    pub unloads: Vec<ModuleUnloadAttempt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waits: Vec<ModuleUnloadWait>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub occupancy_waits: Vec<OccupancyWaitRecord>,
    pub reloaded: Vec<ReloadedRecord>,
    pub stop_reason: Option<ReasonJson>,
    pub version_after: Option<ReadingJson<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceRecord {
    pub pass: Pass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_version_before: Option<ReadingJson<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_changed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_correction: Option<ModuleCorrectionRecord>,
    #[serde(default)]
    pub unloaded: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unload_waits: Vec<ModuleUnloadWait>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub occupancy_waits: Vec<OccupancyWaitRecord>,
    #[serde(default)]
    pub reloaded: Vec<ReloadedRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_exit: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ldconfig_exit: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readback_ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readonly_after: Option<ReadingJson<String>>,
}

impl ReplaceRecord {
    /// Notifications describe the final reload round; earlier failures remain historical.
    pub fn final_reloaded(&self) -> &[ReloadedRecord] {
        self.module_correction
            .as_ref()
            .map(|c| c.reloaded.as_slice())
            .unwrap_or(&self.reloaded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRecord {
    pub pass: Pass,
    pub since_before: ReadingJson<u64>,
    pub since_after: ReadingJson<u64>,
    pub refresh_ok: bool,
    pub nvidia_smi: ReadingJson<String>,
    pub proc_version: ReadingJson<String>,
    pub expected: String,
    pub version_ok: bool,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOriginal {
    pub row_id: String,
    pub version: String,
    pub saved_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreDecision {
    Restore,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restore {
    pub decision: RestoreDecision,
    pub reason: Option<String>,
    pub read_reason: Option<String>,
    pub original: Option<RestoreOriginal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorRecord {
    pub code: String,
    pub params: Value,
    pub command: Option<Vec<String>>,
    pub exit: Option<Value>,
    pub raw: Option<String>,
}

impl ErrorRecord {
    pub fn new(code: ErrorCode, params: Value) -> ErrorRecord {
        ErrorRecord {
            code: code.as_str().to_string(),
            params,
            command: None,
            exit: None,
            raw: None,
        }
    }

    pub fn with_command(mut self, argv: Vec<String>, exit: Exit) -> ErrorRecord {
        self.command = Some(argv);
        self.exit = Some(exit.to_json());
        self
    }

    pub fn with_raw(mut self, raw: String) -> ErrorRecord {
        if !raw.is_empty() {
            self.raw = Some(raw);
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuUserRecord {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuUserError {
    #[serde(rename = "type")]
    pub kind: String,
    pub reason: ReasonValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuUsersRecord {
    #[serde(default)]
    pub users: Vec<GpuUserRecord>,
    #[serde(default)]
    pub errors: Vec<GpuUserError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateNote {
    pub state_read_error: String,
    pub kept_as: Option<String>,
}

/// A GPU observed by the post-replacement diagnostic query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedGpu {
    pub name: String,
    pub pci_address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskState {
    pub task_id: String,
    pub status: Status,
    pub phase: Option<Phase>,
    pub created_at: u64,
    pub finished_at: Option<u64>,
    pub plan: Option<Plan>,
    pub loaded_version_start: Option<ReadingJson<String>>,
    pub loaded_version_end: Option<ReadingJson<String>>,
    pub reload_targets: Option<ReloadTargetsJson>,
    pub download: Option<DownloadProgress>,
    #[serde(default)]
    pub rootfs: Option<Rootfs>,
    #[serde(default)]
    pub replace: Vec<ReplaceRecord>,
    #[serde(default)]
    pub checks: Vec<CheckRecord>,
    #[serde(default)]
    pub restore: Option<Restore>,
    #[serde(default)]
    pub error: Option<ErrorRecord>,
    #[serde(default)]
    pub gpu_users: Option<GpuUsersRecord>,
    /// Diagnostic only; never used to decide success or recovery.
    #[serde(default)]
    pub nvidia_smi_gpus: Option<ReadingJson<Vec<ObservedGpu>>>,
    #[serde(default)]
    pub state_note: Option<StateNote>,
    /// The later verified task that released this task's retained directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<u64>,
    /// The build environment: the official image this task pulled from a registry. It is
    /// recorded after the pull, removed when the task ends and cleared here; an image that
    /// was already local (or was loaded offline) is never recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_env_pulled_image: Option<String>,
    /// Set while a GPU wait inside this task has already lasted about three seconds.
    /// Absent once the wait ends, and absent from a terminal record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_gpu: Option<bool>,
}

impl TaskState {
    /// The fresh record reconciliation writes over a record it could not read:
    /// nothing is claimed about the task itself beyond the note that explains
    /// the rewrite, and `rootfs` keeps whatever recovery evidence the replaced
    /// bytes still carried.
    pub fn interrupted_stub(
        id: &str,
        now: u64,
        rootfs: Option<Rootfs>,
        note: StateNote,
    ) -> TaskState {
        TaskState {
            task_id: id.to_string(),
            status: Status::Interrupted,
            phase: None,
            created_at: now,
            finished_at: Some(now),
            plan: None,
            loaded_version_start: None,
            loaded_version_end: None,
            reload_targets: None,
            download: None,
            rootfs,
            replace: Vec::new(),
            checks: Vec::new(),
            restore: None,
            error: None,
            gpu_users: None,
            nvidia_smi_gpus: None,
            state_note: Some(note),
            released_by: None,
            released_at: None,
            build_env_pulled_image: None,
            waiting_gpu: None,
        }
    }
}

/// Serialization form of the reload target set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadTargetsJson {
    pub basis: String,
    pub modules: Vec<String>,
}

impl From<&ReloadTargets> for ReloadTargetsJson {
    fn from(t: &ReloadTargets) -> Self {
        ReloadTargetsJson {
            basis: t.basis.as_str().to_string(),
            modules: t.modules.iter().map(|m| m.to_string()).collect(),
        }
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn image_id_ok(s: &str) -> bool {
    match s.strip_prefix("sha256:") {
        Some(h) => is_hex(h, 64),
        None => false,
    }
}

fn pci_address_ok(s: &str) -> bool {
    let mut it = s.split(':');
    let (Some(domain), Some(bus), Some(dev_func)) = (it.next(), it.next(), it.next()) else {
        return false;
    };
    if it.next().is_some() {
        return false;
    }
    let Some((dev, func)) = dev_func.split_once('.') else {
        return false;
    };
    (4..=8).contains(&domain.len())
        && domain.bytes().all(|b| b.is_ascii_hexdigit())
        && bus.len() == 2
        && bus.bytes().all(|b| b.is_ascii_hexdigit())
        && dev.len() == 2
        && dev.bytes().all(|b| b.is_ascii_hexdigit())
        && func.len() == 1
        && func.bytes().all(|b| (b'0'..=b'7').contains(&b))
}

fn dataset_ok(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/' | ' '))
}

impl Validate for Plan {
    fn validate(&self) -> Result<(), String> {
        if crate::facts::parse_version(&self.version).is_none() {
            return Err("bad version".into());
        }
        if !crate::facts::kernel_ok(&self.kernel) {
            return Err("bad kernel".into());
        }
        if self.truenas_version.is_empty() {
            return Err("bad TrueNAS version".into());
        }
        if !image_id_ok(&self.image_id) {
            return Err("bad image id".into());
        }
        if !(1..=1440).contains(&self.build_timeout_minutes) {
            return Err("bad build timeout".into());
        }
        for gpu in &self.gpus {
            if !pci_address_ok(&gpu.address) {
                return Err("bad GPU address".into());
            }
            if gpu.device.len() != 4
                || !gpu
                    .device
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("bad device id".into());
            }
        }
        match &self.source {
            PlanSource::Official => {}
            PlanSource::Upload {
                run_sha256,
                fingerprint,
                deb_sha256,
            } => {
                if !is_hex(run_sha256, 64) || !is_hex(fingerprint, FINGERPRINT_HEX) {
                    return Err("bad upload checksums".into());
                }
                if deb_sha256.len() != TOOLKIT_PACKAGES.len()
                    || !TOOLKIT_PACKAGES
                        .iter()
                        .all(|p| deb_sha256.get(*p).map(|v| is_hex(v, 64)).unwrap_or(false))
                {
                    return Err("bad toolkit checksums".into());
                }
            }
            PlanSource::Local {
                row_id,
                row_kind,
                fingerprint,
                sha256,
            } => {
                if !store::row_id_ok(row_id) || !is_hex(sha256, 64) {
                    return Err("bad local source".into());
                }
                match row_kind {
                    RowKind::Upload => {
                        if fingerprint.as_deref().map(|f| is_hex(f, FINGERPRINT_HEX)) != Some(true)
                        {
                            return Err("bad local fingerprint".into());
                        }
                    }
                    _ => {
                        if fingerprint.is_some() {
                            return Err("unexpected local fingerprint".into());
                        }
                    }
                }
                if !self.gpus.is_empty() || self.kernel_module_type.is_some() {
                    return Err("local source with GPU fields".into());
                }
            }
        }
        Ok(())
    }
}

impl Validate for TaskState {
    /// Only the fields that steer destructive actions or restore decisions
    /// are checked here: identifiers, checksums, the plan, `rootfs`, the
    /// reload targets and the modules each replace unloaded and reloaded,
    /// which recovery trusts to decide what must be loaded again. Reason
    /// codes and their parameters are rendered, never
    /// executed, so a record written by a newer build — carrying keys this
    /// build does not know — must stay readable instead of being judged
    /// corrupt and rewritten as INTERRUPTED.
    fn validate(&self) -> Result<(), String> {
        if !store::task_id_ok(&self.task_id) {
            return Err("bad task id".into());
        }
        if let Some(id) = &self.released_by {
            if !store::task_id_ok(id) || id == &self.task_id || self.status == Status::Running {
                return Err("bad releasing task".into());
            }
        }
        if self.released_by.is_some() != self.released_at.is_some() {
            return Err("incomplete release marker".into());
        }
        if (self.phase.is_none() || self.plan.is_none())
            && (self.status != Status::Interrupted || self.state_note.is_none())
        {
            return Err("null phase or plan outside an INTERRUPTED note".into());
        }
        if let Some(note) = &self.state_note {
            if let Some(kept) = &note.kept_as {
                if !crate::store::task_id_ok(kept.trim_end_matches(".json.unreadable"))
                    || !kept.ends_with(".json.unreadable")
                {
                    return Err("bad kept_as".into());
                }
            }
        }
        if let Some(plan) = &self.plan {
            plan.validate()?;
        } else if self.status != Status::Interrupted {
            return Err("missing plan".into());
        }
        if let Some(targets) = &self.reload_targets {
            if crate::facts::ReloadBasis::parse(&targets.basis).is_none() {
                return Err("bad reload basis".into());
            }
            let mut seen = std::collections::BTreeSet::new();
            for m in &targets.modules {
                if !crate::facts::DRIVER_MODULES.contains(&m.as_str()) {
                    return Err("unknown module in reload targets".into());
                }
                if !seen.insert(m) {
                    return Err("duplicate module in reload targets".into());
                }
            }
        }
        if let Some(rootfs) = &self.rootfs {
            if !rootfs.valid() {
                return Err("bad dataset".into());
            }
        }
        if let Some(image) = &self.build_env_pulled_image {
            if !image_id_ok(image) {
                return Err("bad build environment image".into());
            }
        }
        for replace in &self.replace {
            for m in &replace.unloaded {
                if !crate::facts::DRIVER_MODULES.contains(&m.as_str()) {
                    return Err("unknown unloaded module".into());
                }
            }
            for wait in &replace.unload_waits {
                if !crate::facts::DRIVER_MODULES.contains(&wait.module.as_str()) {
                    return Err("unknown unload wait module".into());
                }
            }
            if let Some(correction) = &replace.module_correction {
                for module in correction
                    .modules_before
                    .iter()
                    .chain(correction.unloads.iter().map(|a| &a.module))
                {
                    if !crate::facts::DRIVER_MODULES.contains(&module.as_str()) {
                        return Err("unknown correction module".into());
                    }
                }
                for attempt in &correction.unloads {
                    let ok = attempt.exit.get("code").and_then(Value::as_i64) == Some(0);
                    if ok != attempt.reason.is_none() {
                        return Err("correction unload reason must match exit".into());
                    }
                }
                for wait in &correction.waits {
                    if !crate::facts::DRIVER_MODULES.contains(&wait.module.as_str()) {
                        return Err("unknown unload wait module".into());
                    }
                }
            }
            for r in replace
                .reloaded
                .iter()
                .chain(replace.module_correction.iter().flat_map(|c| &c.reloaded))
            {
                if !crate::facts::DRIVER_MODULES.contains(&r.module.as_str()) {
                    return Err("unknown reloaded module".into());
                }
                if r.ok != r.reason.is_none() {
                    return Err("reload reason must match ok".into());
                }
                if matches!(&r.reason, Some(ReasonJson::Stderr { stderr }) if stderr.is_empty()) {
                    return Err("empty stderr reload reason".into());
                }
            }
        }
        if let Some(error) = &self.error {
            if !error.params.is_object() {
                return Err("error params must be an object".into());
            }
        }
        if let Some(restore) = &self.restore {
            match restore.decision {
                RestoreDecision::Restore => {
                    if restore.reason.is_some()
                        || restore.read_reason.is_some()
                        || restore.original.is_none()
                    {
                        return Err("bad restore record".into());
                    }
                }
                RestoreDecision::None => {
                    if restore.reason.as_deref().unwrap_or("").is_empty() {
                        return Err("restore none without reason".into());
                    }
                    if restore.original.is_some() {
                        return Err("restore none with original".into());
                    }
                }
            }
        }
        // SUCCEEDED and STOCK_RESTORED must carry both check readings.
        match self.status {
            Status::Succeeded => {
                let Some(last) = self.checks.last() else {
                    return Err("SUCCEEDED without checks".into());
                };
                if last.pass != Pass::Update || !last.passed {
                    return Err("SUCCEEDED without a passing update check".into());
                }
                require_check_readings(last)?;
            }
            Status::StockRestored => {
                if !self.checks.iter().any(|c| c.pass == Pass::Update) {
                    return Err("STOCK_RESTORED without an update check".into());
                }
                let Some(last) = self.checks.last() else {
                    return Err("STOCK_RESTORED without checks".into());
                };
                if last.pass != Pass::Restore || !last.passed {
                    return Err("STOCK_RESTORED without a passing restore check".into());
                }
                require_check_readings(last)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn require_check_readings(c: &CheckRecord) -> Result<(), String> {
    if !matches!(c.since_before, ReadingJson::Value { .. })
        || !matches!(c.since_after, ReadingJson::Value { .. })
    {
        return Err("check without both merge times".into());
    }
    if !matches!(c.nvidia_smi, ReadingJson::Value { .. })
        || !matches!(c.proc_version, ReadingJson::Value { .. })
    {
        return Err("check without both versions".into());
    }
    Ok(())
}

/// The general reason phrases. A `{key, params}` the runner stores uses one
/// of these keys; read-time validation does not enforce the set, so a record
/// written by a newer build stays readable.
pub const GENERAL_REASON_KEYS: [&str; 13] = [
    REASON_FILE_MISSING,
    REASON_PERMISSION_DENIED,
    REASON_IO_ERROR,
    REASON_FORMAT_UNRECOGNIZED,
    REASON_RECORD_UNREADABLE,
    REASON_TIMED_OUT,
    REASON_EXIT_STATUS,
    REASON_SIGNAL,
    REASON_SPAWN_FAILED,
    REASON_CONNECT_FAILED,
    REASON_HTTP_STATUS,
    REASON_INDEX_UNRECOGNIZED,
    REASON_STARTUP_CHECK_FAILED,
];

/// A general reason with the placeholders a bare key implies in an API response:
/// `reason.timed_out` carries the timeout in seconds.
pub fn general_reason(key: &str, timeout_seconds: u64) -> ReasonValue {
    if key == REASON_TIMED_OUT {
        ReasonValue::new(key, json!({"seconds": timeout_seconds}))
    } else {
        ReasonValue::new(key, json!({}))
    }
}

/// `{key, params}` shape of [`general_reason`].
pub fn general_reason_json(key: &str, timeout_seconds: u64) -> Value {
    let reason = general_reason(key, timeout_seconds);
    json!({"key": reason.key, "params": reason.params})
}

/// Serialize a task state and write it atomically.
pub fn write(h: &dyn Host, p: &Paths, s: &TaskState) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    store::write_atomic(h, &p.state(&s.task_id), &bytes, 0o644)
}

/// Read a task state file with the classification.
pub fn read(h: &dyn Host, p: &Paths, id: &str) -> JsonRead<TaskState> {
    let result = store::read_json::<TaskState>(h, &p.state(id));
    if let JsonRead::Ok(state) = &result {
        if state.task_id != id {
            return JsonRead::Invalid;
        }
    }
    result
}

/// Append one log line.
pub fn log(h: &dyn Host, p: &Paths, id: &str, key: LogKey, params: Value) {
    let line = json!({
        "t": crate::host::Host::wall_ms(h),
        "key": key.as_str(),
        "p": params,
    });
    let path = p.state_log(id);
    if let Ok(mut f) = h.append(&path, 0o644) {
        let mut bytes = line.to_string().into_bytes();
        bytes.push(b'\n');
        use std::io::Write;
        let _ = f.write_all(&bytes);
    }
}

/// One parsed log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub t: u64,
    pub key: String,
    pub p: Value,
}

#[derive(Debug, Clone)]
pub struct LogTail {
    pub lines: Vec<LogLine>,
    pub next: u64,
    pub reset: bool,
}

/// Read a log tail. `after` is a byte offset.
pub fn log_tail(h: &dyn Host, p: &Paths, id: &str, after: Option<u64>) -> LogTail {
    let bytes = match h.read(&p.state_log(id)) {
        Ok(b) => b,
        Err(_) => {
            return LogTail {
                lines: Vec::new(),
                next: 0,
                reset: false,
            }
        }
    };
    let total = bytes.len() as u64;
    let mut reset = false;
    let start = match after {
        Some(pos) if pos > total => {
            reset = true;
            0
        }
        Some(pos) => pos as usize,
        None => 0,
    };
    let mut complete: Vec<(usize, &[u8])> = Vec::new();
    let mut line_start = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            complete.push((line_start, &bytes[line_start..i]));
            line_start = i + 1;
        }
    }
    let next = line_start as u64;
    let mut parsed: Vec<LogLine> = Vec::new();
    if after.is_some() && !reset {
        for (start_pos, line) in &complete {
            if *start_pos < start {
                continue;
            }
            if let Ok(l) = serde_json::from_slice::<LogLine>(line) {
                parsed.push(l);
            }
        }
    } else {
        let mut tmp: Vec<LogLine> = Vec::new();
        for (_, line) in &complete {
            if let Ok(l) = serde_json::from_slice::<LogLine>(line) {
                tmp.push(l);
            }
        }
        let take = tmp.len().saturating_sub(LOG_TAIL_LINES);
        parsed = tmp.split_off(take);
    }
    LogTail {
        lines: parsed,
        next,
        reset,
    }
}

/// Build the first version of a state file.
pub fn new_state(id: &str, created_at: u64, plan: Plan, phase: Phase) -> TaskState {
    TaskState {
        task_id: id.to_string(),
        status: Status::Running,
        phase: Some(phase),
        created_at,
        finished_at: None,
        plan: Some(plan),
        loaded_version_start: None,
        loaded_version_end: None,
        reload_targets: None,
        download: None,
        rootfs: None,
        replace: Vec::new(),
        checks: Vec::new(),
        restore: None,
        error: None,
        gpu_users: None,
        nvidia_smi_gpus: None,
        state_note: None,
        released_by: None,
        released_at: None,
        build_env_pulled_image: None,
        waiting_gpu: None,
    }
}

/// True when the file name of a state file matches the task id format.
pub fn state_file_id(name: &str) -> Option<&str> {
    let base = name.strip_suffix(".json")?;
    if store::task_id_ok(base) {
        Some(base)
    } else {
        None
    }
}

/// The `{key, params}` object used inside `error.params.reason`.
pub fn reason_json(key: &str, params: Value) -> Value {
    json!({ "key": key, "params": params })
}

/// Path helper used by tests and readers.
pub fn state_path(p: &Paths, id: &str) -> std::path::PathBuf {
    p.state(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RealHost;
    use crate::test_support::TestDir;

    fn sample_plan() -> Plan {
        Plan {
            source: PlanSource::Official,
            version: "580.178.04".into(),
            kernel: "6.12.105-production+truenas".into(),
            truenas_version: "25.10.7".into(),
            original: OriginalChoice::Save,
            image_id: format!("sha256:{}", "a".repeat(64)),
            build_timeout_minutes: 30,
            gpus: vec![PlanGpu {
                address: "0000:01:00.0".into(),
                device: "2b85".into(),
            }],
            kernel_module_type: Some(KernelModuleType::Open),
        }
    }

    fn passing_check(pass: Pass, task: &str) -> CheckRecord {
        CheckRecord {
            pass,
            since_before: ReadingJson::value(1),
            since_after: ReadingJson::value(2),
            refresh_ok: true,
            nvidia_smi: ReadingJson::value(task.to_string()),
            proc_version: ReadingJson::value(task.to_string()),
            expected: task.to_string(),
            version_ok: true,
            passed: true,
        }
    }

    #[test]
    fn first_version_serializes_expected_shape() {
        let st = new_state(
            "20260928T101530Z-3f9a0c12",
            1790000000000,
            sample_plan(),
            Phase::ReadLoadedVersion,
        );
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v["status"], "RUNNING");
        assert_eq!(v["phase"], "read_loaded_version");
        assert_eq!(v["plan"]["source"]["kind"], "official");
        assert_eq!(v["plan"]["kernel_module_type"], "open");
        assert_eq!(v["plan"]["gpus"][0]["device"], "2b85");
        assert!(v["loaded_version_start"].is_null());
        assert!(v.get("released_by").is_none());
        assert!(v.get("released_at").is_none());
        let back: TaskState = serde_json::from_value(v).unwrap();
        assert!(back.validate().is_ok());
    }

    #[test]
    fn release_markers_require_a_terminal_record_and_a_distinct_valid_task() {
        let mut state = new_state("20260928T101530Z-3f9a0c12", 1, sample_plan(), Phase::Write);
        state.status = Status::Interrupted;
        state.released_by = Some("20260929T101530Z-3f9a0c12".into());
        state.released_at = Some(2);
        assert!(state.validate().is_ok());
        for id in ["../task", "20260928T101530Z-3f9a0c12"] {
            state.released_by = Some(id.into());
            assert!(state.validate().is_err());
        }
        state.released_by = Some("20260929T101530Z-3f9a0c12".into());
        state.status = Status::Running;
        assert!(state.validate().is_err());
        state.status = Status::Interrupted;
        state.released_at = None;
        assert!(state.validate().is_err());
        state.released_by = None;
        state.released_at = Some(2);
        assert!(state.validate().is_err());
    }

    #[test]
    fn succeeded_requires_both_readings() {
        let mut st = new_state("20260928T101530Z-3f9a0c12", 1, sample_plan(), Phase::Check);
        st.status = Status::Succeeded;
        assert!(st.validate().is_err());
        st.checks.push(passing_check(Pass::Update, "580.178.04"));
        assert!(st.validate().is_ok());
        st.checks[0].since_after = ReadingJson::unknown(REASON_IO_ERROR);
        assert!(st.validate().is_err());
    }

    #[test]
    fn stock_restored_requires_restore_check() {
        let mut st = new_state("20260928T101530Z-3f9a0c12", 1, sample_plan(), Phase::Check);
        st.status = Status::StockRestored;
        st.checks.push(passing_check(Pass::Update, "580.178.04"));
        assert!(st.validate().is_err());
        st.checks.push(passing_check(Pass::Restore, "580.173.02"));
        assert!(st.validate().is_ok());
    }

    #[test]
    fn existing_task_without_gpu_diagnostic_still_deserializes() {
        let state = new_state("20260928T101530Z-3f9a0c12", 1, sample_plan(), Phase::Check);
        let mut value = serde_json::to_value(&state).unwrap();
        value.as_object_mut().unwrap().remove("nvidia_smi_gpus");
        let decoded: TaskState = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, state);
        assert!(decoded.validate().is_ok());
    }

    #[test]
    fn interrupted_note_allows_null_plan() {
        let mut st = new_state("20260928T101530Z-3f9a0c12", 1, sample_plan(), Phase::Check);
        st.status = Status::Interrupted;
        st.plan = None;
        st.phase = None;
        st.state_note = Some(StateNote {
            state_read_error: "missing".into(),
            kept_as: Some("20260928T101530Z-3f9a0c12.json.unreadable".into()),
        });
        assert!(st.validate().is_ok());
        st.state_note = None;
        assert!(st.validate().is_err());
    }

    #[test]
    fn unknown_reason_codes_stay_readable() {
        // A record written by a newer build may carry reason codes and
        // parameters this build has never heard of: it must stay readable,
        // or a rollback would rewrite every newer task as INTERRUPTED.
        let mut st = new_state("20260928T101530Z-3f9a0c12", 1, sample_plan(), Phase::Check);
        st.status = Status::Failed;
        st.finished_at = Some(2);
        st.loaded_version_start = Some(ReadingJson::unknown("reason.brand_new_cause"));
        st.gpu_users = Some(GpuUsersRecord {
            users: Vec::new(),
            errors: vec![GpuUserError {
                kind: "docker".into(),
                reason: ReasonValue::new("reason.brand_new_cause", json!({"detail": 7})),
            }],
        });
        st.error = Some(ErrorRecord {
            code: "error.brand_new_failure".into(),
            params: json!({"reason": {"key": "build.brand_new", "params": {"x": "y"}}}),
            command: None,
            exit: None,
            raw: None,
        });
        st.restore = Some(Restore {
            decision: RestoreDecision::None,
            reason: Some("restore.brand_new_cause".into()),
            read_reason: Some("reason.brand_new_read".into()),
            original: None,
        });
        st.replace.push(ReplaceRecord {
            pass: Pass::Update,
            module_version_before: None,
            file_changed: None,
            module_correction: None,
            unloaded: vec!["nvidia".into()],
            unload_waits: Vec::new(),
            occupancy_waits: Vec::new(),
            reloaded: vec![ReloadedRecord {
                module: "nvidia".into(),
                ok: false,
                reason: Some(ReasonJson::key("build.brand_new", json!({"n": 1}))),
            }],
            refresh_exit: None,
            ldconfig_exit: None,
            readback_ok: None,
            readonly_after: None,
        });
        assert!(st.validate().is_ok());

        // Recovery trusts these module names and the ok/reason pairing, so
        // they stay checked even though the reason vocabulary is open.
        let valid = st.clone();
        st.replace[0].unloaded = vec!["nvme".into()];
        assert!(
            st.validate().is_err(),
            "unloaded module outside the driver set"
        );
        st = valid.clone();
        st.replace[0].reloaded[0].module = "nvme".into();
        assert!(
            st.validate().is_err(),
            "reloaded module outside the driver set"
        );
        st = valid.clone();
        st.replace[0].reloaded[0].ok = true;
        assert!(
            st.validate().is_err(),
            "a confirmed reload carries no reason"
        );
        st = valid.clone();
        st.replace[0].reloaded[0].reason = None;
        assert!(st.validate().is_err(), "a failed reload needs its reason");
        st = valid;
        st.replace[0].reloaded[0].reason = Some(ReasonJson::stderr(""));
        assert!(
            st.validate().is_err(),
            "an empty stderr reason says nothing"
        );
    }

    #[test]
    fn restore_and_error_shapes_still_hold() {
        let mut st = new_state("20260928T101530Z-3f9a0c12", 1, sample_plan(), Phase::Check);
        st.restore = Some(Restore {
            decision: RestoreDecision::None,
            reason: None,
            read_reason: None,
            original: None,
        });
        assert!(st.validate().is_err(), "restore none needs a reason");
        st.restore = Some(Restore {
            decision: RestoreDecision::None,
            reason: Some("restore.any_cause".into()),
            read_reason: None,
            original: Some(RestoreOriginal {
                row_id: "20260928T101530Z-3f9a0c12-original".into(),
                version: "580.173.02".into(),
                saved_at: 1,
            }),
        });
        assert!(
            st.validate().is_err(),
            "restore none cannot name an original"
        );
        st.restore = Some(Restore {
            decision: RestoreDecision::None,
            reason: Some("restore.any_cause".into()),
            read_reason: None,
            original: None,
        });
        assert!(st.validate().is_ok());
        st.restore = Some(Restore {
            decision: RestoreDecision::Restore,
            reason: None,
            read_reason: None,
            original: None,
        });
        assert!(st.validate().is_err(), "restore needs the original");
        st.restore = None;
        st.error = Some(ErrorRecord {
            code: "error.brand_new_failure".into(),
            params: json!([1]),
            command: None,
            exit: None,
            raw: None,
        });
        assert!(st.validate().is_err(), "error params must be an object");
    }

    #[test]
    fn plan_source_validation() {
        let mut st = new_state("20260928T101530Z-3f9a0c12", 1, sample_plan(), Phase::Check);
        st.plan.as_mut().unwrap().source = PlanSource::Upload {
            run_sha256: "a".repeat(64),
            fingerprint: "a".repeat(12),
            deb_sha256: TOOLKIT_PACKAGES
                .iter()
                .map(|p| (p.to_string(), "b".repeat(64)))
                .collect(),
        };
        assert!(st.validate().is_ok());
        st.plan.as_mut().unwrap().source = PlanSource::Local {
            row_id: "20260928T101530Z-3f9a0c12-build".into(),
            row_kind: RowKind::Upload,
            fingerprint: Some("a".repeat(12)),
            sha256: "a".repeat(64),
        };
        st.plan.as_mut().unwrap().gpus.clear();
        st.plan.as_mut().unwrap().kernel_module_type = None;
        assert!(st.validate().is_ok());
        st.plan.as_mut().unwrap().kernel_module_type = Some(KernelModuleType::Open);
        assert!(st.validate().is_err());
    }

    #[test]
    fn signal_reasons_in_four_state_fields_survive_write_and_read() {
        let tmp = TestDir::new("signal-state");
        let p = Paths::new(&*tmp);
        let h = RealHost::on_host();
        store::prepare_dirs(&h, &p).unwrap();
        let id = "20260928T101530Z-3f9a0c12";
        let signal = json!({"key": REASON_SIGNAL, "params": {"signal": 15}});
        let mut st = new_state(id, 1, sample_plan(), Phase::Build);
        st.status = Status::Failed;
        st.finished_at = Some(2);
        let assert_readback = |state: &TaskState| {
            assert!(state.validate().is_ok());
            write(&h, &p, state).unwrap();
            match read(&h, &p, id) {
                JsonRead::Ok(back) => assert_eq!(back, *state),
                other => panic!("signal state must read back as valid: {other:?}"),
            }
        };

        st.error = Some(ErrorRecord::new(
            ErrorCode::BuildFailed,
            json!({"reason": signal}),
        ));
        assert_readback(&st);
        st.error = None;
        st.replace.push(ReplaceRecord {
            pass: Pass::Update,
            module_version_before: None,
            file_changed: None,
            module_correction: None,
            unloaded: vec!["nvidia".into()],
            unload_waits: Vec::new(),
            occupancy_waits: Vec::new(),
            reloaded: vec![ReloadedRecord {
                module: "nvidia".into(),
                ok: false,
                reason: Some(ReasonJson::key(REASON_SIGNAL, json!({"signal": 15}))),
            }],
            refresh_exit: None,
            ldconfig_exit: None,
            readback_ok: None,
            readonly_after: None,
        });
        assert_readback(&st);
        st.checks.push(CheckRecord {
            pass: Pass::Update,
            since_before: ReadingJson::unknown_with(REASON_SIGNAL, json!({"signal": 15})),
            since_after: ReadingJson::value(2),
            refresh_ok: false,
            nvidia_smi: ReadingJson::unknown_with(REASON_SIGNAL, json!({"signal": 15})),
            proc_version: ReadingJson::value("580.178.04".into()),
            expected: "580.178.04".into(),
            version_ok: false,
            passed: false,
        });
        assert_readback(&st);
        st.error = Some(ErrorRecord::new(
            ErrorCode::ModuleUnloadRefused,
            json!({"module": "nvidia", "unload_reason": signal}),
        ));
        assert_readback(&st);
    }

    #[test]
    fn a_stderr_reload_reason_is_accepted() {
        let mut st = new_state("20260928T101530Z-3f9a0c12", 1, sample_plan(), Phase::Reload);
        st.replace.push(ReplaceRecord {
            pass: Pass::Update,
            module_version_before: None,
            file_changed: None,
            module_correction: None,
            unloaded: vec!["nvidia".into()],
            unload_waits: Vec::new(),
            occupancy_waits: Vec::new(),
            reloaded: vec![ReloadedRecord {
                module: "nvidia".into(),
                ok: false,
                reason: Some(ReasonJson::stderr(
                    "modprobe: ERROR: could not insert nvidia",
                )),
            }],
            refresh_exit: None,
            ldconfig_exit: None,
            readback_ok: None,
            readonly_after: None,
        });
        assert!(st.validate().is_ok());
        let value = serde_json::to_value(&st).unwrap();
        assert_eq!(
            value["replace"][0]["reloaded"][0]["reason"]["stderr"],
            "modprobe: ERROR: could not insert nvidia"
        );
        let parsed: TaskState = serde_json::from_value(value).unwrap();
        assert!(parsed.validate().is_ok());
    }

    #[test]
    fn write_read_and_log_tail_roundtrip() {
        let tmp = TestDir::new("state");
        let p = Paths::new(&*tmp);
        store::prepare_dirs(&RealHost::on_host(), &p).unwrap();
        let h = RealHost::on_host();
        let id = "20260928T101530Z-3f9a0c12";
        let st = new_state(id, 1, sample_plan(), Phase::ReadLoadedVersion);
        write(&h, &p, &st).unwrap();
        let read = read(&h, &p, id);
        assert!(read.is_ok(), "{read:?}");
        for i in 0..5 {
            log(
                &h,
                &p,
                id,
                LogKey::PhaseEnter,
                json!({"phase": format!("p{i}")}),
            );
        }
        let tail = log_tail(&h, &p, id, None);
        assert_eq!(tail.lines.len(), 5);
        assert_eq!(tail.next, std::fs::metadata(p.state_log(id)).unwrap().len());
        let after = tail.lines[2].t;
        let _ = after;
        let half = log_tail(&h, &p, id, Some(tail.next / 2));
        assert!(half.lines.len() <= 5);
    }

    #[test]
    fn missing_read_is_classified() {
        let tmp = TestDir::new("state-missing");
        let p = Paths::new(&*tmp);
        store::prepare_dirs(&RealHost::on_host(), &p).unwrap();
        let read = read(&RealHost::on_host(), &p, "20260928T101530Z-3f9a0c12");
        assert!(matches!(read, JsonRead::Missing));
    }
}

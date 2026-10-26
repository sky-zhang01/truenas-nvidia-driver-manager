//! Pre-update checks, the confirmation dialog, the one-shot plan slot and submission.

use std::sync::Arc;

use serde_json::json;

use crate::catalog::{self, KernelModuleType};
use crate::codes::*;
use crate::drivers;
use crate::facts::{self, Reading};
use crate::host::Host;
use crate::settings::Settings;
use crate::startup::Startup;
use crate::store::Paths;
use crate::task_state::{OriginalChoice, Plan, PlanGpu, PlanSource, RowKind};
use crate::upload::{self, UploadState};

/// Everything the request handlers share.
pub struct Ctx {
    pub host: Arc<dyn Host>,
    pub paths: Paths,
    pub settings: Settings,
    pub startup: Startup,
    /// The process-wide lock.
    pub process_lock: tokio::sync::Mutex<()>,
    pub plan: std::sync::Mutex<PlanSlot>,
    /// Independent single-flight query and short-lived GPU snapshot.
    pub gpu_users_cache: std::sync::Mutex<Option<(std::time::Instant, GpuState)>>,
    pub upload: Arc<UploadState>,
    pub auth: std::sync::Arc<crate::auth::AuthState>,
}

impl Ctx {
    pub fn gpu_users_snapshot(&self) -> GpuState {
        let mut cache = self.gpu_users_cache.lock().unwrap();
        if let Some((at, snapshot)) = cache.as_ref() {
            if at.elapsed() < std::time::Duration::from_millis(1500) {
                return snapshot.clone();
            }
        }
        let users = crate::gpu_users::list(self.host.as_ref(), HOST_QUERY_TIMEOUT);
        let (state, read_reason) = match facts::read_modules(self.host.as_ref()) {
            Reading::Value(modules) => (
                if facts::occupancy(&modules) {
                    "busy"
                } else {
                    "idle"
                },
                None,
            ),
            Reading::Unknown(reason) => (
                "unreadable",
                Some(crate::task_state::general_reason_json(
                    reason,
                    READING_TIMEOUT.as_secs(),
                )),
            ),
        };
        let snapshot = GpuState {
            checked_at: self.host.wall_ms(),
            state,
            users: users.users.iter().map(Into::into).collect(),
            errors: users.errors.iter().map(Into::into).collect(),
            read_reason,
        };
        *cache = Some((std::time::Instant::now(), snapshot.clone()));
        snapshot
    }

    pub fn build_timeout_minutes(&self) -> u32 {
        self.settings
            .build_timeout_minutes
            .unwrap_or(crate::settings::DEFAULT_BUILD_TIMEOUT_MINUTES)
    }

    pub fn setup_problems(&self) -> bool {
        !self.settings.problems.is_empty() || !self.startup.problems.is_empty()
    }
}

/// What the user asked for in plan creation. The shape is closed: unknown fields, a missing
/// `gpus` or an unknown `kind` are rejected by the JSON layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRequest {
    Official {
        version: String,
        gpus: Vec<String>,
    },
    Upload {
        group_id: String,
        version: Option<String>,
        gpus: Vec<String>,
    },
    Local {
        row_id: String,
    },
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OfficialRequest {
    #[allow(dead_code)]
    kind: String,
    version: String,
    gpus: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadRequest {
    #[allow(dead_code)]
    kind: String,
    group_id: String,
    #[serde(default)]
    version: Option<String>,
    gpus: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalRequest {
    #[allow(dead_code)]
    kind: String,
    row_id: String,
}

impl<'de> serde::Deserialize<'de> for SourceRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = value
            .as_object()
            .ok_or_else(|| D::Error::custom("source must be an object"))?;
        let kind = obj
            .get("kind")
            .and_then(|k| k.as_str())
            .ok_or_else(|| D::Error::custom("source.kind is required"))?;
        let allowed: &[&str] = match kind {
            "official" => &["kind", "version", "gpus"],
            "upload" => &["kind", "group_id", "version", "gpus"],
            "local" => &["kind", "row_id"],
            _ => return Err(D::Error::custom("unknown source.kind")),
        };
        for key in obj.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(D::Error::custom(format!("unknown field {key}")));
            }
        }
        match kind {
            "official" => {
                let req: OfficialRequest =
                    serde_json::from_value(value).map_err(D::Error::custom)?;
                if !gpus_request_ok(&req.gpus) {
                    return Err(D::Error::custom(
                        "source.gpus is not a list of PCI addresses",
                    ));
                }
                Ok(SourceRequest::Official {
                    version: req.version,
                    gpus: req.gpus,
                })
            }
            "upload" => {
                let req: UploadRequest = serde_json::from_value(value).map_err(D::Error::custom)?;
                if !gpus_request_ok(&req.gpus) {
                    return Err(D::Error::custom(
                        "source.gpus is not a list of PCI addresses",
                    ));
                }
                Ok(SourceRequest::Upload {
                    group_id: req.group_id,
                    version: req.version,
                    gpus: req.gpus,
                })
            }
            "local" => {
                let req: LocalRequest = serde_json::from_value(value).map_err(D::Error::custom)?;
                Ok(SourceRequest::Local { row_id: req.row_id })
            }
            _ => unreachable!(),
        }
    }
}

pub use crate::facts::pci_address_ok;

fn gpus_request_ok(gpus: &[String]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    gpus.iter()
        .all(|g| pci_address_ok(g) && seen.insert(g.as_str()))
}

/// plan creation rejections with their response data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceInvalid {
    SetupBlocked,
    RequestInvalid,
    VersionFormat,
    VersionBelow470,
    BranchUnbuildable {
        version: String,
        from: Option<String>,
        reasons: Vec<&'static str>,
    },
    VersionNotOfficial {
        version: String,
    },
    VersionUnconfirmed {
        version: String,
        unreadable: Vec<serde_json::Value>,
    },
    BranchUndecidable {
        unreadable: Vec<Unreadable>,
    },
    GpuUndecidable {
        unreadable: Vec<Unreadable>,
    },
    GpuUnsupported {
        version: Option<String>,
        gpus: serde_json::Value,
        conflict: bool,
    },
    GpusChanged,
    UploadGone,
    UploadBusy,
    UploadIncomplete {
        missing: Vec<String>,
    },
    UploadProblem,
    UploadVersionRequired,
    UploadVersionNotCandidate {
        version: String,
    },
    ToolkitTooOld {
        driver_version: String,
        toolkit_version: String,
    },
    LocalRowMissing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub object: &'static str,
    pub reason: crate::task_state::ReasonValue,
    pub raw: Option<String>,
}

impl Unreadable {
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("object".into(), json!(self.object));
        obj.insert(
            "reason".into(),
            json!({"key": self.reason.key, "params": self.reason.params}),
        );
        if let Some(raw) = &self.raw {
            obj.insert("raw".into(), json!(raw));
        }
        serde_json::Value::Object(obj)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Failure {
    pub item: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_kernel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_kernel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_kind: Option<String>,
}

impl Failure {
    fn new(item: &'static str) -> Self {
        Self {
            item,
            object: None,
            reason: None,
            target: None,
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        }
    }

    fn unreadable(object: &'static str, reason: serde_json::Value) -> Self {
        Self {
            object: Some(object),
            reason: Some(reason),
            ..Self::new("unreadable")
        }
    }
}

/// Reject unrecovered host changes before another task can adopt them as its baseline.
fn host_residue_failure(residue: &crate::task_state::ReasonValue) -> Failure {
    Failure {
        item: "host_not_restored",
        object: None,
        reason: Some(json!({"key": residue.key, "params": residue.params})),
        target: None,
        target_kernel: None,
        current_kernel: None,
        row_kind: None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GpuState {
    pub checked_at: u64,
    pub state: &'static str,
    pub users: Vec<crate::gpu_users::GpuUserJson>,
    pub errors: Vec<crate::gpu_users::GpuUserErrorJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_reason: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConfirmationButton {
    pub button: &'static str,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Confirmation {
    pub original: &'static str,
    pub buttons: Vec<ConfirmationButton>,
    pub how: crate::task_state::ReasonValue,
    pub warnings: Vec<crate::task_state::ReasonValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Dialog {
    pub source: serde_json::Value,
    pub version: String,
    pub kernel: String,
    pub truenas_version: String,
    pub original: serde_json::Value,
    pub save_available: bool,
    pub confirmation: Confirmation,
}

#[derive(Debug, Clone)]
pub enum Precheck {
    Ready(Dialog),
    Blocked {
        failures: Vec<Failure>,
        gpu: Option<GpuState>,
    },
    Uncomputable {
        items: Vec<Unreadable>,
    },
}

/// The one-shot snapshot kept in memory.
#[derive(Debug, Clone, Default)]
pub struct PlanSlot(pub Option<Snapshot>);

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: String,
    pub source: PlanSource,
    pub version: String,
    pub kernel: String,
    pub truenas_version: String,
    pub original: OriginalChoice,
    pub original_saved: bool,
    pub save_available: bool,
    pub link: facts::LinkReading,
    pub gpus: Vec<PlanGpu>,
    pub kernel_module_type: Option<KernelModuleType>,
    pub upload_group: Option<String>,
    pub local_row: Option<String>,
}

impl PlanSlot {
    pub fn new() -> PlanSlot {
        PlanSlot(None)
    }

    pub fn remember(&mut self, snapshot: Snapshot) -> String {
        let id = crate::store::random_hex(16);
        let mut snapshot = snapshot;
        snapshot.id = id.clone();
        self.0 = Some(snapshot);
        id
    }

    /// Take the snapshot if the id matches; a plan is used at most once.
    pub fn take(&mut self, id: &str) -> Option<Snapshot> {
        match &self.0 {
            Some(s) if s.id == id => self.0.take(),
            _ => None,
        }
    }
}

fn gpu_busy(h: &dyn Host) -> GpuState {
    let users = crate::gpu_users::list(h, HOST_QUERY_TIMEOUT);
    GpuState {
        checked_at: h.wall_ms(),
        state: "busy",
        users: users.users.iter().map(Into::into).collect(),
        errors: users.errors.iter().map(Into::into).collect(),
        read_reason: None,
    }
}

fn gpu_unreadable(h: &dyn Host, reason: &'static str) -> GpuState {
    let users = crate::gpu_users::list(h, HOST_QUERY_TIMEOUT);
    GpuState {
        checked_at: h.wall_ms(),
        state: "unreadable",
        users: users.users.iter().map(Into::into).collect(),
        errors: users.errors.iter().map(Into::into).collect(),
        read_reason: Some(crate::task_state::general_reason_json(
            reason,
            READING_TIMEOUT.as_secs(),
        )),
    }
}

/// The GPU check shared by the official and upload sources.
pub fn check_gpus(
    request_gpus: &[String],
    host_gpus: &[facts::HostGpu],
) -> Result<Vec<facts::HostGpu>, SourceInvalid> {
    let counted: Vec<&facts::HostGpu> = facts::counted(host_gpus);
    if request_gpus.is_empty() {
        if counted.is_empty() {
            return Ok(Vec::new());
        }
        return Err(SourceInvalid::GpusChanged);
    }
    let mut selected = Vec::new();
    for address in request_gpus {
        let Some(gpu) = counted.iter().find(|g| &g.address == address) else {
            return Err(SourceInvalid::GpusChanged);
        };
        selected.push((*gpu).clone());
    }
    Ok(selected)
}

fn check_branch(
    version: &facts::Version,
    kernel: &str,
    interfaces: &catalog::KernelInterfaces,
    module_type: Option<KernelModuleType>,
    loaded: &Reading<Vec<facts::ModuleLine>>,
) -> Result<(), SourceInvalid> {
    match catalog::official_verdict(version, kernel, interfaces, module_type) {
        catalog::BranchVerdict::Unbuildable(reasons) => Err(SourceInvalid::BranchUnbuildable {
            version: version.text().to_string(),
            from: catalog::branch_range(version, kernel, interfaces, loaded, module_type),
            reasons,
        }),
        catalog::BranchVerdict::Undecidable => Err(SourceInvalid::BranchUndecidable {
            unreadable: catalog::HeadersUnreadable::for_branch(version, interfaces, module_type)
                .items
                .into_iter()
                .map(|i| Unreadable {
                    object: "kernel_headers",
                    reason: crate::task_state::general_reason(i.reason, READING_TIMEOUT.as_secs()),
                    raw: Some(i.path.to_string()),
                })
                .collect(),
        }),
        catalog::BranchVerdict::Buildable => Ok(()),
    }
}

fn read_source_gpus(h: &dyn Host) -> Result<Vec<facts::HostGpu>, SourceInvalid> {
    let gpus = match facts::read_gpus(h) {
        Ok(g) => g,
        Err(e) => {
            crate::log::error_json(
                Event::GpusUnreadable,
                &[
                    ("path", json!(e.path.as_str())),
                    (
                        "reason",
                        crate::task_state::general_reason_json(e.reason, READING_TIMEOUT.as_secs()),
                    ),
                ],
            );
            return Err(SourceInvalid::GpuUndecidable {
                unreadable: vec![Unreadable {
                    object: "gpus",
                    reason: crate::task_state::general_reason(e.reason, READING_TIMEOUT.as_secs()),
                    raw: Some(e.path),
                }],
            });
        }
    };
    Ok(gpus)
}

fn plan_gpus(selected: &[facts::HostGpu]) -> Vec<PlanGpu> {
    selected
        .iter()
        .map(|g| PlanGpu {
            address: g.address.clone(),
            device: format!("{:04x}", g.device),
        })
        .collect()
}

fn prepare_official_source(
    h: &dyn Host,
    version: &str,
    gpus: &[String],
    snapshot: &mut Snapshot,
    modules_for_occupancy: &mut Option<Reading<Vec<facts::ModuleLine>>>,
    official_kernel: &mut Option<String>,
    official_versions: &Result<Vec<facts::Version>, serde_json::Value>,
) -> Result<Option<Precheck>, SourceInvalid> {
    if !gpus_request_ok(gpus) {
        return Err(SourceInvalid::RequestInvalid);
    }
    let Some(parsed) = facts::parse_version_exact(version) else {
        return Err(SourceInvalid::VersionFormat);
    };
    if parsed.major() < MIN_MAJOR_BRANCH {
        return Err(SourceInvalid::VersionBelow470);
    }
    if parsed.major() == 470 {
        match facts::kernel(h) {
            Reading::Value(kernel) => *official_kernel = Some(kernel),
            Reading::Unknown(reason) => {
                return Ok(Some(Precheck::Uncomputable {
                    items: vec![Unreadable {
                        object: "current_kernel",
                        reason: crate::task_state::general_reason(
                            reason,
                            READING_TIMEOUT.as_secs(),
                        ),
                        raw: None,
                    }],
                }));
            }
        }
    }
    let kernel = official_kernel.as_deref().unwrap_or_default();
    let interfaces = catalog::read_interfaces(h);
    check_branch(
        &parsed,
        kernel,
        &interfaces,
        None,
        &Reading::Unknown(REASON_IO_ERROR),
    )?;
    let loaded = facts::read_modules(h);
    let reasons = catalog::official_module_reasons(&parsed, kernel, &loaded);
    if !reasons.is_empty() {
        return Err(SourceInvalid::BranchUnbuildable {
            version: version.to_string(),
            from: catalog::branch_range(&parsed, kernel, &interfaces, &loaded, None),
            reasons,
        });
    }
    *modules_for_occupancy = Some(loaded.clone());
    let host_gpus = read_source_gpus(h)?;
    let selected = check_gpus(gpus, &host_gpus)?;
    let devices: Vec<u16> = selected.iter().map(|g| g.device).collect();
    let module_type = catalog::module_type(crate::gpu_table::GPUS, &devices, &parsed);
    check_branch(&parsed, kernel, &interfaces, module_type, &loaded)?;
    let versions = match official_versions {
        Ok(versions) => versions,
        Err(item) => {
            return Err(SourceInvalid::VersionUnconfirmed {
                version: version.to_string(),
                unreadable: vec![item.clone()],
            })
        }
    };
    if !versions.iter().any(|candidate| candidate.text() == version) {
        return Err(SourceInvalid::VersionNotOfficial {
            version: version.to_string(),
        });
    }
    if !catalog::drives_all(
        crate::gpu_table::GPUS,
        crate::gpu_table::VERSIONS,
        &devices,
        &parsed,
    ) {
        let conflict = catalog::conflict(crate::gpu_table::GPUS, &devices);
        let mut payload = catalog::gpu_unsupported_json(
            crate::gpu_table::GPUS,
            crate::gpu_table::VERSIONS,
            &selected,
            &parsed,
        );
        let current_kernel = official_kernel
            .clone()
            .or_else(|| facts::kernel(h).value().cloned());
        let candidates = catalog::items(
            &catalog::branch_latest(versions),
            &interfaces,
            &loaded,
            current_kernel.as_deref(),
        )
        .unwrap_or_default();
        let excluded = catalog::open_unbuildable(
            &candidates,
            current_kernel.as_deref().unwrap_or_default(),
            &interfaces,
        );
        let usable = (loaded.is_value() && current_kernel.is_some()).then_some(excluded.as_slice());
        let listed = catalog::gpu_list(
            &candidates,
            &selected,
            crate::gpu_table::GPUS,
            crate::gpu_table::VERSIONS,
            usable,
        );
        for gpu in payload["gpus"].as_array_mut().unwrap() {
            gpu["usable_from_branch"] = json!(listed
                .iter()
                .find(|entry| gpu["address"] == entry.gpu.address)
                .and_then(|entry| entry.usable_from.as_ref().map(facts::Version::major)));
        }
        return Err(SourceInvalid::GpuUnsupported {
            version: Some(version.to_string()),
            gpus: payload["gpus"].clone(),
            conflict,
        });
    }
    snapshot.source = PlanSource::Official;
    snapshot.version = version.to_string();
    snapshot.gpus = plan_gpus(&selected);
    snapshot.kernel_module_type = module_type;
    Ok(None)
}

fn prepare_upload_source(
    ctx: &Ctx,
    group_id: &str,
    version: &Option<String>,
    gpus: &[String],
    snapshot: &mut Snapshot,
) -> Result<(), SourceInvalid> {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    if !gpus_request_ok(gpus) {
        return Err(SourceInvalid::RequestInvalid);
    }
    if !crate::store::group_id_ok(group_id) {
        return Err(SourceInvalid::RequestInvalid);
    }
    if ctx.upload.current().as_deref() != Some(group_id) {
        return Err(SourceInvalid::UploadGone);
    }
    // A body already in flight must finish before its bytes can be confirmed.
    if ctx.upload.receiving() {
        return Err(SourceInvalid::UploadBusy);
    }
    let summary = upload::summary(h, p, group_id);
    let source = match upload::resolve(&summary, version.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            return Err(match e {
                upload::SourceInvalid::VersionRequired => SourceInvalid::UploadVersionRequired,
                upload::SourceInvalid::VersionNotCandidate { version } => {
                    SourceInvalid::UploadVersionNotCandidate { version }
                }
                upload::SourceInvalid::VersionFormat => SourceInvalid::VersionFormat,
                upload::SourceInvalid::Incomplete { missing } => {
                    SourceInvalid::UploadIncomplete { missing }
                }
                upload::SourceInvalid::Problem => SourceInvalid::UploadProblem,
                upload::SourceInvalid::ToolkitTooOld {
                    driver_version,
                    toolkit_version,
                } => SourceInvalid::ToolkitTooOld {
                    driver_version,
                    toolkit_version,
                },
                upload::SourceInvalid::GroupGone => SourceInvalid::UploadGone,
            })
        }
    };
    let chosen = version.clone().unwrap_or_default();
    // Uploads defer driver compatibility to the isolated build; only GPU conflicts block here.
    let host_gpus = read_source_gpus(h)?;
    let selected = check_gpus(gpus, &host_gpus)?;
    let devices: Vec<u16> = selected.iter().map(|g| g.device).collect();
    if catalog::conflict(crate::gpu_table::GPUS, &devices) {
        return Err(SourceInvalid::GpuUnsupported {
            version: None,
            gpus: json!([]),
            conflict: true,
        });
    }
    // Hash staged bytes because group metadata cannot prove that the inputs are unchanged.
    let run_sha256 = match upload::hash_run(h, p, group_id) {
        Ok(sha) => sha,
        Err(_) => return Err(SourceInvalid::UploadProblem),
    };
    let deb_sha256 = match upload::hash_debs(h, p, group_id) {
        Ok(map) => map,
        Err(_) => return Err(SourceInvalid::UploadProblem),
    };
    snapshot.source = PlanSource::Upload {
        fingerprint: run_sha256.chars().take(FINGERPRINT_HEX).collect(),
        run_sha256,
        deb_sha256,
    };
    let _ = source;
    snapshot.version = chosen;
    snapshot.gpus = plan_gpus(&selected);
    if let Some(parsed) = facts::parse_version(&snapshot.version) {
        snapshot.kernel_module_type =
            catalog::module_type(crate::gpu_table::GPUS, &devices, &parsed);
    }
    snapshot.upload_group = Some(group_id.to_string());
    Ok(())
}

fn prepare_local_source(
    h: &dyn Host,
    p: &Paths,
    row_id: &str,
    snapshot: &mut Snapshot,
) -> Result<drivers::Row, SourceInvalid> {
    if !crate::store::row_id_ok(row_id) {
        return Err(SourceInvalid::RequestInvalid);
    }
    let rows = drivers::list(h, p);
    let Some(row) = rows.into_iter().find(|r| r.id == row_id) else {
        return Err(SourceInvalid::LocalRowMissing);
    };
    if let Some(record) = row.record_ok() {
        snapshot.source = PlanSource::Local {
            row_id: row_id.to_string(),
            row_kind: record.kind,
            fingerprint: record.fingerprint.clone(),
            sha256: record.sha256.clone(),
        };
        snapshot.version = record.version.clone();
    }
    Ok(row)
}

fn check_local_source(
    h: &dyn Host,
    p: &Paths,
    local_row: Option<&drivers::Row>,
    local_unreadable: bool,
    failures: &mut Vec<Failure>,
) {
    if local_unreadable {
        failures.push(Failure::unreadable(
            "local_checksum",
            crate::task_state::general_reason_json(
                REASON_RECORD_UNREADABLE,
                READING_TIMEOUT.as_secs(),
            ),
        ));
        failures.push(Failure::unreadable(
            "local_target_kernel",
            crate::task_state::general_reason_json(
                REASON_RECORD_UNREADABLE,
                READING_TIMEOUT.as_secs(),
            ),
        ));
    }

    if let Some(row) = local_row {
        if !local_unreadable {
            let record = row.record_ok().unwrap();
            let path = p.row_dir(&row.id).join("nvidia.raw");
            // Bound the whole-file checksum using the host clock.
            match crate::store::hash_file_within(h, &path, h.now() + READING_TIMEOUT) {
                Ok(copied) => {
                    if copied.sha256 != record.sha256 {
                        failures.push(Failure::new("local_checksum_mismatch"));
                    }
                }
                Err(e) => failures.push(Failure::unreadable(
                    "local_checksum",
                    crate::task_state::general_reason_json(
                        if e.kind() == std::io::ErrorKind::TimedOut {
                            REASON_TIMED_OUT
                        } else {
                            crate::log::io_reason_key(&e)
                        },
                        READING_TIMEOUT.as_secs(),
                    ),
                )),
            }
            let kernel_now = facts::kernel(h);
            match kernel_now {
                Reading::Value(current) => {
                    if record.target_kernel != current {
                        failures.push(Failure {
                            item: "local_kernel_mismatch",
                            object: None,
                            reason: None,
                            target: None,
                            target_kernel: Some(record.target_kernel.clone()),
                            current_kernel: Some(current),
                            row_kind: Some(match record.kind {
                                RowKind::Official => "official".into(),
                                RowKind::Upload => "upload".into(),
                                RowKind::Original => "original".into(),
                            }),
                        });
                    }
                }
                Reading::Unknown(reason) => failures.push(Failure::unreadable(
                    "current_kernel",
                    crate::task_state::general_reason_json(reason, READING_TIMEOUT.as_secs()),
                )),
            }
        }
    }
}

fn check_occupancy(
    h: &dyn Host,
    modules_for_occupancy: Option<Reading<Vec<facts::ModuleLine>>>,
) -> Option<GpuState> {
    // Reuse idle/unreadable module readings; busy or absent readings enter the bounded wait.
    match modules_for_occupancy {
        Some(Reading::Unknown(reason)) => Some(gpu_unreadable(h, reason)),
        Some(Reading::Value(list)) if !facts::occupancy(&list) => None,
        _ => match facts::wait_until_gpu_idle(
            h,
            None,
            |host| match facts::read_modules(host) {
                Reading::Value(list) => Ok(list),
                Reading::Unknown(reason) => Err(reason),
            },
            || true,
        ) {
            facts::OccupancyDecision::Idle(_) | facts::OccupancyDecision::Released { .. } => None,
            facts::OccupancyDecision::StillInUse { .. }
            | facts::OccupancyDecision::Deadline { .. }
            | facts::OccupancyDecision::NotPersisted => Some(gpu_busy(h)),
            facts::OccupancyDecision::Unreadable(reason) => Some(gpu_unreadable(h, reason)),
        },
    }
}

/// Check source and host readiness before assembling a confirmation.
pub fn precheck(
    ctx: &Ctx,
    req: &SourceRequest,
    official_versions: &Result<Vec<facts::Version>, serde_json::Value>,
) -> Result<(Precheck, Snapshot), SourceInvalid> {
    if ctx.setup_problems() {
        return Err(SourceInvalid::SetupBlocked);
    }
    let h = ctx.host.as_ref();
    let p = &ctx.paths;

    let mut snapshot = Snapshot {
        id: String::new(),
        source: PlanSource::Official,
        version: String::new(),
        kernel: String::new(),
        truenas_version: String::new(),
        original: OriginalChoice::Save,
        original_saved: false,
        save_available: true,
        link: facts::LinkReading::Missing,
        gpus: Vec::new(),
        kernel_module_type: None,
        upload_group: None,
        local_row: None,
    };
    let mut modules_for_occupancy: Option<Reading<Vec<facts::ModuleLine>>> = None;
    let mut local_row: Option<drivers::Row> = None;
    let mut local_unreadable = false;
    let mut official_kernel: Option<String> = None;

    match req {
        SourceRequest::Official { version, gpus } => {
            if let Some(result) = prepare_official_source(
                h,
                version,
                gpus,
                &mut snapshot,
                &mut modules_for_occupancy,
                &mut official_kernel,
                official_versions,
            )? {
                return Ok((result, snapshot));
            }
        }
        SourceRequest::Upload {
            group_id,
            version,
            gpus,
        } => {
            prepare_upload_source(ctx, group_id, version, gpus, &mut snapshot)?;
        }
        SourceRequest::Local { row_id } => {
            let row = prepare_local_source(h, p, row_id, &mut snapshot)?;
            local_unreadable = row.record_ok().is_none();
            local_row = Some(row);
        }
    }

    let mut failures: Vec<Failure> = Vec::new();
    let reconciled = crate::tasks::reconcile_managed(h, p);
    match reconciled.active.blocker() {
        None => {}
        Some(crate::tasks::Blocker::Running { .. }) => failures.push(Failure::new("task_running")),
        Some(crate::tasks::Blocker::Unreadable { reason }) => failures.push(Failure::unreadable(
            "running_task",
            json!({"key": reason.key, "params": reason.params}),
        )),
    }
    // Leftover host changes must not become the next task's baseline.
    if let Some(residue) = &reconciled.host_residue {
        failures.push(host_residue_failure(residue));
    }

    let link = facts::link(h);
    match &link {
        facts::LinkReading::Missing => failures.push(Failure::new("link_missing")),
        facts::LinkReading::Elsewhere(target) => failures.push(Failure {
            item: "link_elsewhere",
            object: None,
            reason: None,
            target: Some(target.clone()),
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        }),
        facts::LinkReading::Unknown(reason) => failures.push(Failure::unreadable(
            "extension_link",
            crate::task_state::general_reason_json(reason, READING_TIMEOUT.as_secs()),
        )),
        facts::LinkReading::Ok => {}
    }

    check_local_source(h, p, local_row.as_ref(), local_unreadable, &mut failures);

    let gpu_state = check_occupancy(h, modules_for_occupancy);

    if !failures.is_empty() || gpu_state.is_some() {
        return Ok((
            Precheck::Blocked {
                failures,
                gpu: gpu_state,
            },
            snapshot,
        ));
    }

    let kernel = official_kernel.map_or_else(|| facts::kernel(h), Reading::Value);
    let truenas = facts::truenas_version(h);
    let mut uncomputable = Vec::new();
    match &kernel {
        Reading::Value(_) => {}
        Reading::Unknown(reason) => uncomputable.push(Unreadable {
            object: "current_kernel",
            reason: crate::task_state::general_reason(reason, READING_TIMEOUT.as_secs()),
            raw: None,
        }),
    }
    match &truenas {
        Reading::Value(_) => {}
        Reading::Unknown(reason) => uncomputable.push(Unreadable {
            object: "truenas_version",
            reason: crate::task_state::general_reason(reason, READING_TIMEOUT.as_secs()),
            raw: None,
        }),
    }
    if !uncomputable.is_empty() {
        return Ok((
            Precheck::Uncomputable {
                items: uncomputable,
            },
            snapshot,
        ));
    }
    let kernel = kernel.value().unwrap().clone();
    let truenas = truenas.value().unwrap().clone();
    snapshot.kernel = kernel.clone();
    snapshot.truenas_version = truenas.clone();
    snapshot.link = link;

    let original = drivers::find_original(h, p, &kernel, &truenas);
    let (original_saved, original_json) = match &original {
        drivers::OriginalLookup::Found(row) => {
            let record = row.record_ok().unwrap();
            (
                true,
                json!({
                    "saved": true,
                    "version": record.version,
                    "saved_at": record.created_at,
                }),
            )
        }
        drivers::OriginalLookup::NotFound => (false, json!({"saved": false})),
        drivers::OriginalLookup::Unreadable => (false, json!({"saved": false})),
    };
    let mut save_available = true;
    if !original_saved {
        if let Ok(image_facts) = facts::canonical_facts(h) {
            if image_facts.file_version.unknown_reason().is_some()
                || image_facts.target_kernel.unknown_reason().is_some()
            {
                save_available = false;
            }
        }
    }
    snapshot.original_saved = original_saved;
    snapshot.save_available = save_available;
    snapshot.original = if original_saved {
        OriginalChoice::AlreadySaved
    } else {
        OriginalChoice::Save
    };

    let dialog = confirmation_dialog(&snapshot, kernel, truenas, original_json);
    Ok((Precheck::Ready(dialog), snapshot))
}

fn confirmation_dialog(
    snapshot: &Snapshot,
    kernel: String,
    truenas: String,
    original_json: serde_json::Value,
) -> Dialog {
    let original_saved = snapshot.original_saved;
    let save_available = snapshot.save_available;
    let source_json = match &snapshot.source {
        PlanSource::Official => json!({"kind": "official"}),
        PlanSource::Upload {
            run_sha256,
            fingerprint,
            ..
        } => json!({
            "kind": "upload",
            "fingerprint": fingerprint,
            "sha256": run_sha256,
        }),
        PlanSource::Local {
            row_kind,
            fingerprint,
            sha256,
            ..
        } => {
            let mut obj = serde_json::Map::new();
            obj.insert("kind".into(), json!("local"));
            obj.insert(
                "row_kind".into(),
                json!(match row_kind {
                    RowKind::Official => "official",
                    RowKind::Upload => "upload",
                    RowKind::Original => "original",
                }),
            );
            if let Some(f) = fingerprint {
                obj.insert("fingerprint".into(), json!(f));
            }
            obj.insert("sha256".into(), json!(sha256));
            serde_json::Value::Object(obj)
        }
    };
    let original_state = if original_saved {
        "saved"
    } else if save_available {
        "save_available"
    } else {
        "unavailable"
    };
    let buttons = if original_saved {
        vec![ConfirmationButton {
            button: "start",
            enabled: true,
        }]
    } else {
        vec![
            ConfirmationButton {
                button: "save",
                enabled: save_available,
            },
            ConfirmationButton {
                button: "skip",
                enabled: true,
            },
        ]
    };
    let how_key = match snapshot.source {
        PlanSource::Official => "confirm.how.official",
        PlanSource::Upload { .. } => "confirm.how.upload",
        PlanSource::Local { .. } => "confirm.how.local",
    };
    let mut warnings = Vec::new();
    if matches!(snapshot.source, PlanSource::Upload { .. }) {
        warnings.push(crate::task_state::ReasonValue::new(
            "confirm.upload_trust",
            json!({}),
        ));
    }
    if catalog::community_patch_470(
        matches!(snapshot.source, PlanSource::Official),
        &snapshot.version,
        &kernel,
    ) {
        warnings.push(crate::task_state::ReasonValue::new(
            "confirm.community_patch",
            json!({}),
        ));
    }
    if !original_saved {
        if !save_available {
            warnings.push(crate::task_state::ReasonValue::new(
                "confirm.original.unsavable",
                json!({}),
            ));
        }
        warnings.push(crate::task_state::ReasonValue::new(
            "original.skip_warning",
            json!({}),
        ));
    }
    let confirmation = Confirmation {
        original: original_state,
        buttons,
        how: crate::task_state::ReasonValue::new(how_key, json!({})),
        warnings,
    };
    Dialog {
        source: source_json,
        version: snapshot.version.clone(),
        kernel,
        truenas_version: truenas,
        original: original_json,
        save_available,
        confirmation,
    }
}

/// Replace the one-shot plan and lock its staged upload group.
pub fn remember(ctx: &Ctx, snapshot: Snapshot) -> String {
    let mut slot = ctx.plan.lock().unwrap_or_else(|e| e.into_inner());
    ctx.upload.lock_group(snapshot.upload_group.as_deref());
    slot.remember(snapshot)
}

/// Cancel only the requested snapshot. An older dialog cannot unlock a newer plan.
/// The caller holds the process lock, just as for creation and submission.
pub fn cancel(ctx: &Ctx, plan_id: &str) {
    let mut slot = ctx.plan.lock().unwrap_or_else(|e| e.into_inner());
    if slot.take(plan_id).is_some() {
        ctx.upload.lock_group(None);
    }
}

/// Cancel the current upload's abandoned confirmation while the caller holds process_lock.
/// Plans for other sources and running tasks are unaffected.
pub fn cancel_current_upload(ctx: &Ctx) {
    let mut slot = ctx.plan.lock().unwrap_or_else(|e| e.into_inner());
    let Some(group) = ctx.upload.current() else {
        return;
    };
    if slot
        .0
        .as_ref()
        .and_then(|snapshot| snapshot.upload_group.as_deref())
        == Some(group.as_str())
    {
        slot.0 = None;
        ctx.upload.lock_group(None);
    }
}

/// Confirmation action selected by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Save,
    Skip,
    Start,
}

/// Submission rejections and their response data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitReject {
    Used,
    SetupBlocked,
    ButtonInvalid,
    TaskRunning,
    PrecheckFailed {
        failures: Vec<Failure>,
    },
    Uncomputable {
        items: Vec<Unreadable>,
    },
    UploadBusy,
    Changed {
        changed: Vec<&'static str>,
    },
    Failed {
        reason: crate::task_state::ReasonValue,
        raw: Option<String>,
    },
}

fn check_submit_button(snapshot: &Snapshot, button: Button) -> Result<(), SubmitReject> {
    let button_ok = if snapshot.original_saved {
        button == Button::Start
    } else if snapshot.save_available {
        matches!(button, Button::Save | Button::Skip)
    } else {
        button == Button::Skip
    };
    if !button_ok {
        return Err(SubmitReject::ButtonInvalid);
    }
    Ok(())
}

fn check_source_fingerprint(ctx: &Ctx, snapshot: &Snapshot, changed: &mut Vec<&'static str>) {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    match &snapshot.source {
        PlanSource::Official => {}
        PlanSource::Upload {
            run_sha256,
            deb_sha256,
            ..
        } => {
            let group = snapshot.upload_group.clone().unwrap_or_default();
            if ctx.upload.current().as_deref() != Some(group.as_str()) {
                changed.push("source");
            } else {
                // Rehash the inputs because group metadata cannot prove they are unchanged.
                let run_now = upload::hash_run(h, p, &group).ok();
                let debs_now = upload::hash_debs(h, p, &group).ok();
                let matches = run_now.as_deref() == Some(run_sha256.as_str())
                    && debs_now.as_ref() == Some(deb_sha256);
                if !matches {
                    changed.push("source");
                }
            }
        }
        PlanSource::Local { row_id, sha256, .. } => {
            let rows = drivers::list(h, p);
            match rows.into_iter().find(|r| &r.id == row_id) {
                Some(row) => {
                    match (
                        row.record_ok(),
                        crate::store::hash_file(h, &p.row_dir(row_id).join("nvidia.raw")),
                    ) {
                        (Some(record), Ok(copied)) if copied.sha256 == *sha256 => {
                            if record.version != snapshot.version {
                                changed.push("version");
                            }
                        }
                        _ => changed.push("source"),
                    }
                }
                None => changed.push("source"),
            }
        }
    }
}

fn recheck_submission(ctx: &Ctx, snapshot: &Snapshot) -> Result<(), SubmitReject> {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    let reconciled = crate::tasks::reconcile(h, p);
    match reconciled.active.blocker() {
        None => {}
        Some(crate::tasks::Blocker::Running { .. }) => return Err(SubmitReject::TaskRunning),
        Some(crate::tasks::Blocker::Unreadable { reason }) => {
            return Err(SubmitReject::PrecheckFailed {
                failures: vec![Failure::unreadable(
                    "running_task",
                    json!({"key": reason.key, "params": reason.params}),
                )],
            })
        }
    }
    if let Some(residue) = &reconciled.host_residue {
        return Err(SubmitReject::PrecheckFailed {
            failures: vec![host_residue_failure(residue)],
        });
    }

    let mut items = Vec::new();
    let kernel = facts::kernel(h);
    if let Reading::Unknown(reason) = &kernel {
        items.push(Unreadable {
            object: "current_kernel",
            reason: crate::task_state::general_reason(reason, READING_TIMEOUT.as_secs()),
            raw: None,
        });
    }
    let truenas = facts::truenas_version(h);
    if let Reading::Unknown(reason) = &truenas {
        items.push(Unreadable {
            object: "truenas_version",
            reason: crate::task_state::general_reason(reason, READING_TIMEOUT.as_secs()),
            raw: None,
        });
    }
    let link = facts::link(h);
    if let facts::LinkReading::Unknown(reason) = &link {
        items.push(Unreadable {
            object: "extension_link",
            reason: crate::task_state::general_reason(reason, READING_TIMEOUT.as_secs()),
            raw: None,
        });
    }
    if !items.is_empty() {
        return Err(SubmitReject::Uncomputable { items });
    }
    let mut failures = Vec::new();
    match &link {
        facts::LinkReading::Missing => failures.push(Failure::new("link_missing")),
        facts::LinkReading::Elsewhere(target) => failures.push(Failure {
            item: "link_elsewhere",
            object: None,
            reason: None,
            target: Some(target.clone()),
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        }),
        _ => {}
    }
    if !failures.is_empty() {
        return Err(SubmitReject::PrecheckFailed { failures });
    }

    if snapshot.upload_group.is_some() && ctx.upload.receiving() {
        return Err(SubmitReject::UploadBusy);
    }

    let mut changed: Vec<&'static str> = Vec::new();
    check_source_fingerprint(ctx, snapshot, &mut changed);
    if kernel.value() != Some(&snapshot.kernel) {
        changed.push("kernel");
    }
    if truenas.value() != Some(&snapshot.truenas_version) {
        changed.push("truenas_version");
    }
    let original_now = drivers::find_original(
        h,
        p,
        kernel.value().map(|s| s.as_str()).unwrap_or(""),
        truenas.value().map(|s| s.as_str()).unwrap_or(""),
    );
    let original_saved_now = matches!(original_now, drivers::OriginalLookup::Found(_));
    if original_saved_now != snapshot.original_saved {
        changed.push("original");
    }
    if !changed.is_empty() {
        return Err(SubmitReject::Changed { changed });
    }

    Ok(())
}

fn prepare_task(ctx: &Ctx, snapshot: &Snapshot, button: Button) -> Result<String, SubmitReject> {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    // Persist state before moving inputs so reconciliation can recover an interrupted move.
    let task_id = crate::store::new_task_id(h.wall_ms());
    let original_choice = match button {
        Button::Save => OriginalChoice::Save,
        Button::Skip => OriginalChoice::Skip,
        Button::Start => OriginalChoice::AlreadySaved,
    };
    let plan = Plan {
        source: snapshot.source.clone(),
        version: snapshot.version.clone(),
        kernel: snapshot.kernel.clone(),
        truenas_version: snapshot.truenas_version.clone(),
        original: original_choice,
        // This is the build environment image; the runner replaces it with the verified local ID.
        image_id: crate::build_kit::BUILD_ENV_IMAGE_ID.to_string(),
        build_timeout_minutes: ctx.build_timeout_minutes(),
        gpus: snapshot.gpus.clone(),
        kernel_module_type: snapshot.kernel_module_type,
    };
    let state = crate::task_state::new_state(
        &task_id,
        h.wall_ms(),
        plan.clone(),
        crate::task_state::Phase::ReadLoadedVersion,
    );
    if crate::task_state::write(h, p, &state).is_err() {
        return Err(SubmitReject::Failed {
            reason: crate::task_state::ReasonValue::new(SUBMIT_STATE_UNWRITABLE, json!({})),
            raw: None,
        });
    }
    let dir_result = match &snapshot.source {
        PlanSource::Upload { .. } => {
            let group = snapshot.upload_group.clone().unwrap_or_default();
            let result = upload::move_into_task(h, p, &group, &task_id);
            if result.is_ok() {
                ctx.upload.clear_if(&group);
            }
            result
        }
        _ => {
            let dir = p.task_dir(&task_id);
            h.create_dir(&dir, 0o755)
        }
    };
    if let Err(e) = dir_result {
        let _ = h.remove_file(&p.state(&task_id));
        return Err(SubmitReject::Failed {
            reason: crate::task_state::ReasonValue::new(SUBMIT_TASK_DIR_FAILED, json!({})),
            raw: Some(e.to_string()),
        });
    }

    Ok(task_id)
}

fn rollback_never_ran(
    ctx: &Ctx,
    snapshot: &Snapshot,
    task_id: &str,
    reason: &'static str,
    raw: String,
    code: Option<i32>,
) -> SubmitReject {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    // Only positive NeverRan proof permits rollback; failed move-back must retain task inputs.
    let mut rollback_error: Option<String> = None;
    if let PlanSource::Upload { .. } = &snapshot.source {
        let group = snapshot.upload_group.clone().unwrap_or_default();
        // Persist the destination so reconciliation can retry a failed move-back.
        if let Err(e) = upload::mark_pending_rollback(h, p, &group, task_id) {
            rollback_error = Some(e.to_string());
        }
        match upload::move_back(h, p, &group, task_id) {
            Ok(()) => {
                let _ = h.remove_file(&p.state(task_id));
                ctx.upload.restore(&group);
                let _ = h.remove_dir_all(&p.task_dir(task_id));
            }
            Err(e) => {
                // Keep state, rollback destination and the only input copy for reconciliation.
                rollback_error = Some(match rollback_error {
                    Some(previous) => format!("{previous}\n{e}"),
                    None => e.to_string(),
                });
            }
        }
    } else {
        let _ = h.remove_file(&p.state(task_id));
        let _ = h.remove_dir_all(&p.task_dir(task_id));
    }
    crate::log::error_json(
        Event::TaskSubmitFailed,
        &[
            ("task_id", json!(task_id)),
            (
                "reason",
                crate::log::reason_object(
                    reason,
                    if reason == SUBMIT_UNIT_NOT_CREATED {
                        json!({"code": code})
                    } else {
                        json!({})
                    },
                    Some(&raw),
                ),
            ),
            ("rollback", json!(rollback_error.as_deref().unwrap_or("ok"))),
        ],
    );
    SubmitReject::Failed {
        reason: crate::task_state::ReasonValue::new(
            reason,
            if reason == SUBMIT_UNIT_NOT_CREATED {
                json!({"code": code})
            } else {
                json!({})
            },
        ),
        raw: Some(match rollback_error {
            Some(error) => format!("{raw}\n{error}"),
            None => raw,
        }),
    }
}

/// Consume and submit a plan while the caller holds process_lock.
pub fn submit(ctx: &Ctx, plan_id: &str, button: Button) -> Result<String, SubmitReject> {
    // Keep the host-global lock through rechecks, launch and any rollback.
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    let _control = crate::admission::control_lock(h, crate::admission::LOCK_WAIT).map_err(|b| {
        SubmitReject::Failed {
            reason: crate::task_state::ReasonValue::new(b.key, b.params.clone()),
            raw: b.raw.clone(),
        }
    })?;

    let snapshot = {
        let mut slot = ctx.plan.lock().unwrap_or_else(|e| e.into_inner());
        slot.take(plan_id)
    };
    let Some(snapshot) = snapshot else {
        return Err(SubmitReject::Used);
    };
    // Rejections also consume the plan; process_lock protects uploads during rechecks and moves.
    ctx.upload.lock_group(None);
    if ctx.setup_problems() {
        return Err(SubmitReject::SetupBlocked);
    }
    check_submit_button(&snapshot, button)?;
    recheck_submission(ctx, &snapshot)?;

    if let Err(e) = crate::tasks::stage_runner(h, p) {
        crate::log::error_json(
            Event::RunnerCopyFailed,
            &[(
                "reason",
                crate::log::reason_object(
                    SUBMIT_RUNNER_COPY_FAILED,
                    json!({}),
                    Some(&e.to_string()),
                ),
            )],
        );
        return Err(SubmitReject::Failed {
            reason: crate::task_state::ReasonValue::new(SUBMIT_RUNNER_COPY_FAILED, json!({})),
            raw: Some(e.to_string()),
        });
    }

    let task_id = prepare_task(ctx, &snapshot, button)?;

    let Some(host_data_dir) = ctx.startup.host_data_dir.clone() else {
        return Err(SubmitReject::SetupBlocked);
    };
    crate::log::info(
        Event::TaskSubmitted,
        &[
            ("task_id", task_id.as_str()),
            ("source", source_kind(&snapshot.source)),
            ("version", snapshot.version.as_str()),
        ],
    );
    match crate::tasks::start(h, &host_data_dir, &task_id) {
        crate::tasks::StartResult::Started => Ok(task_id),
        crate::tasks::StartResult::Unknown => Ok(task_id),
        crate::tasks::StartResult::NeverRan { reason, raw, code } => Err(rollback_never_ran(
            ctx, &snapshot, &task_id, reason, raw, code,
        )),
    }
}

pub fn source_kind(source: &PlanSource) -> &'static str {
    match source {
        PlanSource::Official => "official",
        PlanSource::Upload { .. } => "upload",
        PlanSource::Local { .. } => "local",
    }
}

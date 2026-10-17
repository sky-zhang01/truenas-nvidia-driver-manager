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
    pub upload: Arc<UploadState>,
    pub auth: std::sync::Arc<crate::auth::AuthState>,
}

impl Ctx {
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

/// The plan creation `gpus[]` parameter rule of a PCI address, not repeated.
pub fn pci_address_ok(s: &str) -> bool {
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
        && domain
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && bus.len() == 2
        && bus
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && dev.len() == 2
        && dev
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && func.len() == 1
        && func.bytes().all(|b| (b'0'..=b'7').contains(&b))
}

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
        branch: u32,
        reasons: Vec<&'static str>,
    },
    BranchUndecidable {
        unreadable: Vec<Unreadable>,
    },
    GpuUndecidable {
        unreadable: Vec<Unreadable>,
    },
    GpuUnsupported {
        gpus: serde_json::Value,
        conflict: bool,
    },
    GpusChanged,
    UploadGone,
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

/// The pre-check failure item of a leftover an earlier task could not put back: the
/// writable mount is still there, or the read-only property does not read back
/// as the value the kept record names. The reason carries the exact reading.
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

/// Read `/proc/modules`.
pub fn read_modules(h: &dyn Host) -> Reading<Vec<facts::ModuleLine>> {
    let start = h.now();
    let result = match h.read(std::path::Path::new(PROC_MODULES_PATH)) {
        Ok(bytes) => match facts::parse_proc_modules(&String::from_utf8_lossy(&bytes)) {
            Ok(m) => Reading::Value(m),
            Err(_) => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
        },
        Err(e) => Reading::Unknown(match e.kind() {
            std::io::ErrorKind::NotFound => REASON_FILE_MISSING,
            std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
            _ => REASON_IO_ERROR,
        }),
    };
    // A reading that completes only after `READING_TIMEOUT` is unreadable: the
    // runtime-condition table then removes no branch.
    if h.now().saturating_duration_since(start) >= READING_TIMEOUT {
        return Reading::Unknown(REASON_TIMED_OUT);
    }
    result
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
) -> Result<(), SourceInvalid> {
    let major = version.major();
    match catalog::official_verdict(version, kernel, interfaces, module_type) {
        catalog::BranchVerdict::Unbuildable(reasons) => Err(SourceInvalid::BranchUnbuildable {
            branch: major,
            reasons,
        }),
        catalog::BranchVerdict::Undecidable => Err(SourceInvalid::BranchUndecidable {
            unreadable: catalog::HeadersUnreadable::for_branch(major, interfaces, module_type)
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

/// plan creation: run the pre-update checks and produce the dialog.
pub fn precheck(ctx: &Ctx, req: &SourceRequest) -> Result<(Precheck, Snapshot), SourceInvalid> {
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
            if !gpus_request_ok(gpus) {
                return Err(SourceInvalid::RequestInvalid);
            }
            let Some(parsed) = facts::parse_version_exact(version) else {
                return Err(SourceInvalid::VersionFormat);
            };
            if parsed.major() < MIN_MAJOR_BRANCH {
                return Err(SourceInvalid::VersionBelow470);
            }
            if version == COMMUNITY_PATCH_VERSION {
                match facts::kernel(h) {
                    Reading::Value(kernel) => official_kernel = Some(kernel),
                    Reading::Unknown(reason) => {
                        return Ok((
                            Precheck::Uncomputable {
                                items: vec![Unreadable {
                                    object: "current_kernel",
                                    reason: crate::task_state::general_reason(
                                        reason,
                                        READING_TIMEOUT.as_secs(),
                                    ),
                                    raw: None,
                                }],
                            },
                            snapshot,
                        ));
                    }
                }
            }
            let kernel = official_kernel.as_deref().unwrap_or_default();
            let interfaces = catalog::read_interfaces(h);
            check_branch(&parsed, kernel, &interfaces, None)?;
            let loaded = read_modules(h);
            let reasons = catalog::official_module_reasons(&parsed, kernel, &loaded);
            if !reasons.is_empty() {
                return Err(SourceInvalid::BranchUnbuildable {
                    branch: parsed.major(),
                    reasons,
                });
            }
            modules_for_occupancy = Some(loaded);
            // GPU check.
            let host_gpus = match facts::read_gpus(h) {
                Ok(g) => g,
                Err(e) => {
                    // the log's reason is the reading's `{key, params}` object, never the
                    // bare code.
                    crate::log::error_json(
                        Event::GpusUnreadable,
                        &[
                            ("path", json!(e.path.as_str())),
                            (
                                "reason",
                                crate::task_state::general_reason_json(
                                    e.reason,
                                    READING_TIMEOUT.as_secs(),
                                ),
                            ),
                        ],
                    );
                    return Err(SourceInvalid::GpuUndecidable {
                        unreadable: vec![Unreadable {
                            object: "gpus",
                            reason: crate::task_state::general_reason(
                                e.reason,
                                READING_TIMEOUT.as_secs(),
                            ),
                            raw: Some(e.path),
                        }],
                    });
                }
            };
            let selected = check_gpus(gpus, &host_gpus)?;
            let devices: Vec<u16> = selected.iter().map(|g| g.device).collect();
            let module_type = catalog::module_type(crate::gpu_table::GPUS, &devices, &parsed);
            check_branch(&parsed, kernel, &interfaces, module_type)?;
            if !catalog::drives_all(
                crate::gpu_table::GPUS,
                crate::gpu_table::VERSIONS,
                &devices,
                &parsed,
            ) {
                let conflict = catalog::conflict(crate::gpu_table::GPUS, &devices);
                let payload = catalog::gpu_unsupported_json(
                    crate::gpu_table::GPUS,
                    crate::gpu_table::VERSIONS,
                    &selected,
                    &parsed,
                );
                return Err(SourceInvalid::GpuUnsupported {
                    gpus: payload["gpus"].clone(),
                    conflict,
                });
            }
            snapshot.source = PlanSource::Official;
            snapshot.version = version.clone();
            snapshot.gpus = selected
                .iter()
                .map(|g| PlanGpu {
                    address: g.address.clone(),
                    device: format!("{:04x}", g.device),
                })
                .collect();
            snapshot.kernel_module_type = module_type;
        }
        SourceRequest::Upload {
            group_id,
            version,
            gpus,
        } => {
            if !gpus_request_ok(gpus) {
                return Err(SourceInvalid::RequestInvalid);
            }
            if !crate::store::group_id_ok(group_id) {
                // Parameter validation: a malformed id is a format error, checked
                // before the group is looked up.
                return Err(SourceInvalid::RequestInvalid);
            }
            if ctx.upload.current().as_deref() != Some(group_id.as_str()) {
                return Err(SourceInvalid::UploadGone);
            }
            let summary = upload::summary(h, p, group_id);
            let source = match upload::resolve(&summary, version.as_deref()) {
                Ok(s) => s,
                Err(e) => {
                    return Err(match e {
                        upload::SourceInvalid::VersionRequired => {
                            SourceInvalid::UploadVersionRequired
                        }
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
            // GPU check: upload only rejects a conflict.
            let host_gpus = match facts::read_gpus(h) {
                Ok(g) => g,
                Err(e) => {
                    // the log's reason is the reading's `{key, params}` object, never the
                    // bare code.
                    crate::log::error_json(
                        Event::GpusUnreadable,
                        &[
                            ("path", json!(e.path.as_str())),
                            (
                                "reason",
                                crate::task_state::general_reason_json(
                                    e.reason,
                                    READING_TIMEOUT.as_secs(),
                                ),
                            ),
                        ],
                    );
                    return Err(SourceInvalid::GpuUndecidable {
                        unreadable: vec![Unreadable {
                            object: "gpus",
                            reason: crate::task_state::general_reason(
                                e.reason,
                                READING_TIMEOUT.as_secs(),
                            ),
                            raw: Some(e.path),
                        }],
                    });
                }
            };
            let selected = check_gpus(gpus, &host_gpus)?;
            let devices: Vec<u16> = selected.iter().map(|g| g.device).collect();
            if catalog::conflict(crate::gpu_table::GPUS, &devices) {
                return Err(SourceInvalid::GpuUnsupported {
                    gpus: json!([]),
                    conflict: true,
                });
            }
            // The plan records the sha256 of the five actual staged files, not the values
            // stored in `group.json`.
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
            snapshot.gpus = selected
                .iter()
                .map(|g| PlanGpu {
                    address: g.address.clone(),
                    device: format!("{:04x}", g.device),
                })
                .collect();
            if let Some(parsed) = facts::parse_version(&snapshot.version) {
                snapshot.kernel_module_type =
                    catalog::module_type(crate::gpu_table::GPUS, &devices, &parsed);
            }
            snapshot.upload_group = Some(group_id.clone());
        }
        SourceRequest::Local { row_id } => {
            if !crate::store::row_id_ok(row_id) {
                // Same parameter table: a malformed row id never reaches the directory
                // lookup.
                return Err(SourceInvalid::RequestInvalid);
            }
            let rows = drivers::list(h, p);
            let Some(row) = rows.into_iter().find(|r| &r.id == row_id) else {
                return Err(SourceInvalid::LocalRowMissing);
            };
            match row.record_ok() {
                None => {
                    // The row directory exists but its record is unreadable: both local
                    // checks report `record_unreadable` and the request still returns 200
                    // blocked.
                    local_unreadable = true;
                    local_row = Some(row);
                }
                Some(record) => {
                    snapshot.source = PlanSource::Local {
                        row_id: row_id.clone(),
                        row_kind: record.kind,
                        fingerprint: record.fingerprint.clone(),
                        sha256: record.sha256.clone(),
                    };
                    snapshot.version = record.version.clone();
                    local_row = Some(row);
                }
            }
        }
    }

    let mut failures: Vec<Failure> = Vec::new();
    let reconciled = crate::tasks::reconcile_managed(h, p);
    match reconciled.active.blocker() {
        None => {}
        Some(crate::tasks::Blocker::Running { .. }) => failures.push(Failure {
            item: "task_running",
            object: None,
            reason: None,
            target: None,
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        }),
        Some(crate::tasks::Blocker::Unreadable { reason }) => failures.push(Failure {
            item: "unreadable",
            object: Some("running_task"),
            reason: Some(json!({"key": reason.key, "params": reason.params})),
            target: None,
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        }),
    }
    // A host that still shows this tool's own leftovers must not start a task:
    // the new task would read the leftover value as its baseline and make it
    // permanent. The reason names exactly what did not read back.
    if let Some(residue) = &reconciled.host_residue {
        failures.push(host_residue_failure(residue));
    }

    let link = facts::link(h);
    match &link {
        facts::LinkReading::Missing => failures.push(Failure {
            item: "link_missing",
            object: None,
            reason: None,
            target: None,
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        }),
        facts::LinkReading::Elsewhere(target) => failures.push(Failure {
            item: "link_elsewhere",
            object: None,
            reason: None,
            target: Some(target.clone()),
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        }),
        facts::LinkReading::Unknown(reason) => failures.push(Failure {
            item: "unreadable",
            object: Some("extension_link"),
            reason: Some(crate::task_state::general_reason_json(
                reason,
                READING_TIMEOUT.as_secs(),
            )),
            target: None,
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        }),
        facts::LinkReading::Ok => {}
    }

    if local_unreadable {
        failures.push(Failure {
            item: "unreadable",
            object: Some("local_checksum"),
            reason: Some(crate::task_state::general_reason_json(
                REASON_RECORD_UNREADABLE,
                READING_TIMEOUT.as_secs(),
            )),
            target: None,
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        });
        failures.push(Failure {
            item: "unreadable",
            object: Some("local_target_kernel"),
            reason: Some(crate::task_state::general_reason_json(
                REASON_RECORD_UNREADABLE,
                READING_TIMEOUT.as_secs(),
            )),
            target: None,
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        });
    }

    if let Some(row) = &local_row {
        if !local_unreadable {
            let record = row.record_ok().unwrap();
            let path = p.row_dir(&row.id).join("nvidia.raw");
            // The whole-file sha256 has the `READING_TIMEOUT` deadline, judged on the host
            // clock.
            match crate::store::hash_file_within(h, &path, h.now() + READING_TIMEOUT) {
                Ok(copied) => {
                    if copied.sha256 != record.sha256 {
                        failures.push(Failure {
                            item: "local_checksum_mismatch",
                            object: None,
                            reason: None,
                            target: None,
                            target_kernel: None,
                            current_kernel: None,
                            row_kind: None,
                        });
                    }
                }
                Err(e) => failures.push(Failure {
                    item: "unreadable",
                    object: Some("local_checksum"),
                    reason: Some(crate::task_state::general_reason_json(
                        match e.kind() {
                            std::io::ErrorKind::NotFound => REASON_FILE_MISSING,
                            std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
                            std::io::ErrorKind::TimedOut => REASON_TIMED_OUT,
                            _ => REASON_IO_ERROR,
                        },
                        READING_TIMEOUT.as_secs(),
                    )),
                    target: None,
                    target_kernel: None,
                    current_kernel: None,
                    row_kind: None,
                }),
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
                Reading::Unknown(reason) => failures.push(Failure {
                    item: "unreadable",
                    object: Some("current_kernel"),
                    reason: Some(crate::task_state::general_reason_json(
                        reason,
                        READING_TIMEOUT.as_secs(),
                    )),
                    target: None,
                    target_kernel: None,
                    current_kernel: None,
                    row_kind: None,
                }),
            }
        }
    }

    // Occupancy.
    let modules = match modules_for_occupancy {
        Some(m) => m,
        None => read_modules(h),
    };
    let gpu_state = match &modules {
        Reading::Value(list) => {
            if facts::occupancy(list) {
                let users = crate::gpu_users::list(h, HOST_QUERY_TIMEOUT);
                Some(GpuState {
                    state: "busy",
                    users: users.users.iter().map(Into::into).collect(),
                    errors: users.errors.iter().map(Into::into).collect(),
                    read_reason: None,
                })
            } else {
                None
            }
        }
        Reading::Unknown(reason) => {
            let users = crate::gpu_users::list(h, HOST_QUERY_TIMEOUT);
            Some(GpuState {
                state: "unreadable",
                users: users.users.iter().map(Into::into).collect(),
                errors: users.errors.iter().map(Into::into).collect(),
                read_reason: Some(crate::task_state::general_reason_json(
                    reason,
                    READING_TIMEOUT.as_secs(),
                )),
            })
        }
    };

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

    // Whether an original is saved and whether saving is possible.
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
    let dialog = Dialog {
        source: source_json,
        version: snapshot.version.clone(),
        kernel,
        truenas_version: truenas,
        original: original_json,
        save_available,
        confirmation,
    };
    Ok((Precheck::Ready(dialog), snapshot))
}

/// plan creation: remember the snapshot.
pub fn remember(ctx: &Ctx, snapshot: Snapshot) -> String {
    let mut slot = ctx.plan.lock().unwrap_or_else(|e| e.into_inner());
    slot.remember(snapshot)
}

/// task submission button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Save,
    Skip,
    Start,
}

/// task submission rejections.
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

/// task submission: submit a remembered plan. The caller holds the process lock.
pub fn submit(ctx: &Ctx, plan_id: &str, button: Button) -> Result<String, SubmitReject> {
    // the submission is serialized by the host-global control lock: the
    //    disk-changing part of reconciliation and every other launcher take it
    //    too, and a host that cannot hold it must not start a task.
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
    if ctx.setup_problems() {
        return Err(SubmitReject::SetupBlocked);
    }
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
    let reconciled = crate::tasks::reconcile(h, p);
    match reconciled.active.blocker() {
        None => {}
        Some(crate::tasks::Blocker::Running { .. }) => return Err(SubmitReject::TaskRunning),
        Some(crate::tasks::Blocker::Unreadable { reason }) => {
            return Err(SubmitReject::PrecheckFailed {
                failures: vec![Failure {
                    item: "unreadable",
                    object: Some("running_task"),
                    reason: Some(json!({"key": reason.key, "params": reason.params})),
                    target: None,
                    target_kernel: None,
                    current_kernel: None,
                    row_kind: None,
                }],
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
        facts::LinkReading::Missing => failures.push(Failure {
            item: "link_missing",
            object: None,
            reason: None,
            target: None,
            target_kernel: None,
            current_kernel: None,
            row_kind: None,
        }),
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
                // Recompute the sha256 of the five files themselves; the `group.json` values
                // are not the source of truth.
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
                            // The row's recorded version is part of the plan.
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

    if let Err(e) = crate::tasks::stage_runner(h, p) {
        // the submit reason key, with the staging error as the raw of the same reason object.
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
            // The response carries the system error verbatim.
            raw: Some(e.to_string()),
        });
    }

    // first version of the state file, then the task directory.
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
        // The `image_id` of the task record is the build environment image: the build
        // container runs in the pinned official Debian image, not in the program's own image.
        // The runner re-checks the local image ID before the build and replaces this value
        // with the verified one.
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
                // The staged group is the task input now.
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
        crate::tasks::StartResult::NeverRan { reason, raw, code } => {
            // The unit never ran: undo everything, but only what can be undone. If the staged
            // group cannot be moved back, the inputs must survive in the task directory
            // instead of being deleted.
            let mut rollback_error: Option<String> = None;
            if let PlanSource::Upload { .. } = &snapshot.source {
                let group = snapshot.upload_group.clone().unwrap_or_default();
                // Record the destination while it is still known: if the move-back fails,
                // reconcile finds the only copy of the upload in the kept task directory and
                // finishes the rollback.
                if let Err(e) = upload::mark_pending_rollback(h, p, &group, &task_id) {
                    rollback_error = Some(e.to_string());
                }
                match upload::move_back(h, p, &group, &task_id) {
                    Ok(()) => {
                        let _ = h.remove_file(&p.state(&task_id));
                        ctx.upload.restore(&group);
                        let _ = h.remove_dir_all(&p.task_dir(&task_id));
                    }
                    Err(e) => {
                        // Keep the state file and `tasks/<task-id>/in/` (the only copy of the
                        // five files) plus the recorded destination, and report the failure.
                        rollback_error = Some(match rollback_error {
                            Some(previous) => format!("{previous}\n{e}"),
                            None => e.to_string(),
                        });
                    }
                }
            } else {
                let _ = h.remove_file(&p.state(&task_id));
                let _ = h.remove_dir_all(&p.task_dir(&task_id));
            }
            crate::log::error_json(
                Event::TaskSubmitFailed,
                &[
                    ("task_id", json!(task_id.as_str())),
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
            Err(SubmitReject::Failed {
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
            })
        }
    }
}

pub fn source_kind(source: &PlanSource) -> &'static str {
    match source {
        PlanSource::Official => "official",
        PlanSource::Upload { .. } => "upload",
        PlanSource::Local { .. } => "local",
    }
}

//! Upload groups.
//!
//! One group is one `.run` plus the four toolkit `.deb` files staged in
//! `manager/upload/<group_id>/`. Only one group exists at a time and only one file body is
//! received at a time.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes, HttpBody};
use serde::{Deserialize, Serialize};

use crate::codes::*;
use crate::host::{At, Cmd, Create, Host};
use crate::store::{self, JsonRead, Paths, Validate};

#[derive(Debug, Default)]
pub struct UploadState {
    pub inner: Mutex<UploadInner>,
}

#[derive(Debug, Default)]
pub struct UploadInner {
    pub current: Option<String>,
    pub receiving: bool,
}

impl UploadState {
    pub fn new() -> UploadState {
        UploadState::default()
    }

    /// The state after a restart: the group found on disk becomes current again.
    pub fn with_current(group: Option<String>) -> UploadState {
        UploadState {
            inner: Mutex::new(UploadInner {
                current: group,
                receiving: false,
            }),
        }
    }

    pub fn current(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .clone()
    }

    pub fn receiving(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .receiving
    }

    /// Drop the current-group pointer when it still names `group`.
    pub fn clear_if(&self, group: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.current.as_deref() == Some(group) {
            inner.current = None;
        }
    }

    /// Make `group` the current staged group again.
    pub fn restore(&self, group: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.current = Some(group.to_string());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunJson {
    pub sha256: String,
    pub size: u64,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebJson {
    pub sha256: String,
    pub size: u64,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupJson {
    pub run: Option<RunJson>,
    pub debs: BTreeMap<String, DebJson>,
}

impl Validate for GroupJson {
    fn validate(&self) -> Result<(), String> {
        if let Some(run) = &self.run {
            if !is_hex(&run.sha256, 64) {
                return Err("bad run checksum".into());
            }
            for c in &run.candidates {
                if crate::facts::parse_version(c).is_none() {
                    return Err("bad candidate".into());
                }
            }
        }
        for (name, deb) in &self.debs {
            if !TOOLKIT_PACKAGES.contains(&name.as_str()) {
                return Err("unknown toolkit package".into());
            }
            if !is_hex(&deb.sha256, 64) {
                return Err("bad deb checksum".into());
            }
            if deb_version_ok(&deb.version).is_none() {
                return Err("bad deb version".into());
            }
        }
        Ok(())
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Rejection of an upload request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadReject {
    Busy,
    GroupReplaced,
    /// The session was no longer valid at a point that would write.
    Unauthorized,
    Rejected {
        file_name: String,
        reason: &'static str,
        status: u16,
        params: serde_json::Value,
    },
}

impl UploadReject {
    fn file_reject(
        file_name: &str,
        reason: &'static str,
        status: u16,
        params: serde_json::Value,
    ) -> UploadReject {
        UploadReject::Rejected {
            file_name: file_name.to_string(),
            reason,
            status,
            params,
        }
    }
}

/// Result of receiving one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileResult {
    pub kind: String,
    #[serde(rename = "client_name")]
    pub client_name: String,
    pub sha256: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSummary {
    pub group_id: String,
    pub run: Option<GroupSummaryRun>,
    pub debs: BTreeMap<String, GroupSummaryDeb>,
    pub missing: Vec<String>,
    pub toolkit_version: Option<String>,
    pub below_security_floor: bool,
    pub problems: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSummaryRun {
    pub sha256: String,
    pub fingerprint: String,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSummaryDeb {
    pub version: String,
}

/// The upload source fixed by plan creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadSource {
    pub run_sha256: String,
    pub fingerprint: String,
    pub deb_sha256: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceInvalid {
    VersionRequired,
    VersionNotCandidate {
        version: String,
    },
    VersionFormat,
    Incomplete {
        missing: Vec<String>,
    },
    Problem,
    ToolkitTooOld {
        driver_version: String,
        toolkit_version: String,
    },
    GroupGone,
}

impl SourceInvalid {
    pub fn key(&self) -> &'static str {
        match self {
            SourceInvalid::VersionRequired => "plan.source_invalid.upload_version_required",
            SourceInvalid::VersionNotCandidate { .. } => {
                "plan.source_invalid.upload_version_not_candidate"
            }
            SourceInvalid::VersionFormat => "plan.source_invalid.version_format",
            SourceInvalid::Incomplete { .. } => "plan.source_invalid.upload_incomplete",
            SourceInvalid::Problem => "plan.source_invalid.upload_problem",
            SourceInvalid::ToolkitTooOld { .. } => "plan.source_invalid.toolkit_too_old",
            SourceInvalid::GroupGone => "plan.source_invalid.upload_group_gone",
        }
    }

    pub fn params(&self) -> serde_json::Value {
        match self {
            SourceInvalid::VersionNotCandidate { version } => {
                serde_json::json!({"version": version})
            }
            SourceInvalid::Incomplete { missing } => serde_json::json!({"missing": missing}),
            SourceInvalid::ToolkitTooOld {
                driver_version,
                toolkit_version,
            } => serde_json::json!({
                "driver_version": driver_version,
                "toolkit_version": toolkit_version,
            }),
            _ => serde_json::json!({}),
        }
    }
}

// ---- group lifecycle -------------------------------------------------------

fn read_group(h: &dyn Host, p: &Paths, group: &str) -> GroupJson {
    match store::read_json::<GroupJson>(h, &p.upload_group(group).join("group.json")) {
        JsonRead::Ok(g) => g,
        _ => GroupJson {
            run: None,
            debs: BTreeMap::new(),
        },
    }
}

fn write_group(h: &dyn Host, p: &Paths, group: &str, g: &GroupJson) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(g)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    store::write_atomic(h, &p.upload_group(group).join("group.json"), &bytes, 0o644)
}

/// The current sha256 of the staged `.run`.
pub fn hash_run(h: &dyn Host, p: &Paths, group: &str) -> std::io::Result<String> {
    Ok(store::hash_file(h, &p.upload_group(group).join("driver.run"))?.sha256)
}

/// The current sha256 of the four staged `.deb` files.
pub fn hash_debs(
    h: &dyn Host,
    p: &Paths,
    group: &str,
) -> std::io::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for name in TOOLKIT_PACKAGES {
        let path = p
            .upload_group(group)
            .join("toolkit")
            .join(format!("{name}.deb"));
        out.insert(name.to_string(), store::hash_file(h, &path)?.sha256);
    }
    Ok(out)
}

/// The staged group that is still on disk, if any.
pub fn staged_group(h: &dyn Host, p: &Paths) -> Option<String> {
    let mut names: Vec<String> = h
        .read_dir(&p.upload())
        .ok()?
        .iter()
        .filter_map(|n| {
            let text = n.to_string_lossy().into_owned();
            if store::group_id_ok(&text) {
                Some(text)
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names.into_iter().next()
}

/// upload creation: open a new group, replacing an unsubmitted staged group.
pub fn open_group(h: &dyn Host, p: &Paths, st: &UploadState) -> Result<String, UploadReject> {
    let mut inner = st.inner.lock().unwrap_or_else(|e| e.into_inner());
    if inner.receiving {
        return Err(UploadReject::Busy);
    }
    if let Some(old) = inner.current.clone() {
        let from = p.upload_group(&old);
        let to = p.trash().join(&old);
        if h.lstat(&from).is_ok() && h.rename(&from, &to).is_err() {
            // The staged group could not be replaced; keep it as the current
            // group instead of losing it.
            return Err(UploadReject::Rejected {
                file_name: String::new(),
                reason: "write_failed",
                status: 500,
                params: serde_json::json!({}),
            });
        }
        if h.lstat(&to).is_ok() {
            let _ = h.remove_dir_all(&to);
        }
        inner.current = None;
    }
    let group = store::random_hex(16);
    let dir = p.upload_group(&group);
    h.create_dir(&dir, 0o755)
        .map_err(|_| UploadReject::Rejected {
            file_name: String::new(),
            reason: "write_failed",
            status: 500,
            params: serde_json::json!({}),
        })?;
    h.create_dir(&dir.join("toolkit"), 0o755)
        .map_err(|_| UploadReject::Rejected {
            file_name: String::new(),
            reason: "write_failed",
            status: 500,
            params: serde_json::json!({}),
        })?;
    let empty = GroupJson {
        run: None,
        debs: BTreeMap::new(),
    };
    if write_group(h, p, &group, &empty).is_err() {
        let _ = h.remove_dir_all(&dir);
        return Err(UploadReject::Rejected {
            file_name: String::new(),
            reason: "write_failed",
            status: 500,
            params: serde_json::json!({}),
        });
    }
    inner.current = Some(group.clone());
    Ok(group)
}

/// File name rules.
pub fn name_ok(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > CLIENT_NAME_MAX {
        return false;
    }
    if name.contains('/') {
        return false;
    }
    !name.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Run,
    Deb,
}

pub fn file_kind(name: &str) -> Option<FileKind> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".run") {
        Some(FileKind::Run)
    } else if lower.ends_with(".deb") {
        Some(FileKind::Deb)
    } else {
        None
    }
}

/// Extract every dotted version from the plain-text header.
pub fn run_header(prefix: &[u8]) -> Result<Vec<crate::facts::Version>, &'static str> {
    if prefix.len() < 2 {
        return Err("header_unreadable");
    }
    if &prefix[..2] != b"#!" {
        return Err("run_not_shebang");
    }
    let end = prefix.iter().position(|b| *b == 0).unwrap_or(prefix.len());
    let header = String::from_utf8_lossy(&prefix[..end]);
    if !header.contains("Linux-x86_64") {
        return Err("run_not_linux_x86_64");
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for line in header.lines() {
        let Some(rest) = line.strip_prefix("label=") else {
            continue;
        };
        let rest = rest.trim().trim_matches(|c| c == '"' || c == '\'');
        let mut token = String::new();
        for ch in rest.chars().chain(std::iter::once(' ')) {
            if ch.is_ascii_digit() || ch == '.' {
                token.push(ch);
            } else {
                if !token.is_empty() {
                    let trimmed = token.trim_matches('.');
                    if let Some(v) = crate::facts::parse_version(trimmed) {
                        if seen.insert(v.text().to_string()) {
                            out.push(v);
                        }
                    }
                    token.clear();
                }
            }
        }
    }
    Ok(out)
}

/// `[0-9]+(\.[0-9]+){1,3}(-[0-9]+)?`
pub fn deb_version_ok(version: &str) -> Option<(Vec<u32>, Option<u32>)> {
    let (upstream, revision) = match version.split_once('-') {
        Some((u, r)) => {
            if r.is_empty() || !r.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (u, Some(r.parse::<u32>().ok()?))
        }
        None => (version, None),
    };
    let parts: Vec<&str> = upstream.split('.').collect();
    if !(2..=4).contains(&parts.len()) {
        return None;
    }
    let mut nums = Vec::new();
    for p in parts {
        if p.is_empty()
            || p.len() > VERSION_SEGMENT_DIGITS
            || !p.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        nums.push(p.parse::<u32>().ok()?);
    }
    if revision.is_none() && version == upstream {
        // A version without a revision is still acceptable.
    }
    Some((nums, revision))
}

/// Compare toolkit versions numerically.
pub fn toolkit_cmp(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (ap, _) = deb_version_ok(a)?;
    let (bp, _) = deb_version_ok(b)?;
    Some(ap.cmp(&bp))
}

fn toolkit_below_floor(version: &str, floor: [u32; 3]) -> bool {
    deb_version_ok(version)
        .map(|(parts, _)| parts.as_slice() < floor.as_slice())
        .unwrap_or(false)
}

pub(crate) fn toolkit_too_old(driver_major: u32, toolkit_version: &str) -> bool {
    driver_major >= TOOLKIT_DRIVER_FLOOR_BRANCH
        && toolkit_below_floor(toolkit_version, TOOLKIT_DRIVER_615_FLOOR)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebFields {
    pub package: String,
    pub version: String,
    pub architecture: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebReject {
    FieldsUnreadable,
    PackageUnknown,
    Architecture,
    VersionFormat,
}

/// package inspection: read the three fields of a `.deb`.
///
/// `file` is the container path of the received file; the program runs on the host with the
/// host path of that same file through the data-directory bind mount. A file whose host path
/// cannot be derived (the start-up check did not confirm the mapping, or the path is outside
/// the data directory) is `FieldsUnreadable`, the reason the caller already answers.
pub fn deb_fields(
    h: &dyn Host,
    file: &std::path::Path,
    host_data_dir: Option<&std::path::Path>,
) -> Result<DebFields, DebReject> {
    let Some(host_file) = crate::host::host_path_of_data_dir(host_data_dir, file) else {
        return Err(DebReject::FieldsUnreadable);
    };
    let out = h.run(
        &Cmd::new(At::Host, "dpkg-deb", HOST_QUERY_TIMEOUT)
            .arg("--field")
            .arg(host_file.as_os_str())
            .arg("Package")
            .arg("Version")
            .arg("Architecture"),
    );
    if !out.exit.ok() {
        return Err(DebReject::FieldsUnreadable);
    }
    let text = out.stdout_text();
    let mut package = None;
    let mut version = None;
    let mut arch = None;
    let mut counts = (0, 0, 0);
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().to_string();
        match k.trim() {
            "Package" => {
                counts.0 += 1;
                package = Some(v);
            }
            "Version" => {
                counts.1 += 1;
                version = Some(v);
            }
            "Architecture" => {
                counts.2 += 1;
                arch = Some(v);
            }
            _ => {}
        }
    }
    if counts != (1, 1, 1) {
        return Err(DebReject::FieldsUnreadable);
    }
    let (package, version, architecture) = (package.unwrap(), version.unwrap(), arch.unwrap());
    if !TOOLKIT_PACKAGES.contains(&package.as_str()) {
        return Err(DebReject::PackageUnknown);
    }
    if architecture != "amd64" {
        return Err(DebReject::Architecture);
    }
    if deb_version_ok(&version).is_none() {
        return Err(DebReject::VersionFormat);
    }
    Ok(DebFields {
        package,
        version,
        architecture,
    })
}

/// current upload/file upload: summarize the current group.
pub fn summary(h: &dyn Host, p: &Paths, group: &str) -> GroupSummary {
    let g = read_group(h, p, group);
    let mut missing = Vec::new();
    for name in TOOLKIT_PACKAGES {
        if !g.debs.contains_key(name) {
            missing.push(name.to_string());
        }
    }
    let versions: Vec<String> = TOOLKIT_PACKAGES
        .iter()
        .filter_map(|n| g.debs.get(*n).map(|d| d.version.clone()))
        .collect();
    let all_same =
        versions.len() == TOOLKIT_PACKAGES.len() && versions.iter().all(|v| v == &versions[0]);
    let toolkit_version = if all_same {
        Some(
            versions[0]
                .split('-')
                .next()
                .unwrap_or(&versions[0])
                .to_string(),
        )
    } else {
        None
    };
    let below_security_floor = toolkit_version
        .as_deref()
        .map(|v| toolkit_below_floor(v, TOOLKIT_SECURITY_FLOOR))
        .unwrap_or(false);
    let mut problems = Vec::new();
    if g.run.is_none() {
        problems.push(serde_json::json!({"key": "upload.group.run_missing", "params": {}}));
    }
    if !missing.is_empty() {
        problems.push(serde_json::json!({
            "key": "upload.group.missing",
            "params": {"packages": missing},
        }));
    }
    if !all_same && versions.len() == TOOLKIT_PACKAGES.len() {
        problems.push(serde_json::json!({
            "key": "upload.group.versions_differ",
            "params": {"versions": versions},
        }));
    }
    if let (Some(run), Some(toolkit)) = (&g.run, &toolkit_version) {
        if run.candidates.len() == 1 {
            let driver = crate::facts::parse_version(&run.candidates[0]);
            if let Some(driver) = driver {
                if toolkit_too_old(driver.major(), toolkit) {
                    problems.push(serde_json::json!({
                        "key": "upload.group.toolkit_too_old",
                        "params": {
                            "driver_version": run.candidates[0],
                            "toolkit_version": toolkit,
                        },
                    }));
                }
            }
        }
    }
    GroupSummary {
        group_id: group.to_string(),
        run: g.run.map(|r| GroupSummaryRun {
            fingerprint: r.sha256.chars().take(FINGERPRINT_HEX).collect(),
            sha256: r.sha256,
            candidates: r.candidates,
        }),
        debs: g
            .debs
            .into_iter()
            .map(|(k, v)| (k, GroupSummaryDeb { version: v.version }))
            .collect(),
        missing,
        toolkit_version,
        below_security_floor,
        problems,
    }
}

/// plan creation: validate the upload source.
pub fn resolve(s: &GroupSummary, version: Option<&str>) -> Result<UploadSource, SourceInvalid> {
    if s.run.is_none() || !s.missing.is_empty() {
        return Err(SourceInvalid::Incomplete {
            missing: s.missing.clone(),
        });
    }
    for p in &s.problems {
        if p.get("key").and_then(|k| k.as_str()) == Some("upload.group.versions_differ") {
            return Err(SourceInvalid::Problem);
        }
    }
    let run = s.run.as_ref().unwrap();
    let chosen = match version {
        None => return Err(SourceInvalid::VersionRequired),
        Some(v) => {
            if run.candidates.is_empty() {
                if crate::facts::parse_version(v).is_none() {
                    return Err(SourceInvalid::VersionFormat);
                }
            } else if !run.candidates.iter().any(|c| c == v) {
                return Err(SourceInvalid::VersionNotCandidate {
                    version: v.to_string(),
                });
            }
            v.to_string()
        }
    };
    if let Some(toolkit) = &s.toolkit_version {
        if let Some(driver) = crate::facts::parse_version(&chosen) {
            if toolkit_too_old(driver.major(), toolkit) {
                return Err(SourceInvalid::ToolkitTooOld {
                    driver_version: chosen,
                    toolkit_version: toolkit.clone(),
                });
            }
        }
    }
    // Re-read the group JSON for the toolkit checksums (the summary only has
    // versions); callers that need the checksums read them through
    // `group_checksums`.
    Ok(UploadSource {
        run_sha256: run.sha256.clone(),
        fingerprint: run.fingerprint.clone(),
        deb_sha256: BTreeMap::new(),
    })
}

/// The four `.deb` checksums of a staged group.
pub fn group_checksums(h: &dyn Host, p: &Paths, group: &str) -> BTreeMap<String, String> {
    read_group(h, p, group)
        .debs
        .into_iter()
        .map(|(k, v)| (k, v.sha256))
        .collect()
}

/// task submission: move the whole group into the task directory. The directory created for
/// the move is removed again when the group cannot move in, so a failed submission leaves no
/// `tasks/<task-id>/` behind.
pub fn move_into_task(h: &dyn Host, p: &Paths, group: &str, task_id: &str) -> std::io::Result<()> {
    let from = p.upload_group(group);
    let to = p.task_dir(task_id).join("in");
    let task_dir = p.task_dir(task_id);
    h.create_dir(&task_dir, 0o755)?;
    match h.rename(&from, &to) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = h.remove_dir_all(&task_dir);
            Err(e)
        }
    }
}

/// task submission: move the group back after a failed submission. The rename is the normal
/// path; if it fails, the files are copied into the staging group so the only copy of the
/// user's upload is never deleted.
pub fn move_back(h: &dyn Host, p: &Paths, group: &str, task_id: &str) -> std::io::Result<()> {
    let from = p.task_dir(task_id).join("in");
    let to = p.upload_group(group);
    if h.rename(&from, &to).is_ok() {
        return Ok(());
    }
    h.create_dir(&to, 0o755)?;
    h.create_dir(&to.join("toolkit"), 0o755)?;
    store::copy_file_verified(h, &from.join("group.json"), &to.join("group.json"), 0o644)?;
    store::copy_file_verified(h, &from.join("driver.run"), &to.join("driver.run"), 0o644)?;
    for name in TOOLKIT_PACKAGES {
        let file = format!("{name}.deb");
        store::copy_file_verified(
            h,
            &from.join("toolkit").join(&file),
            &to.join("toolkit").join(&file),
            0o644,
        )?;
    }
    Ok(())
}

/// Hidden marker inside a kept task directory that records the upload group the staged files
/// must be moved back to. It is written before the move-back is attempted, so a failed
/// move-back leaves the only copy of the upload in `tasks/<task id>/in/` next to a durable
/// destination.
pub const PENDING_ROLLBACK_FILE: &str = ".rollback-upload";

/// Record the group the staged files of `task_id` must be moved back to.
pub fn mark_pending_rollback(
    h: &dyn Host,
    p: &Paths,
    group: &str,
    task_id: &str,
) -> std::io::Result<()> {
    if !store::group_id_ok(group) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid upload group",
        ));
    }
    let dir = p.task_dir(task_id);
    let path = dir.join(PENDING_ROLLBACK_FILE);
    let mut f = h.create(&path, Create::New, 0o600)?;
    f.write_all(group.as_bytes())?;
    f.sync_all()?;
    drop(f);
    h.fsync_dir(&dir)
}

/// The group recorded by [`mark_pending_rollback`], if the marker is there.
pub fn pending_rollback(h: &dyn Host, p: &Paths, task_id: &str) -> std::io::Result<Option<String>> {
    match h.read(&p.task_dir(task_id).join(PENDING_ROLLBACK_FILE)) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "pending rollback marker is not text",
                )
            })?;
            let group = text.trim().to_string();
            if !store::group_id_ok(&group) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "pending rollback marker does not name a group",
                ));
            }
            Ok(Some(group))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Finish the rollback of a submission that never ran: put the staged files back into the
/// upload group recorded by the marker, each verified against the checksums the plan was
/// confirmed with. On any failure the staged copies stay where they are, so the caller keeps
/// the task directory.
pub fn finish_pending_rollback(
    h: &dyn Host,
    p: &Paths,
    task_id: &str,
    group: &str,
) -> std::io::Result<()> {
    if !store::group_id_ok(group) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid upload group",
        ));
    }
    let (run_sha256, deb_sha256) = match crate::task_state::read(h, p, task_id) {
        store::JsonRead::Ok(state) => match state.plan.map(|plan| plan.source) {
            Some(crate::task_state::PlanSource::Upload {
                run_sha256,
                deb_sha256,
                ..
            }) => (run_sha256, deb_sha256),
            _ => {
                return Err(std::io::Error::other(
                    "the task's recorded source is not an upload group",
                ))
            }
        },
        _ => return Err(std::io::Error::other("the task state file is not readable")),
    };
    let from = p.task_dir(task_id).join("in");
    let to = p.upload_group(group);
    h.create_dir(&to, 0o755)?;
    h.create_dir(&to.join("toolkit"), 0o755)?;
    restore_staged(h, &from.join("group.json"), &to.join("group.json"), None)?;
    restore_staged(
        h,
        &from.join("driver.run"),
        &to.join("driver.run"),
        Some(&run_sha256),
    )?;
    for name in TOOLKIT_PACKAGES {
        let file = format!("{name}.deb");
        restore_staged(
            h,
            &from.join("toolkit").join(&file),
            &to.join("toolkit").join(&file),
            deb_sha256.get(name),
        )?;
    }
    Ok(())
}

/// One file of [`finish_pending_rollback`]. A target that already holds the
/// expected bytes is left alone; otherwise the bytes are copied from the staged
/// group through the shared verified copy and checked against the planned
/// checksum.
fn restore_staged(
    h: &dyn Host,
    from: &std::path::Path,
    to: &std::path::Path,
    want: Option<&String>,
) -> std::io::Result<()> {
    match want {
        Some(want) => {
            if store::hash_file(h, to)
                .map(|have| &have.sha256 == want)
                .unwrap_or(false)
            {
                return Ok(());
            }
            let copied = store::copy_file_verified(h, from, to, 0o644)?;
            if &copied.sha256 != want {
                return Err(std::io::Error::other(
                    "the staged file does not match the planned checksum",
                ));
            }
        }
        None => {
            // `group.json` has no planned checksum; the staged copy is the
            // accepted record, so keep it when it is there.
            if h.lstat(from).is_ok() {
                store::copy_file_verified(h, from, to, 0o644)?;
            } else if h.lstat(to).is_err() {
                return Err(std::io::Error::other("the staged record is missing"));
            }
        }
    }
    Ok(())
}

/// Put the accepted slot file back after a rejected replacement. The direct rename is the
/// normal path; a refused rename falls back to a verified copy, and a refused copy keeps the
/// backup under the protected recovery name whose target `store::flush_recovery` restores.
fn restore_slot(h: &dyn Host, backup: &std::path::Path, final_path: &std::path::Path) {
    let Ok(want) = store::hash_file(h, backup) else {
        return;
    };
    let restored = |h: &dyn Host| {
        store::hash_file(h, final_path)
            .map(|have| have.sha256 == want.sha256 && have.size == want.size)
            .unwrap_or(false)
    };
    if h.rename(backup, final_path).is_ok() && restored(h) {
        return;
    }
    if store::copy_file_verified(h, backup, final_path, 0o600).is_ok() && restored(h) {
        let _ = h.remove_file(backup);
        return;
    }
    // A refused rename does not imply a refused write: put the kept bytes back in place as
    // the last restore route before keeping the protected copy.
    if store::restore_in_place(h, backup, final_path, 0o600).is_ok() && restored(h) {
        let _ = h.remove_file(backup);
        return;
    }
    let dir = final_path.parent().unwrap_or(std::path::Path::new("."));
    let file_name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let _ = store::protect_recovery(h, dir, &file_name, backup, 0o600, None);
}

struct WriteOutcome {
    sha256: String,
    size: u64,
    prefix: Vec<u8>,
}

/// The `upload_rejected` line of one interrupted receive: `idle` for the 60-second timeout,
/// `disconnected` for a request-body read error or a handler future that is dropped while the
/// body is still being read. A deliberate outcome (complete body, size refusal, write failure
/// or a later validation) disarms it before it returns, so a normal upload never writes the
/// line.
struct InterruptionLog {
    reason: Option<&'static str>,
}

impl InterruptionLog {
    fn new() -> InterruptionLog {
        InterruptionLog {
            reason: Some("disconnected"),
        }
    }

    /// Report one interruption; the guard does not log a second time.
    fn report(&mut self, reason: &'static str) {
        self.reason = None;
        crate::log::info(Event::UploadRejected, &[("reason", reason)]);
    }

    /// The body read ended deliberately; no interruption line.
    fn finished(&mut self) {
        self.reason = None;
    }
}

impl Drop for InterruptionLog {
    fn drop(&mut self) {
        if let Some(reason) = self.reason {
            crate::log::info(Event::UploadRejected, &[("reason", reason)]);
        }
    }
}

struct PartialGuard {
    host: Arc<dyn Host>,
    path: PathBuf,
    keep: bool,
}

impl PartialGuard {
    fn new(host: Arc<dyn Host>, path: PathBuf) -> PartialGuard {
        PartialGuard {
            host,
            path,
            keep: false,
        }
    }
    fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for PartialGuard {
    fn drop(&mut self) {
        if !self.keep {
            let _ = self.host.remove_file(&self.path);
        }
    }
}

/// Frames the body read may hand to the writer beyond what the writer has
/// accepted. With the frames passed through uncopied this is the fixed bound
/// of not-yet-written upload data held in memory: a slow disk stalls the body
/// read instead of letting it buffer the whole file.
const WRITE_QUEUE_FRAMES: usize = 64;

/// file upload: receive one file body.
#[allow(clippy::too_many_arguments)]
pub async fn receive(
    h: Arc<dyn Host>,
    p: Paths,
    // The host path of the data directory the start-up check confirmed; the
    // `.deb` fields are read on the host through it.
    host_data_dir: Option<PathBuf>,
    group: String,
    client_name: String,
    content_length: Option<u64>,
    body: Body,
    st: Arc<UploadState>,
    process_lock: &tokio::sync::Mutex<()>,
    session_ok: impl Fn() -> bool,
) -> Result<(FileResult, GroupSummary), UploadReject> {
    if !store::group_id_ok(&group) {
        return Err(UploadReject::GroupReplaced);
    }
    {
        let _lock = process_lock.lock().await;
        // Re-check before this receive marks the group busy or creates a file.
        if !session_ok() {
            return Err(UploadReject::Unauthorized);
        }
        let mut inner = st.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.current.as_deref() != Some(group.as_str()) {
            return Err(UploadReject::GroupReplaced);
        }
        if inner.receiving {
            return Err(UploadReject::Busy);
        }
        inner.receiving = true;
    }
    let guard = ReceivingGuard { st: st.clone() };
    if !name_ok(&client_name) {
        return Err(UploadReject::Rejected {
            file_name: client_name,
            reason: "not_run_or_deb",
            status: 422,
            params: serde_json::json!({}),
        });
    }
    let Some(kind) = file_kind(&client_name) else {
        return Err(UploadReject::file_reject(
            &client_name,
            "not_run_or_deb",
            422,
            serde_json::json!({}),
        ));
    };
    if content_length
        .map(|n| n > UPLOAD_FILE_LIMIT)
        .unwrap_or(false)
    {
        return Err(UploadReject::file_reject(
            &client_name,
            "too_large",
            413,
            serde_json::json!({}),
        ));
    }

    // stream into `.incoming-<hex>` in a background thread.
    let incoming_name = format!(".incoming-{}", store::random_hex(8));
    let incoming = p.upload_group(&group).join(&incoming_name);
    let partial = PartialGuard::new(h.clone(), incoming.clone());
    let mode = 0o600;
    let file = h.create(&incoming, Create::New, mode).map_err(|_| {
        UploadReject::file_reject(&client_name, "write_failed", 500, serde_json::json!({}))
    })?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(WRITE_QUEUE_FRAMES);
    // The writer runs on the blocking pool and owns the receiving flag: the
    // flag is released only when the thread that holds the file has really
    // ended, so a cancelled future cannot let a second receive start while
    // this one is still writing.
    let writer = tokio::task::spawn_blocking(move || -> std::io::Result<WriteOutcome> {
        use sha2::{Digest, Sha256};
        let _guard = guard;
        let mut file = file;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut prefix = Vec::new();
        while let Some(chunk) = rx.blocking_recv() {
            if chunk.is_empty() {
                continue;
            }
            file.write_all(&chunk)?;
            hasher.update(&chunk);
            size += chunk.len() as u64;
            if prefix.len() < RUN_HEADER_WINDOW {
                let take = (RUN_HEADER_WINDOW - prefix.len()).min(chunk.len());
                prefix.extend_from_slice(&chunk[..take]);
            }
        }
        file.sync_all()?;
        Ok(WriteOutcome {
            sha256: store::hex(&hasher.finalize()),
            size,
            prefix,
        })
    });

    let mut body = std::pin::pin!(body);
    let mut read_error = false;
    let mut too_large = false;
    let mut written: u64 = 0;
    // Cancelling the handler future drops this guard: the receive was interrupted, so the
    // `disconnected` line is written even though the loop never reported an error.
    let mut interruption = InterruptionLog::new();
    loop {
        let next = tokio::time::timeout(
            UPLOAD_IDLE_TIMEOUT,
            std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)),
        )
        .await;
        let frame = match next {
            Err(_) => {
                interruption.report("idle");
                read_error = true;
                break;
            }
            Ok(None) => break,
            Ok(Some(Err(_))) => {
                interruption.report("disconnected");
                read_error = true;
                break;
            }
            Ok(Some(Ok(frame))) => frame,
        };
        let Some(data) = frame.data_ref() else {
            continue;
        };
        written += data.len() as u64;
        if written > UPLOAD_FILE_LIMIT {
            too_large = true;
            break;
        }
        if tx.send(data.clone()).await.is_err() {
            interruption.finished();
            read_error = true;
            break;
        }
    }
    // The body read is over; only a drop before this point is a receive
    // interruption. A later rejection or the success path was not interrupted.
    interruption.finished();
    drop(tx);
    let outcome = match writer.await {
        Ok(Ok(o)) => o,
        Ok(Err(_)) | Err(_) => {
            return Err(UploadReject::file_reject(
                &client_name,
                "write_failed",
                500,
                serde_json::json!({}),
            ))
        }
    };
    if too_large {
        return Err(UploadReject::file_reject(
            &client_name,
            "too_large",
            413,
            serde_json::json!({}),
        ));
    }
    if read_error {
        return Err(UploadReject::file_reject(
            &client_name,
            "interrupted",
            400,
            serde_json::json!({}),
        ));
    }

    let mut result = FileResult {
        kind: "run".to_string(),
        client_name: client_name.clone(),
        sha256: outcome.sha256.clone(),
        size: outcome.size,
        package: None,
        version: None,
        candidates: None,
    };
    let mut deb: Option<DebJson> = None;
    match kind {
        FileKind::Run => match run_header(&outcome.prefix) {
            Ok(candidates) => {
                let texts: Vec<String> = candidates.iter().map(|v| v.text().to_string()).collect();
                result.candidates = Some(texts);
            }
            Err(reason) => {
                return Err(UploadReject::file_reject(
                    &client_name,
                    reason,
                    422,
                    serde_json::json!({}),
                ))
            }
        },
        FileKind::Deb => {
            result.kind = "deb".to_string();
            match deb_fields(h.as_ref(), &incoming, host_data_dir.as_deref()) {
                Ok(fields) => {
                    result.package = Some(fields.package.clone());
                    result.version = Some(fields.version.clone());
                    deb = Some(DebJson {
                        sha256: outcome.sha256.clone(),
                        size: outcome.size,
                        version: fields.version.clone(),
                    });
                }
                Err(reason) => {
                    let (code, params) = match reason {
                        DebReject::FieldsUnreadable => {
                            ("deb_fields_unreadable", serde_json::json!({}))
                        }
                        DebReject::PackageUnknown => {
                            ("deb_package_unknown", serde_json::json!({"package": ""}))
                        }
                        DebReject::Architecture => {
                            ("deb_arch", serde_json::json!({"architecture": ""}))
                        }
                        DebReject::VersionFormat => {
                            ("deb_version_format", serde_json::json!({"version": ""}))
                        }
                    };
                    return Err(UploadReject::file_reject(&client_name, code, 422, params));
                }
            }
        }
    }

    // finalize under the process lock. The group check, the file rename and the
    // `group.json` rewrite are one critical section, so a submission can never move a
    // half-finalized group. The old file and the old record survive a failed rewrite.
    let final_path = match (&kind, result.package.as_deref()) {
        (FileKind::Run, _) => p.upload_group(&group).join("driver.run"),
        (FileKind::Deb, Some(package)) => {
            let dir = p.upload_group(&group).join("toolkit");
            if h.create_dir(&dir, 0o755).is_err() {
                return Err(UploadReject::file_reject(
                    &client_name,
                    "write_failed",
                    500,
                    serde_json::json!({}),
                ));
            }
            dir.join(format!("{package}.deb"))
        }
        (FileKind::Deb, None) => {
            return Err(UploadReject::file_reject(
                &client_name,
                "deb_fields_unreadable",
                422,
                serde_json::json!({}),
            ))
        }
    };
    {
        let _lock = process_lock.lock().await;
        // Re-check again before the temporary file is renamed into the slot.
        if !session_ok() {
            return Err(UploadReject::Unauthorized);
        }
        {
            let inner = st.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.current.as_deref() != Some(group.as_str()) {
                return Err(UploadReject::GroupReplaced);
            }
        }
        let file_name = final_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // A slot that still has protected recovery material must get those bytes back before
        // a new replacement snapshots the slot.
        let final_dir = final_path.parent().unwrap_or(std::path::Path::new("."));
        if store::flush_recovery(h.as_ref(), final_dir, &file_name).is_err() {
            return Err(UploadReject::file_reject(
                &client_name,
                "write_failed",
                500,
                serde_json::json!({}),
            ));
        }
        // The accepted slot is moved under the protected recovery name, not a plain `.tmp-*`
        // name: whatever happens next, the only copy of the old bytes can never be deleted by
        // the ordinary leftover cleanup. `flush_recovery` above put back any earlier material
        // first.
        let backup = store::recovery_path(final_dir, &file_name);
        let had_old = h.lstat(&final_path).is_ok();
        if had_old && h.rename(&final_path, &backup).is_err() {
            return Err(UploadReject::file_reject(
                &client_name,
                "write_failed",
                500,
                serde_json::json!({}),
            ));
        }
        if h.rename(&incoming, &final_path).is_err() {
            if had_old {
                // The accepted slot must never be lost, including when the rollback rename is
                // refused: the shared restore falls back to a verified copy and finally keeps
                // the bytes under the protected name.
                restore_slot(h.as_ref(), &backup, &final_path);
            }
            return Err(UploadReject::file_reject(
                &client_name,
                "write_failed",
                500,
                serde_json::json!({}),
            ));
        }
        partial.keep();
        let group_path = p.upload_group(&group).join("group.json");
        let group_before = h.read(&group_path).ok();
        let mut g = read_group(h.as_ref(), &p, &group);
        match kind {
            FileKind::Run => {
                g.run = Some(RunJson {
                    sha256: outcome.sha256.clone(),
                    size: outcome.size,
                    candidates: result.candidates.clone().unwrap_or_default(),
                });
            }
            FileKind::Deb => {
                if let (Some(package), Some(deb)) = (result.package.clone(), deb) {
                    g.debs.insert(package, deb);
                }
            }
        }
        if write_group(h.as_ref(), &p, &group, &g).is_err() {
            // Rejecting the new file must not destroy the accepted slot: drop the new file
            // and put the old one back. A refused rename falls back to a verified copy, and a
            // refused copy keeps the backup under the protected recovery name.
            let _ = h.remove_file(&final_path);
            if had_old {
                restore_slot(h.as_ref(), &backup, &final_path);
            }
            // The accepted `group.json` must not describe the rejected file.
            let _ = store::flush_recovery(h.as_ref(), final_dir, "group.json");
            return Err(UploadReject::file_reject(
                &client_name,
                "write_failed",
                500,
                serde_json::json!({}),
            ));
        }
        if had_old {
            // A committed replacement must not keep protected recovery material: a later
            // flush would put the old bytes back over the accepted file. Every retirement
            // route is tried first; only when all of them are refused is the replacement
            // undone.
            if store::retire_recovery(h.as_ref(), final_dir, &file_name, &backup, &final_path)
                .is_err()
            {
                let _ = h.remove_file(&final_path);
                restore_slot(h.as_ref(), &backup, &final_path);
                if let Some(before) = &group_before {
                    let _ = store::write_in_place(h.as_ref(), &group_path, before, 0o644);
                }
                return Err(UploadReject::file_reject(
                    &client_name,
                    "write_failed",
                    500,
                    serde_json::json!({}),
                ));
            }
        }
    }
    Ok((result, summary(h.as_ref(), &p, &group)))
}

struct ReceivingGuard {
    st: Arc<UploadState>,
}

impl Drop for ReceivingGuard {
    fn drop(&mut self) {
        let mut inner = self.st.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.receiving = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RealHost;
    use crate::test_support::TestDir;

    #[test]
    fn name_and_kind_rules() {
        assert!(name_ok("NVIDIA-Linux-x86_64-580.178.04.run"));
        assert!(!name_ok(""));
        assert!(!name_ok("a/b.run"));
        assert!(!name_ok("bad\nname.run"));
        assert_eq!(file_kind("x.RUN"), Some(FileKind::Run));
        assert_eq!(file_kind("x.DEB"), Some(FileKind::Deb));
        assert_eq!(file_kind("x.deb2"), None);
    }

    #[test]
    fn run_header_extracts_candidates() {
        let mut prefix = b"#!/bin/sh\n".to_vec();
        prefix.extend_from_slice(
            b"label=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\n",
        );
        prefix.extend_from_slice(&[0u8; 16]);
        let candidates = run_header(&prefix).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].text(), "580.178.04");
        assert_eq!(run_header(b"#!"), Err("run_not_linux_x86_64"));
        assert_eq!(run_header(b"xy"), Err("run_not_shebang"));
        assert_eq!(run_header(b"#"), Err("header_unreadable"));
    }

    #[test]
    fn deb_versions() {
        assert!(deb_version_ok("1.20.1-1").is_some());
        assert!(deb_version_ok("1.19").is_some());
        assert!(deb_version_ok("1").is_none());
        assert!(deb_version_ok("1.20.1-x").is_none());
        assert_eq!(
            toolkit_cmp("1.19.0", "1.19.1"),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            toolkit_cmp("1.20.1", "1.19.1"),
            Some(std::cmp::Ordering::Greater)
        );
    }

    fn group_with(run: Option<RunJson>, debs: &[(&str, &str)]) -> GroupJson {
        GroupJson {
            run,
            debs: debs
                .iter()
                .map(|(n, v)| {
                    (
                        n.to_string(),
                        DebJson {
                            sha256: "a".repeat(64),
                            size: 1,
                            version: (*v).to_string(),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn summary_and_resolve() {
        let tmp = TestDir::new("upload");
        let p = Paths::new(&*tmp);
        store::prepare_dirs(&RealHost::on_host(), &p).unwrap();
        let h = RealHost::on_host();
        let st = UploadState::new();
        let group = open_group(&h, &p, &st).unwrap();
        // Empty group.
        let s = summary(&h, &p, &group);
        assert_eq!(s.missing.len(), 4);
        assert!(s.run.is_none());
        assert!(resolve(&s, Some("1.2.3")).is_err());
        // Fill the group.
        let g = group_with(
            Some(RunJson {
                sha256: "b".repeat(64),
                size: 10,
                candidates: vec!["580.178.04".into()],
            }),
            &[
                ("libnvidia-container1", "1.20.1-1"),
                ("libnvidia-container-tools", "1.20.1-1"),
                ("nvidia-container-toolkit-base", "1.20.1-1"),
                ("nvidia-container-toolkit", "1.20.1-1"),
            ],
        );
        write_group(&h, &p, &group, &g).unwrap();
        let s = summary(&h, &p, &group);
        assert_eq!(s.toolkit_version.as_deref(), Some("1.20.1"));
        assert!(!s.below_security_floor);
        assert!(s.problems.is_empty());
        assert!(resolve(&s, None).is_err());
        assert!(resolve(&s, Some("999.1.1")).is_err());
        let src = resolve(&s, Some("580.178.04")).unwrap();
        assert_eq!(src.fingerprint, "b".repeat(FINGERPRINT_HEX));
        // Old toolkit for a 615 driver.
        let g = group_with(
            Some(RunJson {
                sha256: "b".repeat(64),
                size: 10,
                candidates: vec!["615.71.09".into()],
            }),
            &[
                ("libnvidia-container1", "1.19.0-1"),
                ("libnvidia-container-tools", "1.19.0-1"),
                ("nvidia-container-toolkit-base", "1.19.0-1"),
                ("nvidia-container-toolkit", "1.19.0-1"),
            ],
        );
        write_group(&h, &p, &group, &g).unwrap();
        let s = summary(&h, &p, &group);
        assert!(s.below_security_floor);
        assert!(matches!(
            resolve(&s, Some("615.71.09")),
            Err(SourceInvalid::ToolkitTooOld { .. })
        ));
    }
}

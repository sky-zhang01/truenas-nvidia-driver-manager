//! Readings and pure parsers of host facts.
//!
//! Every parser here is a pure function with its own unit tests; the reading functions take a
//! [`Host`] and return a [`Reading`] so callers can show UNKNOWN with a reason.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use crate::codes::*;
use crate::host::{At, Cmd, Host};
use serde_json::json;

/// A value or a reason why it could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading<T> {
    Value(T),
    Unknown(&'static str),
}

impl<T> Reading<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Reading::Value(v) => Some(v),
            Reading::Unknown(_) => None,
        }
    }

    pub fn is_value(&self) -> bool {
        matches!(self, Reading::Value(_))
    }

    pub fn unknown_reason(&self) -> Option<&'static str> {
        match self {
            Reading::Unknown(r) => Some(r),
            Reading::Value(_) => None,
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Reading<U> {
        match self {
            Reading::Value(v) => Reading::Value(f(v)),
            Reading::Unknown(r) => Reading::Unknown(r),
        }
    }

    pub fn as_ref(&self) -> Reading<&T> {
        match self {
            Reading::Value(v) => Reading::Value(v),
            Reading::Unknown(r) => Reading::Unknown(r),
        }
    }
}

/// The five driver modules shipped by the driver image.
pub const DRIVER_MODULES: [&str; 5] = [
    "nvidia",
    "nvidia_uvm",
    "nvidia_modeset",
    "nvidia_drm",
    "nvidia_peermem",
];

/// Legal unload order: dependants first.
pub const UNLOAD_ORDER: [&str; 5] = [
    "nvidia_drm",
    "nvidia_modeset",
    "nvidia_peermem",
    "nvidia_uvm",
    "nvidia",
];

/// Modules loaded when a task starts with no driver module loaded.
pub const DEFAULT_RELOAD: [&str; 3] = ["nvidia", "nvidia_drm", "nvidia_modeset"];

/// One line of `/proc/modules`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleLine {
    pub name: String,
    pub size: u64,
    pub refcount: u64,
    pub deps: Vec<String>,
}

/// A parse failure of a whole reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// Parse `/proc/modules`. Any invalid line fails the reading.
pub fn parse_proc_modules(s: &str) -> Result<Vec<ModuleLine>, ParseError> {
    let mut out = Vec::new();
    for (idx, line) in s.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 6 {
            return Err(ParseError(format!(
                "line {} has fewer than 6 fields",
                idx + 1
            )));
        }
        let size = f[1]
            .parse::<u64>()
            .map_err(|_| ParseError(format!("line {}: bad size", idx + 1)))?;
        let refcount = f[2]
            .parse::<u64>()
            .map_err(|_| ParseError(format!("line {}: bad refcount", idx + 1)))?;
        let deps = if f[3] == "-" {
            Vec::new()
        } else {
            f[3].split(',')
                .filter(|d| !d.is_empty() && !d.starts_with('['))
                .map(|d| d.to_string())
                .collect()
        };
        out.push(ModuleLine {
            name: f[0].to_string(),
            size,
            refcount,
            deps,
        });
    }
    Ok(out)
}

/// True when at least one loaded `nvidia*` module has more references than modules depending
/// on it.
pub fn occupancy(modules: &[ModuleLine]) -> bool {
    modules
        .iter()
        .any(|m| m.name.starts_with("nvidia") && m.refcount > m.deps.len() as u64)
}

/// Human-readable readings for the `log.occupancy` line.
pub fn module_readings(modules: &[ModuleLine]) -> String {
    let loaded: Vec<&ModuleLine> = modules
        .iter()
        .filter(|m| m.name.starts_with("nvidia"))
        .collect();
    if loaded.is_empty() {
        return "none".to_string();
    }
    loaded
        .iter()
        .map(|m| format!("{} {}/{}", m.name, m.refcount, m.deps.len()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Loaded driver modules in a legal unload order.
pub fn unload_plan(modules: &[ModuleLine]) -> Vec<&'static str> {
    let loaded: BTreeSet<&str> = modules.iter().map(|m| m.name.as_str()).collect();
    UNLOAD_ORDER
        .iter()
        .copied()
        .filter(|name| loaded.contains(name))
        .collect()
}

/// Basis of the reload target set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReloadBasis {
    LoadedAtStart,
    NoneLoadedAtStart,
    UnloadedInUpdate,
}

impl ReloadBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            ReloadBasis::LoadedAtStart => "loaded_at_start",
            ReloadBasis::NoneLoadedAtStart => "none_loaded_at_start",
            ReloadBasis::UnloadedInUpdate => "unloaded_in_update",
        }
    }

    pub fn parse(s: &str) -> Option<ReloadBasis> {
        Some(match s {
            "loaded_at_start" => ReloadBasis::LoadedAtStart,
            "none_loaded_at_start" => ReloadBasis::NoneLoadedAtStart,
            "unloaded_in_update" => ReloadBasis::UnloadedInUpdate,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadTargets {
    pub basis: ReloadBasis,
    pub modules: Vec<&'static str>,
}

/// Reload targets from the initial `/proc/modules` reading. `None` means the reading itself
/// was unreadable.
pub fn reload_targets(start: Option<&[ModuleLine]>) -> Option<ReloadTargets> {
    let modules = start?;
    let loaded: BTreeSet<&str> = modules.iter().map(|m| m.name.as_str()).collect();
    let present: Vec<&'static str> = UNLOAD_ORDER
        .iter()
        .copied()
        .filter(|name| loaded.contains(name))
        .collect();
    if present.is_empty() {
        Some(ReloadTargets {
            basis: ReloadBasis::NoneLoadedAtStart,
            modules: DEFAULT_RELOAD.to_vec(),
        })
    } else {
        let mut targets = present;
        targets.reverse();
        Some(ReloadTargets {
            basis: ReloadBasis::LoadedAtStart,
            modules: targets,
        })
    }
}

/// Reload targets when the initial reading could not read `/proc/modules`.
pub fn reload_targets_from_unloaded(unloaded: &[&str]) -> ReloadTargets {
    if unloaded.is_empty() {
        ReloadTargets {
            basis: ReloadBasis::UnloadedInUpdate,
            modules: DEFAULT_RELOAD.to_vec(),
        }
    } else {
        let mut targets: Vec<&'static str> = UNLOAD_ORDER
            .iter()
            .copied()
            .filter(|name| unloaded.contains(name))
            .collect();
        targets.reverse();
        ReloadTargets {
            basis: ReloadBasis::UnloadedInUpdate,
            modules: targets,
        }
    }
}

/// A version number: two to four groups of one to nine digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Version {
    text: String,
    parts: Vec<u32>,
}

impl Version {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn parts(&self) -> &[u32] {
        &self.parts
    }

    pub fn major(&self) -> u32 {
        self.parts.first().copied().unwrap_or(0)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.parts.cmp(&other.parts)
    }
}

/// Parse a version number. Fact text may carry surrounding whitespace, so it is trimmed here;
/// request parameters use [`parse_version_exact`] instead.
pub fn parse_version(s: &str) -> Option<Version> {
    parse_version_exact(s.trim())
}

/// [`parse_version`] without the trim: the whole string must match
/// `^[0-9]{1,9}(\.[0-9]{1,9}){1,3}$`.
pub fn parse_version_exact(s: &str) -> Option<Version> {
    let parts: Vec<&str> = s.split('.').collect();
    if !(2..=4).contains(&parts.len()) {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in &parts {
        if p.is_empty()
            || p.len() > VERSION_SEGMENT_DIGITS
            || !p.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        nums.push(p.parse::<u32>().ok()?);
    }
    Some(Version {
        text: s.to_string(),
        parts: nums,
    })
}

/// `NVRM version:` line: first dotted version, and whether the line mentions the open kernel
/// modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvrmVersion {
    pub version: Version,
    pub open: bool,
}

pub fn parse_nvrm_version(s: &str) -> Option<NvrmVersion> {
    for line in s.lines() {
        if !line.contains("NVRM version:") {
            continue;
        }
        let open = line.contains("Open Kernel Module");
        for token in line.split(|c: char| c.is_whitespace() || c == ',') {
            let token = token.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
            if let Some(v) = parse_version(token) {
                return Some(NvrmVersion { version: v, open });
            }
        }
    }
    None
}

/// `nvidia-smi` version output: every non-empty line must be equal.
pub fn parse_smi_versions(stdout: &str) -> Option<String> {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let first = lines[0].to_string();
    if lines.iter().all(|l| *l == first) {
        Some(first)
    } else {
        None
    }
}

/// The mutually exclusive `/usr` readings of `systemd-sysext status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysextUsr {
    Nvidia(u64),
    Unmerged,
    OtherExtensions,
}

/// Parse the single `/usr` hierarchy, including the exact unmerged shape.
/// Other hierarchies cannot supply either its merge time or its confirmation.
pub fn parse_sysext_status(json: &str) -> Result<SysextUsr, ParseError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| ParseError(format!("not JSON: {e}")))?;
    let arr = value
        .as_array()
        .ok_or_else(|| ParseError("not an array".to_string()))?;
    let mut usr = None;
    for item in arr {
        let Some(obj) = item.as_object() else {
            return Err(ParseError("item is not an object".to_string()));
        };
        let hierarchy = obj
            .get("hierarchy")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ParseError("item has no hierarchy".to_string()))?;
        if hierarchy != "/usr" {
            continue;
        }
        if usr.is_some() {
            return Err(ParseError("duplicate /usr hierarchy".to_string()));
        }
        let extensions = obj
            .get("extensions")
            .ok_or_else(|| ParseError("item has no extensions".to_string()))?;
        let since = obj
            .get("since")
            .ok_or_else(|| ParseError("item has no since".to_string()))?;
        usr = Some(match extensions {
            serde_json::Value::String(s) if s == "none" && since.is_null() => SysextUsr::Unmerged,
            serde_json::Value::Array(items) if items.iter().all(serde_json::Value::is_string) => {
                let since = since
                    .as_u64()
                    .ok_or_else(|| ParseError("no merge time".to_string()))?;
                if items.iter().any(|v| v.as_str() == Some("nvidia")) {
                    SysextUsr::Nvidia(since)
                } else {
                    SysextUsr::OtherExtensions
                }
            }
            _ => {
                return Err(ParseError("invalid /usr extensions or since".to_string()));
            }
        });
    }
    usr.ok_or_else(|| ParseError("no /usr hierarchy".to_string()))
}

/// The `/usr` nvidia extension's merge time. `None` is a valid `/usr` row without a nvidia
/// merge, never a missing or malformed row.
pub fn parse_sysext_since(json: &str) -> Result<Option<u64>, ParseError> {
    Ok(match parse_sysext_status(json)? {
        SysextUsr::Nvidia(since) => Some(since),
        SysextUsr::Unmerged | SysextUsr::OtherExtensions => None,
    })
}

/// The `systemd-sysext status` command, shared by the runner stages and the
/// ctx-free readers below so both always ask the host the same question. The
/// budget is the caller's: the runner checks keep their long
/// `HOST_CMD_TIMEOUT`, the ctx-free readers use `HOST_QUERY_TIMEOUT`.
pub fn sysext_status_cmd(timeout: std::time::Duration) -> Cmd {
    Cmd::new(At::Host, "systemd-sysext", timeout)
        .arg("status")
        .arg("--json=short")
}

/// The `nvidia-smi` driver version query, shared like [`sysext_status_cmd`].
pub fn nvidia_smi_version_cmd(timeout: std::time::Duration) -> Cmd {
    Cmd::new(At::Host, "nvidia-smi", timeout)
        .arg("--query-gpu=driver_version")
        .arg("--format=csv,noheader")
}

/// The UNKNOWN reason of a command that did not end with exit code 0, with the
/// same classification the runner stages use.
pub fn command_unknown_reason(exit: crate::host::Exit) -> &'static str {
    use crate::host::Exit;
    match exit {
        Exit::TimedOut => REASON_TIMED_OUT,
        Exit::SpawnFailed => REASON_SPAWN_FAILED,
        Exit::Code(_) => REASON_EXIT_STATUS,
        Exit::Signal(_) => REASON_SIGNAL,
    }
}

/// The merge time of the current `/usr` nvidia extension, read without a
/// runner context. The recovery proof binds one recorded `since` to this
/// physical reading; the argv and the parser are the runner's own.
pub fn sysext_nvidia_since_now(h: &dyn Host) -> Reading<u64> {
    let out = h.run(&sysext_status_cmd(HOST_QUERY_TIMEOUT));
    if !out.exit.ok() {
        return Reading::Unknown(command_unknown_reason(out.exit));
    }
    match parse_sysext_status(&out.stdout_text()) {
        Ok(SysextUsr::Nvidia(since)) => Reading::Value(since),
        _ => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
    }
}

/// The current user-space driver version, read without a runner context.
pub fn nvidia_smi_version_now(h: &dyn Host) -> Reading<String> {
    let out = h.run(&nvidia_smi_version_cmd(HOST_QUERY_TIMEOUT));
    if !out.exit.ok() {
        return Reading::Unknown(command_unknown_reason(out.exit));
    }
    match parse_smi_versions(&out.stdout_text()) {
        Some(version) => Reading::Value(version),
        None => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
    }
}

/// The current, complete `/proc/modules` reading. The product's processes run
/// with the host PID namespace (the same assumption the web-side occupancy
/// reader already makes), so this is the host's module list.
pub fn proc_modules_now(h: &dyn Host) -> Reading<Vec<ModuleLine>> {
    match h.read(Path::new(PROC_MODULES_PATH)) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => match parse_proc_modules(&text) {
                Ok(modules) => Reading::Value(modules),
                Err(_) => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
            },
            Err(_) => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
        },
        Err(e) => Reading::Unknown(crate::log::io_reason_key(&e)),
    }
}

/// `/data/manifest.json` version.
pub fn parse_manifest_version(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let v = value.get("version")?.as_str()?.trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// One line of `/proc/self/mounts` or `/proc/1/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountLine {
    pub source: String,
    pub mount_point: String,
    pub fstype: String,
    pub options: String,
}

impl MountLine {
    pub fn is_rw(&self) -> bool {
        is_rw(&self.options)
    }
}

/// True when the options field has `rw` and no `ro`.
pub fn is_rw(options: &str) -> bool {
    let mut rw = false;
    let mut ro = false;
    for opt in options.split(',') {
        match opt {
            "rw" => rw = true,
            "ro" => ro = true,
            _ => {}
        }
    }
    rw && !ro
}

/// Decode the `\040` style octal escapes of the mount tables.
pub fn unescape_octal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &bytes[i + 1..i + 4];
            if oct.iter().all(|b| (b'0'..=b'7').contains(b)) {
                let value = (oct[0] - b'0') as u32 * 64
                    + (oct[1] - b'0') as u32 * 8
                    + (oct[2] - b'0') as u32;
                out.push(value as u8);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse `/proc/self/mounts` (six fields, octal escapes decoded).
pub fn parse_mounts(s: &str) -> Vec<MountLine> {
    let mut out = Vec::new();
    for line in s.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 6 {
            continue;
        }
        out.push(MountLine {
            source: unescape_octal(f[0]),
            mount_point: unescape_octal(f[1]),
            fstype: f[2].to_string(),
            options: f[3].to_string(),
        });
    }
    out
}

/// A `/proc/<pid>/mountinfo` line with its root field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfoLine {
    pub root: String,
    pub line: MountLine,
}

/// Parse `/proc/<pid>/mountinfo`: root is field 4, mount point field 5, mount
/// options field 6; fstype and source come after the `-` separator.
pub fn parse_mountinfo(s: &str) -> Vec<MountInfoLine> {
    let mut out = Vec::new();
    for line in s.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 {
            continue;
        }
        let Some(dash) = f.iter().position(|x| *x == "-") else {
            continue;
        };
        if f.len() < dash + 3 || dash < 6 {
            continue;
        }
        out.push(MountInfoLine {
            root: unescape_octal(f[3]),
            line: MountLine {
                source: unescape_octal(f[dash + 2]),
                mount_point: unescape_octal(f[4]),
                fstype: f[dash + 1].to_string(),
                options: f[5].to_string(),
            },
        });
    }
    out
}

/// One entry of an `unsquashfs -lls` listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEntry {
    pub kind: EntryKind,
    pub path: String,
    pub mode: String,
    pub link_target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// Reject the entire `unsquashfs -lls` listing when any line is malformed.
pub fn parse_unsquashfs_lls(s: &str) -> Result<Vec<ImageEntry>, ParseError> {
    let mut out = Vec::new();
    for (idx, raw) in s.lines().enumerate() {
        let line_no = idx + 1;
        if raw.is_empty() {
            continue;
        }
        if raw.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f) {
            return Err(ParseError(format!("line {line_no}: control character")));
        }
        let mode_end = raw
            .find(char::is_whitespace)
            .ok_or_else(|| ParseError(format!("line {line_no}: no owner field")))?;
        let mode = &raw[..mode_end];
        if mode.chars().count() != 10 {
            return Err(ParseError(format!("line {line_no}: bad permission string")));
        }
        let rest = &raw[mode_end..];
        let mut fields = rest.split_whitespace().peekable();
        let _owner = fields
            .next()
            .ok_or_else(|| ParseError(format!("line {line_no}: no owner")))?;

        // Scan forward to the date field, remembering its byte offset. The
        // scan stops when only whitespace is left; every step moves past the
        // token it just read, so the loop always advances.
        let mut offset = mode_end;
        let total = raw.len();
        let mut date_end = None;
        while offset < total {
            let Some(rel) = raw[offset..].find(|c: char| !c.is_whitespace()) else {
                break;
            };
            let start = offset + rel;
            let end = raw[start..]
                .find(char::is_whitespace)
                .map(|i| start + i)
                .unwrap_or(total);
            if is_date_token(&raw[start..end]) {
                date_end = Some(end);
                break;
            }
            offset = end;
        }
        let Some(date_end) = date_end else {
            return Err(ParseError(format!("line {line_no}: no date field")));
        };
        // The time field follows the date.
        let Some(rel) = raw[date_end..].find(|c: char| !c.is_whitespace()) else {
            return Err(ParseError(format!("line {line_no}: no time field")));
        };
        let time_start = date_end + rel;
        let time_end = raw[time_start..]
            .find(char::is_whitespace)
            .map(|i| time_start + i)
            .ok_or_else(|| ParseError(format!("line {line_no}: no time field")))?;
        let time = &raw[time_start..time_end];
        if !is_time_token(time) {
            return Err(ParseError(format!("line {line_no}: bad time field")));
        }
        let mut path = raw[time_end..].trim_start().to_string();
        if path.is_empty() {
            return Err(ParseError(format!("line {line_no}: no path")));
        }
        let kind = match mode.as_bytes()[0] {
            b'-' => EntryKind::File,
            b'd' => EntryKind::Dir,
            b'l' => EntryKind::Symlink,
            _ => EntryKind::Other,
        };
        let mut link_target = None;
        if kind == EntryKind::Symlink {
            if let Some(pos) = path.find(" -> ") {
                link_target = Some(path[pos + 4..].to_string());
                path.truncate(pos);
            }
        }
        let Some(rel) = path.strip_prefix("squashfs-root") else {
            return Err(ParseError(format!(
                "line {line_no}: path does not start with squashfs-root"
            )));
        };
        let rel = rel.trim_start_matches('/').to_string();
        if rel.is_empty() {
            // The listing starts with the `squashfs-root` directory itself; it carries no
            // relative path and is skipped. Anything else with an empty path stays invalid.
            if kind == EntryKind::Dir {
                continue;
            }
            return Err(ParseError(format!("line {line_no}: empty path")));
        }
        out.push(ImageEntry {
            kind,
            path: rel,
            mode: mode.to_string(),
            link_target,
        });
    }
    Ok(out)
}

fn is_date_token(token: &str) -> bool {
    let b = token.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
}

fn is_time_token(token: &str) -> bool {
    let b = token.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && b[0..2].iter().all(u8::is_ascii_digit)
        && b[3..5].iter().all(u8::is_ascii_digit)
}

/// Facts taken from a driver image listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFacts {
    pub target_kernel: Reading<String>,
    pub file_version: Reading<String>,
    /// The three toolkit programs: (name, is a regular file with the owner
    /// execute bit).
    pub tools: Vec<(&'static str, Reading<bool>)>,
}

pub const TOOLKIT_PROGRAMS: [&str; 3] = [
    "usr/bin/nvidia-container-runtime",
    "usr/bin/nvidia-ctk",
    "usr/bin/nvidia-container-runtime-hook",
];

/// Derive the target kernel, file version and toolkit programs.
pub fn image_facts(entries: &[ImageEntry]) -> ImageFacts {
    let mut kernels: Vec<&str> = Vec::new();
    for e in entries {
        if e.kind == EntryKind::Dir {
            if let Some(rest) = e.path.strip_prefix("usr/lib/modules/") {
                if !rest.is_empty() && !rest.contains('/') {
                    kernels.push(rest);
                }
            }
        }
    }
    kernels.sort_unstable();
    kernels.dedup();
    let target_kernel = if kernels.len() == 1 {
        Reading::Value(kernels[0].to_string())
    } else {
        // Not a unique value: the reason is "the content format is not recognized", never an
        // internal marker.
        Reading::Unknown(REASON_FORMAT_UNRECOGNIZED)
    };

    let mut versions = Vec::new();
    for e in entries {
        if e.kind == EntryKind::File {
            if let Some(rest) = e.path.strip_prefix("usr/lib/x86_64-linux-gnu/") {
                if rest.contains('/') {
                    continue;
                }
                if let Some(suffix) = rest.strip_prefix("libnvidia-ml.so.") {
                    if parse_version(suffix).is_some() {
                        versions.push(suffix.to_string());
                    }
                }
            }
        }
    }
    versions.sort();
    versions.dedup();
    let file_version = if versions.len() == 1 {
        Reading::Value(versions[0].clone())
    } else {
        Reading::Unknown(REASON_FORMAT_UNRECOGNIZED)
    };

    let tools = TOOLKIT_PROGRAMS
        .iter()
        .map(|name| {
            let matches: Vec<&ImageEntry> = entries.iter().filter(|e| e.path == *name).collect();
            if matches.len() != 1 {
                (*name, Reading::Unknown(REASON_FORMAT_UNRECOGNIZED))
            } else {
                let e = matches[0];
                let ok =
                    e.kind == EntryKind::File && e.mode.as_bytes().get(3).copied() == Some(b'x');
                (*name, Reading::Value(ok))
            }
        })
        .collect();
    ImageFacts {
        target_kernel,
        file_version,
        tools,
    }
}

/// An NVIDIA display device read from sysfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostGpu {
    pub address: String,
    pub device: u16,
    pub driver: Option<String>,
}

/// Why the GPU scan could not be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpusUnreadable {
    pub reason: &'static str,
    pub path: String,
}

/// `0x` plus `digits` hexadecimal digits, with an optional trailing newline.
pub fn parse_sysfs_hex(s: &str, digits: usize) -> Option<u32> {
    // The kernel prints `0x%0*x\n` (Linux v6.12 `drivers/pci/pci-sysfs.c`): the width is
    // exact, the hex digits are lowercase, and the only byte after the value is at most one
    // newline; any other whitespace, width or uppercase digit makes the reading unreadable.
    let body = s.strip_suffix('\n').unwrap_or(s);
    if body.chars().any(char::is_whitespace) {
        return None;
    }
    let rest = body.strip_prefix("0x")?;
    if rest.len() != digits
        || !rest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    u32::from_str_radix(rest, 16).ok()
}

fn pci_address_ok(name: &str) -> bool {
    // Linux names PCI functions `%04x:%02x:%02x.%d`, so the regex is lowercase-only:
    // `^[0-9a-f]{4,8}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$`.
    let lower_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    let Some(colon1) = name.find(':') else {
        return false;
    };
    if !(4..=8).contains(&colon1) || !name[..colon1].bytes().all(lower_hex) {
        return false;
    }
    let Some(colon2_rel) = name[colon1 + 1..].find(':') else {
        return false;
    };
    let colon2 = colon1 + 1 + colon2_rel;
    if colon2 - colon1 - 1 != 2 || !name[colon1 + 1..colon2].bytes().all(lower_hex) {
        return false;
    }
    let Some(dot_rel) = name[colon2 + 1..].find('.') else {
        return false;
    };
    let dot = colon2 + 1 + dot_rel;
    if dot - colon2 - 1 != 2 || !name[colon2 + 1..dot].bytes().all(lower_hex) {
        return false;
    }
    let func = &name[dot + 1..];
    func.len() == 1 && func.bytes().all(|c| (b'0'..=b'7').contains(&c))
}

fn io_reason(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => REASON_FILE_MISSING,
        std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
        std::io::ErrorKind::TimedOut => REASON_TIMED_OUT,
        _ => REASON_IO_ERROR,
    }
}

/// Read the host's NVIDIA display devices.
pub fn read_gpus(h: &dyn Host) -> Result<Vec<HostGpu>, GpusUnreadable> {
    // The whole-reading deadline starts on the host clock, so a hypothetical dialed clock
    // cannot pre-expire a fresh reading.
    read_gpus_within(h, h.now())
}

/// [`read_gpus`] with the start of its whole-reading deadline: GPU reading gives the whole
/// listing `READING_TIMEOUT`, so a listing that keeps going past it is a reading failure,
/// never a partial success.
pub fn read_gpus_within(
    h: &dyn Host,
    start: std::time::Instant,
) -> Result<Vec<HostGpu>, GpusUnreadable> {
    let gpus_timed_out = || GpusUnreadable {
        reason: REASON_TIMED_OUT,
        path: PCI_DEVICES_PATH.to_string(),
    };
    let deadline_hit = |h: &dyn Host| h.now().saturating_duration_since(start) >= READING_TIMEOUT;
    if deadline_hit(h) {
        return Err(gpus_timed_out());
    }
    let dir = h.view(Path::new(PCI_DEVICES_PATH));
    let names = h.read_dir(&dir).map_err(|e| GpusUnreadable {
        reason: io_reason(&e),
        path: PCI_DEVICES_PATH.to_string(),
    })?;
    let mut out = Vec::new();
    for name in names {
        if deadline_hit(h) {
            return Err(gpus_timed_out());
        }
        let name = name.to_string_lossy().into_owned();
        if !pci_address_ok(&name) {
            return Err(GpusUnreadable {
                reason: REASON_FORMAT_UNRECOGNIZED,
                path: format!("{PCI_DEVICES_PATH}/{name}"),
            });
        }
        let base = dir.join(&name);
        let vendor_path = base.join("vendor");
        let class_path = base.join("class");
        let vendor = h.read(&vendor_path).map_err(|e| GpusUnreadable {
            reason: io_reason(&e),
            path: format!("{PCI_DEVICES_PATH}/{name}/vendor"),
        })?;
        let class = h.read(&class_path).map_err(|e| GpusUnreadable {
            reason: io_reason(&e),
            path: format!("{PCI_DEVICES_PATH}/{name}/class"),
        })?;
        let vendor = parse_sysfs_hex(&String::from_utf8_lossy(&vendor), 4).ok_or_else(|| {
            GpusUnreadable {
                reason: REASON_FORMAT_UNRECOGNIZED,
                path: format!("{PCI_DEVICES_PATH}/{name}/vendor"),
            }
        })?;
        let class =
            parse_sysfs_hex(&String::from_utf8_lossy(&class), 6).ok_or_else(|| GpusUnreadable {
                reason: REASON_FORMAT_UNRECOGNIZED,
                path: format!("{PCI_DEVICES_PATH}/{name}/class"),
            })?;
        if vendor != 0x10de || (class >> 8 != 0x0300 && class >> 8 != 0x0302) {
            continue;
        }
        let device_path = base.join("device");
        let device = h.read(&device_path).map_err(|e| GpusUnreadable {
            reason: io_reason(&e),
            path: format!("{PCI_DEVICES_PATH}/{name}/device"),
        })?;
        let device = parse_sysfs_hex(&String::from_utf8_lossy(&device), 4).ok_or_else(|| {
            GpusUnreadable {
                reason: REASON_FORMAT_UNRECOGNIZED,
                path: format!("{PCI_DEVICES_PATH}/{name}/device"),
            }
        })?;
        let driver = match h.read_link(&base.join("driver")) {
            Ok(target) => target.file_name().map(|n| n.to_string_lossy().into_owned()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(GpusUnreadable {
                    reason: io_reason(&e),
                    path: format!("{PCI_DEVICES_PATH}/{name}/driver"),
                })
            }
        };
        out.push(HostGpu {
            address: name,
            device: device as u16,
            driver,
        });
    }
    if deadline_hit(h) {
        return Err(gpus_timed_out());
    }
    Ok(out)
}

/// GPUs counted for the driver: everything not bound to `vfio-pci`.
pub fn counted(gpus: &[HostGpu]) -> Vec<&HostGpu> {
    gpus.iter()
        .filter(|g| g.driver.as_deref() != Some("vfio-pci"))
        .collect()
}

/// The loaded driver version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadedVersion {
    Value(Version),
    NotLoaded,
    Unknown(&'static str),
}

/// loaded version reading; when the file is missing, look at the `/proc/modules` reading. A
/// read that completes only after the reading's `READING_TIMEOUT` is UNKNOWN, never a value.
pub fn loaded_version(h: &dyn Host, modules: Option<&Reading<Vec<ModuleLine>>>) -> LoadedVersion {
    let start = h.now();
    let result = match h.read(Path::new(NVRM_VERSION_PATH)) {
        Ok(bytes) => match parse_nvrm_version(&String::from_utf8_lossy(&bytes)) {
            Some(v) => LoadedVersion::Value(v.version),
            None => LoadedVersion::Unknown(REASON_FORMAT_UNRECOGNIZED),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match modules {
            Some(Reading::Value(list)) if !list.iter().any(|m| m.name == "nvidia") => {
                LoadedVersion::NotLoaded
            }
            Some(Reading::Value(_)) => LoadedVersion::Unknown(REASON_RECORD_UNREADABLE),
            Some(Reading::Unknown(r)) => LoadedVersion::Unknown(r),
            None => LoadedVersion::Unknown(REASON_FILE_MISSING),
        },
        Err(e) => LoadedVersion::Unknown(io_reason(&e)),
    };
    if h.now().saturating_duration_since(start) >= READING_TIMEOUT {
        return LoadedVersion::Unknown(REASON_TIMED_OUT);
    }
    result
}

/// TrueNAS version reading: TrueNAS version.
pub fn truenas_version(h: &dyn Host) -> Reading<String> {
    let start = h.now();
    let result = match h.read(&h.view(Path::new(MANIFEST_PATH))) {
        Ok(bytes) => match parse_manifest_version(&String::from_utf8_lossy(&bytes)) {
            Some(v) => Reading::Value(v),
            None => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
        },
        Err(e) => Reading::Unknown(io_reason(&e)),
    };
    if h.now().saturating_duration_since(start) >= READING_TIMEOUT {
        return Reading::Unknown(REASON_TIMED_OUT);
    }
    result
}

/// kernel release reading: current kernel release.
pub fn kernel(h: &dyn Host) -> Reading<String> {
    match h.uname_release() {
        Ok(k) if kernel_ok(&k) => Reading::Value(k),
        Ok(_) => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
        Err(e) => Reading::Unknown(io_reason(&e)),
    }
}

pub fn kernel_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// extension link reading: the extension link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkReading {
    Missing,
    Ok,
    Elsewhere(String),
    Unknown(&'static str),
}

pub fn link(h: &dyn Host) -> LinkReading {
    let start = h.now();
    let p = h.view(Path::new(LINK_PATH));
    let result = match h.lstat(&p) {
        Ok(_) => match h.read_link(&p) {
            Ok(target) => {
                if target == Path::new(CANONICAL_PATH) {
                    LinkReading::Ok
                } else {
                    LinkReading::Elsewhere(target.to_string_lossy().into_owned())
                }
            }
            Err(e) => LinkReading::Unknown(io_reason(&e)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LinkReading::Missing,
        Err(e) => LinkReading::Unknown(io_reason(&e)),
    };
    if h.now().saturating_duration_since(start) >= READING_TIMEOUT {
        return LinkReading::Unknown(REASON_TIMED_OUT);
    }
    result
}

/// canonical checksum reading: whole-file sha256 of the permanent path, under one
/// `READING_TIMEOUT` deadline. A read that stops at the deadline is UNKNOWN, so driver list
/// reports `canonical_unreadable` and driver deletion refuses the deletion.
pub fn canonical_sha256(h: &dyn Host) -> Reading<String> {
    let deadline = h.now() + READING_TIMEOUT;
    match crate::store::hash_file_within(h, &h.view(Path::new(CANONICAL_PATH)), deadline) {
        Ok(copied) => Reading::Value(copied.sha256),
        Err(e) => Reading::Unknown(io_reason(&e)),
    }
}

/// Why the permanent-path listing could not be read. The reason is one of the general phrases
/// and carries its placeholder values (the exit status its code, a signal its number, a
/// timeout its seconds); `raw` is the command output for callers that show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsFailure {
    pub key: &'static str,
    pub params: serde_json::Value,
    pub raw: Option<String>,
}

impl FactsFailure {
    /// The `{key, params}` reason shape.
    pub fn reason_json(&self) -> serde_json::Value {
        serde_json::json!({"key": self.key, "params": self.params})
    }
}

/// image listing: the driver version inside the file at the permanent path.
pub fn canonical_facts(h: &dyn Host) -> Result<ImageFacts, FactsFailure> {
    // The listing runs on the host with the host path, the very argv [`image_facts_command`]
    // records for the runner: the web container executes no program from its own image.
    let out = h.run(&image_facts_command(
        Path::new(CANONICAL_PATH),
        READING_TIMEOUT,
    ));
    if !out.exit.ok() {
        let raw = out.stderr_text();
        let (key, params) = match out.exit {
            crate::host::Exit::TimedOut => (
                REASON_TIMED_OUT,
                json!({"seconds": READING_TIMEOUT.as_secs()}),
            ),
            crate::host::Exit::SpawnFailed => (REASON_SPAWN_FAILED, json!({})),
            crate::host::Exit::Code(code) => (REASON_EXIT_STATUS, json!({"code": code})),
            crate::host::Exit::Signal(signal) => (REASON_SIGNAL, json!({"signal": signal})),
        };
        let failure = FactsFailure {
            key,
            params,
            raw: (!raw.is_empty()).then_some(raw),
        };
        return Err(failure);
    }
    match parse_unsquashfs_lls_bytes(&out.stdout) {
        Ok(entries) => Ok(image_facts(&entries)),
        Err(_) => Err(FactsFailure {
            key: REASON_FORMAT_UNRECOGNIZED,
            params: json!({}),
            raw: None,
        }),
    }
}

/// The /image listing listing command, so callers that record host commands (the runner's
/// `RunCtx::cmd`) can run the very same argv.
pub fn image_facts_command(listing_path: &Path, timeout: std::time::Duration) -> Cmd {
    Cmd::new(At::Host, "unsquashfs", timeout)
        .arg("-lls")
        .arg(listing_path.as_os_str())
}

/// [`parse_unsquashfs_lls`] for the raw bytes of a command's output: an invalid UTF-8 byte
/// anywhere makes the whole listing unreadable.
pub fn parse_unsquashfs_lls_bytes(bytes: &[u8]) -> Result<Vec<ImageEntry>, ParseError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ParseError("listing is not valid UTF-8".to_string()))?;
    parse_unsquashfs_lls(text)
}

/// Parse one listing command's output into the image facts. A command that did not exit 0 is
/// a parse failure whose reason is the verbatim external output.
pub fn image_facts_from_output(out: &crate::host::Output) -> Result<ImageFacts, ParseError> {
    if !out.exit.ok() {
        return Err(ParseError(out.stderr_text()));
    }
    let entries = parse_unsquashfs_lls_bytes(&out.stdout)?;
    Ok(image_facts(&entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_modules_parses_and_rejects_bad_lines() {
        let s = "nvidia 12345 12 nvidia_uvm,nvidia_modeset Live 0x0000000000000000\n\
                 nvidia_uvm 100 0 - Live 0x0 [permanent]\n\
                 nvidia_modeset 50 1 nvidia Live 0x0\n";
        let m = parse_proc_modules(s).unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].refcount, 12);
        assert_eq!(m[0].deps, vec!["nvidia_uvm", "nvidia_modeset"]);
        assert_eq!(m[1].deps, Vec::<String>::new());
        assert!(occupancy(&m));
        assert!(!occupancy(&[ModuleLine {
            name: "nvidia".into(),
            size: 1,
            refcount: 2,
            deps: vec!["a".into(), "b".into()]
        }]));
        assert!(parse_proc_modules("only three fields\n").is_err());
        assert!(parse_proc_modules("nvidia x 1 - Live 0x0\n").is_err());
    }

    #[test]
    fn unload_and_reload_order() {
        let s = "nvidia 1 2 a,b Live 0x0\nnvidia_uvm 1 0 - Live 0x0\nnvidia_drm 1 0 - Live 0x0\nnvidia_modeset 1 1 a Live 0x0\n";
        let m = parse_proc_modules(s).unwrap();
        assert_eq!(
            unload_plan(&m),
            vec!["nvidia_drm", "nvidia_modeset", "nvidia_uvm", "nvidia"]
        );
        let t = reload_targets(Some(&m)).unwrap();
        assert_eq!(t.basis, ReloadBasis::LoadedAtStart);
        assert_eq!(
            t.modules,
            vec!["nvidia", "nvidia_uvm", "nvidia_modeset", "nvidia_drm"]
        );
        let t = reload_targets(Some(&parse_proc_modules("").unwrap())).unwrap();
        assert_eq!(t.basis, ReloadBasis::NoneLoadedAtStart);
        assert_eq!(t.modules, DEFAULT_RELOAD.to_vec());
        assert!(reload_targets(None).is_none());
        let t = reload_targets_from_unloaded(&[]);
        assert_eq!(t.basis, ReloadBasis::UnloadedInUpdate);
        assert_eq!(t.modules, DEFAULT_RELOAD.to_vec());
        let t = reload_targets_from_unloaded(&["nvidia_drm", "nvidia_modeset"]);
        assert_eq!(t.modules, vec!["nvidia_modeset", "nvidia_drm"]);
    }

    #[test]
    fn version_parsing_and_comparison() {
        assert!(parse_version("580.178.04").is_some());
        assert!(parse_version("565.77").is_some());
        assert!(parse_version("1").is_none());
        assert!(parse_version("1.2.3.4.5").is_none());
        assert!(parse_version("580.178.04.").is_none());
        assert!(parse_version("580.178.0x").is_none());
        assert!(parse_version("1234567890.1").is_none());
        let a = parse_version("580.95.05").unwrap();
        let b = parse_version("580.173.02").unwrap();
        assert!(a < b);
        let c = parse_version("580.95").unwrap();
        assert!(c < a);
        assert_eq!(a.text(), "580.95.05");
        assert_eq!(a.major(), 580);
    }

    #[test]
    fn nvrm_version_open_and_proprietary() {
        let open = "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  580.178.04  Release Build  (dvs-builder)  Mon Oct  1 00:00:00 UTC 2026\n";
        let v = parse_nvrm_version(open).unwrap();
        assert!(v.open);
        assert_eq!(v.version.text(), "580.178.04");
        let prop = "NVRM version: NVIDIA UNIX x86_64 Kernel Module  580.178.04  Thu Oct  1 00:00:00 UTC 2026\n";
        let v = parse_nvrm_version(prop).unwrap();
        assert!(!v.open);
        assert!(parse_nvrm_version("nothing here").is_none());
    }

    #[test]
    fn smi_versions_require_all_lines_equal() {
        assert_eq!(
            parse_smi_versions("580.178.04\n580.178.04\n"),
            Some("580.178.04".into())
        );
        assert_eq!(parse_smi_versions("580.178.04\n580.1\n"), None);
        assert_eq!(parse_smi_versions(""), None);
    }

    #[test]
    fn sysext_since() {
        let json = r#"[{"hierarchy":"/usr","extensions":["nvidia"],"since":1790000000123456}]"#;
        assert_eq!(parse_sysext_since(json).unwrap(), Some(1790000000123456));
        let json = r#"[{"hierarchy":"/usr","extensions":"none","since":null}]"#;
        assert_eq!(parse_sysext_since(json).unwrap(), None);
        // A name string is not an array member: it cannot satisfy the membership condition.
        let json = r#"[{"hierarchy":"/usr","extensions":"nvidia","since":5}]"#;
        assert!(parse_sysext_since(json).is_err());
        let json = r#"[{"hierarchy":"/usr","extensions":["nvidia","other"],"since":6}]"#;
        assert_eq!(parse_sysext_since(json).unwrap(), Some(6));
        let json = r#"[{"hierarchy":"/usr","extensions":["other"],"since":7}]"#;
        assert_eq!(parse_sysext_since(json).unwrap(), None);
        let json = r#"[{"hierarchy":"/opt","extensions":["nvidia"],"since":5}]"#;
        assert!(parse_sysext_since(json).is_err());
        assert!(parse_sysext_since("not json").is_err());
        let json = r#"[{"hierarchy":"/usr","extensions":["nvidia"],"since":null}]"#;
        assert!(parse_sysext_since(json).is_err());
        let duplicate = r#"[{"hierarchy":"/usr","extensions":["nvidia"],"since":5},{"hierarchy":"/usr","extensions":"none","since":null}]"#;
        assert!(parse_sysext_since(duplicate).is_err());
        let invalid_member = r#"[{"hierarchy":"/usr","extensions":["nvidia",null],"since":5}]"#;
        assert!(parse_sysext_since(invalid_member).is_err());
        let missing_since = r#"[{"hierarchy":"/usr","extensions":"none"}]"#;
        assert!(parse_sysext_since(missing_since).is_err());
        let empty = r#"[{"hierarchy":"/usr","extensions":[],"since":8}]"#;
        assert_eq!(
            parse_sysext_status(empty).unwrap(),
            SysextUsr::OtherExtensions
        );
        assert!(
            parse_sysext_status(r#"[{"hierarchy":"/usr","extensions":[],"since":null}]"#).is_err()
        );
        assert!(
            parse_sysext_status(r#"[{"hierarchy":"/usr","extensions":"none","since":8}]"#).is_err()
        );
        assert!(parse_sysext_status(
            r#"[{"hierarchy":"/usr","extensions":["nvidia"],"since":"8"}]"#
        )
        .is_err());
        let other_hierarchy = r#"[{"hierarchy":"/opt","extensions":"none","since":null},{"hierarchy":"/usr","extensions":["nvidia"],"since":9}]"#;
        assert_eq!(parse_sysext_since(other_hierarchy).unwrap(), Some(9));
    }

    #[test]
    fn manifest_version() {
        assert_eq!(
            parse_manifest_version(r#"{"version":"25.10.7"}"#),
            Some("25.10.7".into())
        );
        assert_eq!(parse_manifest_version(r#"{"version":""}"#), None);
        assert_eq!(parse_manifest_version("nope"), None);
    }

    #[test]
    fn mounts_and_octal_escapes() {
        let s = "/dev/sda1 /boot ext4 rw,relatime 0 0\n\
                 tank/ROOT/25.10.7/usr /usr zfs ro,relatime,xattr 0 0\n\
                 /dev/sdb1 /mnt/<pool>\\040data ext4 rw 0 0\n";
        let m = parse_mounts(s);
        assert_eq!(m.len(), 3);
        assert!(!m[1].is_rw());
        assert_eq!(m[2].mount_point, "/mnt/<pool> data");
        assert!(is_rw("rw,relatime"));
        assert!(!is_rw("ro,relatime"));
        assert!(!is_rw("rw,ro"));
    }

    #[test]
    fn mountinfo_fields() {
        let s = "36 25 0:32 / /usr rw,relatime - zfs <pool>/ROOT/env/usr rw,xattr,posixacl\n\
                 42 30 0:40 / /run/truenas-nvidia-driver-manager/usr-rw rw,relatime - zfs <pool>/ROOT/env/usr rw\n";
        let m = parse_mountinfo(s);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].root, "/");
        assert_eq!(m[0].line.fstype, "zfs");
        assert_eq!(m[0].line.mount_point, "/usr");
        assert_eq!(
            m[1].line.mount_point,
            "/run/truenas-nvidia-driver-manager/usr-rw"
        );
        assert!(m[1].line.is_rw());
    }

    #[test]
    fn unsquashfs_listing_parses_regular_symlink_and_device_lines() {
        let s = "drwxr-xr-x root/root         0 2026-09-28 10:15 squashfs-root/usr\n\
lrwxrwxrwx root/root         0 2026-09-28 10:15 squashfs-root/usr/lib64 -> lib\n\
-rw-r--r-- root/root  32539112 2026-09-28 10:15 squashfs-root/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.580.173.02\n\
crw-rw-rw- root/root  1, 3 2026-09-28 10:15 squashfs-root/dev/nvidia0\n";
        let e = parse_unsquashfs_lls(s).unwrap();
        assert_eq!(e.len(), 4);
        assert_eq!(e[1].kind, EntryKind::Symlink);
        assert_eq!(e[1].link_target.as_deref(), Some("lib"));
        assert_eq!(e[2].kind, EntryKind::File);
        assert_eq!(e[3].kind, EntryKind::Other);
        assert!(parse_unsquashfs_lls("broken line\n").is_err());
        assert!(
            parse_unsquashfs_lls("-rw-r--r-- root/root 1 2026-09-28 10:15 other/usr\n").is_err()
        );
    }

    /// A date-less line whose tail is only whitespace must end the date scan:
    /// the scan has to advance on every step, so the parse returns instead of
    /// spinning on the empty token. A stuck parse never reaches the channel.
    #[test]
    fn unsquashfs_listing_without_a_date_field_terminates() {
        for line in ["-rw-r--r-- root/root 5 \n", "-rw-r--r-- root/root   \n"] {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(parse_unsquashfs_lls(line));
            });
            let result = rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap_or_else(|_| panic!("the parse of {line:?} did not finish"));
            assert_eq!(result.unwrap_err().0, "line 1: no date field", "{line:?}");
        }
    }

    /// A date whose time field is missing is reported as a missing time, also
    /// when the line ends in whitespace; the same line with a time keeps
    /// parsing to the same entry.
    #[test]
    fn unsquashfs_listing_date_without_a_time_field_is_rejected() {
        assert_eq!(
            parse_unsquashfs_lls("-rw-r--r-- root/root 5 2026-09-28 \n")
                .unwrap_err()
                .0,
            "line 1: no time field"
        );
        assert_eq!(
            parse_unsquashfs_lls("-rw-r--r-- root/root 5 2026-09-28\n")
                .unwrap_err()
                .0,
            "line 1: no time field"
        );

        let s = "-rw-r--r-- root/root 5 2026-09-28 10:15 squashfs-root/usr/lib/x.so\n";
        let e = parse_unsquashfs_lls(s).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].kind, EntryKind::File);
        assert_eq!(e[0].path, "usr/lib/x.so");
        assert_eq!(e[0].mode, "-rw-r--r--");
    }

    #[test]
    fn stock_image_listing_parses() {
        let listing =
            include_str!("../tests/fixtures/stock-nvidia.raw-580.173.02-unsquashfs-lls.txt");
        let entries = parse_unsquashfs_lls(listing).expect("the stock listing must parse");
        assert!(
            entries.len() > 3000,
            "unexpected entry count {}",
            entries.len()
        );
        let facts = image_facts(&entries);
        assert_eq!(facts.file_version, Reading::Value("580.173.02".into()));
        assert_eq!(
            facts.target_kernel,
            Reading::Value("6.12.105-production+truenas".into())
        );
    }

    #[test]
    fn image_facts_take_unique_values() {
        let s = "drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr/lib/modules/6.12.105-production+truenas\n\
-rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.580.173.02\n\
-rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.1\n\
lrwxrwxrwx root/root 0 2026-09-28 10:15 squashfs-root/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.1 -> libnvidia-ml.so.580.173.02\n\
-rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-ctk\n\
-rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime\n\
-rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime-hook\n";
        let facts = image_facts(&parse_unsquashfs_lls(s).unwrap());
        assert_eq!(
            facts.target_kernel,
            Reading::Value("6.12.105-production+truenas".into())
        );
        assert_eq!(facts.file_version, Reading::Value("580.173.02".into()));
        for (_, r) in &facts.tools {
            assert_eq!(*r, Reading::Value(true));
        }
    }

    #[test]
    fn sysfs_hex() {
        assert_eq!(parse_sysfs_hex("0x10de\n", 4), Some(0x10de));
        assert_eq!(parse_sysfs_hex("0x030000\n", 6), Some(0x030000));
        assert_eq!(parse_sysfs_hex("0x10de", 4), Some(0x10de));
        // The width is exact and whitespace is closed.
        assert_eq!(parse_sysfs_hex("0x0300\n", 6), None);
        assert_eq!(parse_sysfs_hex("0x1", 4), None);
        assert_eq!(parse_sysfs_hex(" 0x10de\n", 4), None);
        assert_eq!(parse_sysfs_hex("0x10de \n", 4), None);
        assert_eq!(parse_sysfs_hex("0x10de\n\n", 4), None);
        assert_eq!(parse_sysfs_hex("10de\n", 4), None);
        assert_eq!(parse_sysfs_hex("0xzzzz\n", 4), None);
        assert_eq!(parse_sysfs_hex("0x12345\n", 4), None);
        assert_eq!(parse_sysfs_hex("0x10DE\n", 4), None);
    }

    #[test]
    fn pci_address_format() {
        assert!(pci_address_ok("0000:0a:00.0"));
        assert!(pci_address_ok("00000000:0a:00.0"));
        assert!(pci_address_ok("0000:0a:00.7"));
        assert!(!pci_address_ok("0000:0a:00.8"));
        assert!(!pci_address_ok("000:0a:00.0"));
        assert!(!pci_address_ok("0000:0a:00"));
        assert!(!pci_address_ok("0000:zz:00.0"));
        assert!(!pci_address_ok("nvme0n1"));
        // The regex is lowercase-only.
        assert!(!pci_address_ok("0000:0A:00.0"));
        assert!(!pci_address_ok("0000:0a:00.0".to_uppercase().as_str()));
    }

    #[test]
    fn counted_skips_vfio() {
        let gpus = vec![
            HostGpu {
                address: "0000:01:00.0".into(),
                device: 0x1b80,
                driver: Some("nvidia".into()),
            },
            HostGpu {
                address: "0000:02:00.0".into(),
                device: 0x2b85,
                driver: Some("vfio-pci".into()),
            },
            HostGpu {
                address: "0000:03:00.0".into(),
                device: 0x2684,
                driver: None,
            },
        ];
        let c = counted(&gpus);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].address, "0000:01:00.0");
        assert_eq!(c[1].address, "0000:03:00.0");
    }
}

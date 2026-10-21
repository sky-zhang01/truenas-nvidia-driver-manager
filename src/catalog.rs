//! Version index, branch table, GPU table and kernel module type.

use serde::{Deserialize, Serialize};

use crate::codes::*;
use crate::facts::{self, ModuleLine, Reading, Version};
use crate::host::{At, Cmd, Host};

/// Parse the NVIDIA version index.
pub fn parse_index(html: &str) -> Vec<Version> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let (quote, start) = if bytes[i] == b'"' {
            (b'"', i + 1)
        } else if bytes[i] == b'\'' {
            (b'\'', i + 1)
        } else {
            i += 1;
            continue;
        };
        // Only consider `href="..."` / `href='...'` occurrences. Compare bytes: the page is
        // arbitrary UTF-8, so slicing at a byte offset may not be a character boundary.
        if i < 5
            || !bytes[i - 5..i].eq_ignore_ascii_case(b"href=")
            || (i > 5 && !bytes[i - 6].is_ascii_whitespace() && bytes[i - 6] != b'<')
        {
            i += 1;
            continue;
        }
        let Some(end_rel) = bytes[start..].iter().position(|b| *b == quote) else {
            break;
        };
        let target = &html[start..start + end_rel];
        if let Some(stripped) = target.strip_suffix('/') {
            // The body must be exactly the 2–4 dot-separated digit groups; any surrounding
            // whitespace is not a version directory.
            if let Some(v) = facts::parse_version_exact(stripped) {
                out.push(v);
            }
        }
        i = start + end_rel + 1;
    }
    out
}

/// The latest version of every branch, 470 and later, newest first.
pub fn branch_latest(versions: &[Version]) -> Vec<Version> {
    let mut best: std::collections::BTreeMap<u32, Version> = std::collections::BTreeMap::new();
    for v in versions {
        if v.major() < MIN_MAJOR_BRANCH {
            continue;
        }
        best.entry(v.major())
            .and_modify(|cur| {
                if v > cur {
                    *cur = v.clone();
                }
            })
            .or_insert_with(|| v.clone());
    }
    let mut out: Vec<Version> = best.into_values().collect();
    out.sort_by(|a, b| b.cmp(a));
    out
}

/// The fixed community-patched official installer is qualified by the actual
/// kernel family, not by the TrueNAS release or the header interface probes.
pub fn community_patch_470(is_official: bool, version: &str, kernel: &str) -> bool {
    if !is_official || version != COMMUNITY_PATCH_VERSION || !facts::kernel_ok(kernel) {
        return false;
    }
    let mut fields = kernel.splitn(3, '.');
    matches!(fields.next(), Some("6"))
        && matches!(fields.next(), Some("12"))
        && fields
            .next()
            .is_some_and(|patch| patch.as_bytes().first().is_some_and(u8::is_ascii_digit))
}

/// catalog query: read the NVIDIA version index with the host's curl.
pub fn fetch(h: &dyn Host) -> Result<String, crate::host::Output> {
    let out = h.run(
        &Cmd::new(At::Host, "curl", CATALOG_CMD_TIMEOUT)
            .arg("--silent")
            .arg("--show-error")
            .arg("--fail")
            .arg("--proto")
            .arg("=https")
            .arg("--connect-timeout")
            .arg(CATALOG_CONNECT.as_secs().to_string())
            .arg("--max-time")
            .arg(CATALOG_TOTAL.as_secs().to_string())
            .arg("--output")
            .arg("-")
            .arg("--write-out")
            .arg("\n%{http_code}")
            .arg(CATALOG_URL),
    );
    if out.exit.ok() {
        Ok(out.stdout_text())
    } else {
        Err(out)
    }
}

/// Split a curl body into the index HTML and the last HTTP status line.
pub fn split_status(stdout: &str) -> (String, Option<u16>) {
    let trimmed = stdout.trim_end_matches(['\r', '\n']);
    match trimmed.rsplit_once('\n') {
        Some((body, status)) => {
            let code = status.trim().parse::<u16>().ok();
            (format!("{body}\n"), code)
        }
        None => (String::new(), trimmed.trim().parse::<u16>().ok()),
    }
}

/// catalog query failure reason. Any non-zero exit is a failure; exits 6/7 are connect
/// failures, 28 is curl's 15 s total timeout, and `Exit::TimedOut` is the host command's 20 s
/// deadline. Other exit codes are reported with their own status. The curl stderr travels
/// separately as the `raw` field of the versions item.
pub fn fetch_failure_reason(out: &crate::host::Output) -> crate::host::Reason {
    use crate::host::Exit;
    match out.exit {
        Exit::Code(6) | Exit::Code(7) => {
            crate::host::Reason::keyed(REASON_CONNECT_FAILED, serde_json::json!({}))
        }
        Exit::Code(28) => crate::host::Reason::keyed(
            REASON_TIMED_OUT,
            serde_json::json!({ "seconds": CATALOG_TOTAL.as_secs() }),
        ),
        Exit::TimedOut => crate::host::Reason::keyed(
            REASON_TIMED_OUT,
            serde_json::json!({ "seconds": CATALOG_CMD_TIMEOUT.as_secs() }),
        ),
        Exit::Code(c) => {
            crate::host::Reason::keyed(REASON_EXIT_STATUS, serde_json::json!({ "code": c }))
        }
        Exit::Signal(s) => {
            crate::host::Reason::keyed(REASON_SIGNAL, serde_json::json!({ "signal": s }))
        }
        Exit::SpawnFailed => crate::host::Reason::keyed(REASON_SPAWN_FAILED, serde_json::json!({})),
    }
}

// ---- Branch table -------------------------------------------

const PCI_RESIZE_ADAPTED_580: &str = "580.126.09";

pub type BranchSet = &'static [u32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Always,
    FollowPfnAbsent,
    OutputPollChangedAbsent,
    PciResizeResourceArgs(u32),
}

pub struct BranchRule {
    pub branches: BranchSet,
    pub when: Condition,
    pub reason: &'static str,
    pub source: &'static str,
}

pub struct ModuleRule {
    pub branches: BranchSet,
    pub module: &'static str,
    pub reason: &'static str,
    pub source: &'static str,
}

pub static BRANCHES: &[BranchRule] = &[
    BranchRule {
        branches: &[470, 495, 510, 515, 520, 525, 530, 545],
        when: Condition::FollowPfnAbsent,
        reason: BRANCH_FOLLOW_PFN_REMOVED,
        source: "NVIDIA/open-gpu-kernel-modules 470.256.02-545.29.06 kernel/nvidia/os-mlock.c; torvalds/linux v6.9 has follow_pfn(), v6.10 removed it; truenas/linux release/25.10.7 has none (2026-09-28)",
    },
    BranchRule {
        branches: &[470, 495, 510, 515, 520, 525, 530, 545, 555, 560],
        when: Condition::OutputPollChangedAbsent,
        reason: BRANCH_OUTPUT_POLL_CHANGED_REMOVED,
        source: "driver kernels set .output_poll_changed unconditionally (470.256.02-560.35.03); include/drm/drm_mode_config.h has it in v6.11, not in v6.12 or truenas release/25.10.7 (2026-09-28)",
    },
    BranchRule {
        branches: &[545, 550, 555, 560, 565, 570, 575],
        when: Condition::PciResizeResourceArgs(3),
        reason: BRANCH_PCI_RESIZE_RESOURCE_ARGS,
        source: "kernel-open/nvidia/nv-pci.c calls pci_resize_resource() with 3 arguments (545.29.06-575.64.05); this condition does not apply to proprietary modules or an unspecified installer type; include/linux/pci.h has 3 in v6.12.96 and 4 in v6.12.97/6.12.105 and truenas release/25.10.7; 580.119.02 lacks the detection; 580.126.09 adds it (2026-10-21)",
    },
    BranchRule {
        branches: &[510, 515, 520, 525, 530, 535],
        when: Condition::Always,
        reason: BRANCH_INSTALLER_LOADS_MODULE,
        source: "nvidia-installer 510.108.03-535.247.01 try to load the new module or run modprobe after installing; the zero-capability build container refuses (2026-09-28)",
    },
];

pub static MODULE_RULES: &[ModuleRule] = &[
    ModuleRule {
        branches: &[545],
        module: "nvidia",
        reason: BRANCH_NVIDIA_NOT_LOADED,
        source: "545.29.06 kernel.c:1661-1717 tries to load the new module unless nvidia is already loaded (2026-09-28)",
    },
    ModuleRule {
        branches: &[470, 495],
        module: "nvidia_drm",
        reason: BRANCH_NVIDIA_DRM_NOT_LOADED,
        source: "470.256.02 install-from-cwd.c:336-339 runs modprobe nvidia-drm after installing (2026-09-28)",
    },
];

/// Kernel interfaces used by the driver compatibility rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelInterfaces {
    pub follow_pfn: Reading<bool>,
    pub output_poll_changed: Reading<bool>,
    pub pci_resize_resource_args: Reading<Option<u32>>,
}

impl KernelInterfaces {
    pub fn all_unknown(reason: &'static str) -> KernelInterfaces {
        KernelInterfaces {
            follow_pfn: Reading::Unknown(reason),
            output_poll_changed: Reading::Unknown(reason),
            pci_resize_resource_args: Reading::Unknown(reason),
        }
    }

    pub fn complete(&self) -> bool {
        self.follow_pfn.is_value()
            && self.output_poll_changed.is_value()
            && self.pci_resize_resource_args.is_value()
    }
}

/// Header files used by the compatibility probes.
pub fn header_paths() -> [&'static str; 3] {
    [
        "/usr/src/linux-headers-truenas-production-amd64/include/linux/mm.h",
        "/usr/src/linux-headers-truenas-production-amd64/include/drm/drm_mode_config.h",
        "/usr/src/linux-headers-truenas-production-amd64/include/linux/pci.h",
    ]
}

/// Count the arguments of every `pci_resize_resource(` declaration.
pub fn parse_pci_resize_args(text: &str) -> Reading<Option<u32>> {
    let needle = b"pci_resize_resource(";
    let bytes = text.as_bytes();
    let mut best: Option<u32> = None;
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let mut depth = 0u32;
            let mut args = 1u32;
            let mut j = i + needle.len();
            let mut closed = false;
            while j < bytes.len() {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => {
                        if depth == 0 {
                            closed = true;
                            break;
                        }
                        depth -= 1;
                    }
                    b',' if depth == 0 => args += 1,
                    _ => {}
                }
                j += 1;
            }
            if !closed {
                return Reading::Unknown(REASON_FORMAT_UNRECOGNIZED);
            }
            best = Some(best.map_or(args, |b| b.max(args)));
            i = j;
        } else {
            i += 1;
        }
    }
    Reading::Value(best)
}

/// kernel header reading: read the three kernel interface readings. The three reads share one
/// `READING_TIMEOUT` deadline; a read that completes only after the deadline counts as
/// unreadable.
pub fn read_interfaces(h: &dyn Host) -> KernelInterfaces {
    let start = h.now();
    let paths = header_paths();
    let follow_pfn = read_header_bool(h, paths[0], b"follow_pfn(", start);
    let output_poll_changed = read_header_bool(h, paths[1], b"(*output_poll_changed)(", start);
    let pci = read_pci_interface(h, paths[2], start);
    KernelInterfaces {
        follow_pfn,
        output_poll_changed,
        pci_resize_resource_args: pci,
    }
}

fn header_deadline_expired(h: &dyn Host, start: std::time::Instant) -> bool {
    h.now().saturating_duration_since(start) >= READING_TIMEOUT
}

/// Read one header and answer whether it carries `needle`.
fn read_header_bool(
    h: &dyn Host,
    path: &str,
    needle: &[u8],
    start: std::time::Instant,
) -> Reading<bool> {
    if header_deadline_expired(h, start) {
        return Reading::Unknown(REASON_TIMED_OUT);
    }
    let p = h.view(std::path::Path::new(path));
    let value = match h.read(&p) {
        Ok(bytes) => Reading::Value(bytes.windows(needle.len()).any(|w| w == needle)),
        Err(e) => Reading::Unknown(crate::log::io_reason_key(&e)),
    };
    if header_deadline_expired(h, start) {
        return Reading::Unknown(REASON_TIMED_OUT);
    }
    value
}

fn read_pci_interface(h: &dyn Host, path: &str, start: std::time::Instant) -> Reading<Option<u32>> {
    if header_deadline_expired(h, start) {
        return Reading::Unknown(REASON_TIMED_OUT);
    }
    let p = h.view(std::path::Path::new(path));
    let value = match h.read(&p) {
        Ok(bytes) => parse_pci_resize_args(&String::from_utf8_lossy(&bytes)),
        Err(e) => Reading::Unknown(crate::log::io_reason_key(&e)),
    };
    if header_deadline_expired(h, start) {
        return Reading::Unknown(REASON_TIMED_OUT);
    }
    value
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchVerdict {
    Buildable,
    Unbuildable(Vec<&'static str>),
    Undecidable,
}

fn condition_verdict(c: Condition, k: &KernelInterfaces) -> Option<bool> {
    match c {
        Condition::Always => Some(true),
        Condition::FollowPfnAbsent => match k.follow_pfn {
            Reading::Value(has) => Some(!has),
            Reading::Unknown(_) => None,
        },
        Condition::OutputPollChangedAbsent => match k.output_poll_changed {
            Reading::Value(has) => Some(!has),
            Reading::Unknown(_) => None,
        },
        Condition::PciResizeResourceArgs(n) => match k.pci_resize_resource_args {
            Reading::Value(Some(m)) => Some(m != n),
            Reading::Value(None) => Some(false),
            Reading::Unknown(_) => None,
        },
    }
}

fn rule_applies(
    rule: &BranchRule,
    version: &Version,
    module_type: Option<KernelModuleType>,
) -> bool {
    let old_580 = matches!(rule.when, Condition::PciResizeResourceArgs(_))
        && version.major() == 580
        && version < &facts::parse_version_exact(PCI_RESIZE_ADAPTED_580).unwrap();
    (rule.branches.contains(&version.major()) || old_580)
        && (!matches!(rule.when, Condition::PciResizeResourceArgs(_))
            || module_type == Some(KernelModuleType::Open))
}

/// Decide the conditions applicable to the planned module type. An unspecified type leaves
/// the open-only PCI condition to the isolated build.
pub fn verdict(
    version: &Version,
    k: &KernelInterfaces,
    module_type: Option<KernelModuleType>,
) -> BranchVerdict {
    let mut reasons = Vec::new();
    let mut undecidable = false;
    for rule in BRANCHES {
        if !rule_applies(rule, version, module_type) {
            continue;
        }
        match condition_verdict(rule.when, k) {
            Some(true) => reasons.push(rule.reason),
            Some(false) => {}
            None => undecidable = true,
        }
    }
    if !reasons.is_empty() {
        BranchVerdict::Unbuildable(reasons)
    } else if undecidable {
        BranchVerdict::Undecidable
    } else {
        BranchVerdict::Buildable
    }
}

/// Official source verdict, including the one fixed 470/6.12 exception.
pub fn official_verdict(
    version: &Version,
    kernel: &str,
    k: &KernelInterfaces,
    module_type: Option<KernelModuleType>,
) -> BranchVerdict {
    if community_patch_470(true, version.text(), kernel) {
        BranchVerdict::Buildable
    } else {
        verdict(version, k, module_type)
    }
}

/// Runtime conditions: modules that are not loaded make a branch fail.
pub fn module_reasons(major: u32, loaded: &Reading<Vec<ModuleLine>>) -> Vec<&'static str> {
    let Reading::Value(modules) = loaded else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for rule in MODULE_RULES {
        if !rule.branches.contains(&major) {
            continue;
        }
        if !modules.iter().any(|m| m.name == rule.module) {
            out.push(rule.reason);
        }
    }
    out
}

pub fn official_module_reasons(
    version: &Version,
    kernel: &str,
    loaded: &Reading<Vec<ModuleLine>>,
) -> Vec<&'static str> {
    if community_patch_470(true, version.text(), kernel) {
        Vec::new()
    } else {
        module_reasons(version.major(), loaded)
    }
}

/// One unreadable header file, used in versions/plan creation `unreadable` items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderUnreadable {
    pub reason: &'static str,
    pub path: &'static str,
}

/// The three header readings could not be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadersUnreadable {
    pub items: Vec<HeaderUnreadable>,
}

impl HeadersUnreadable {
    pub fn from_interfaces(k: &KernelInterfaces) -> HeadersUnreadable {
        let paths = header_paths();
        let mut items = Vec::new();
        if let Reading::Unknown(reason) = k.follow_pfn {
            items.push(HeaderUnreadable {
                reason,
                path: paths[0],
            });
        }
        if let Reading::Unknown(reason) = k.output_poll_changed {
            items.push(HeaderUnreadable {
                reason,
                path: paths[1],
            });
        }
        if let Reading::Unknown(reason) = k.pci_resize_resource_args {
            items.push(HeaderUnreadable {
                reason,
                path: paths[2],
            });
        }
        HeadersUnreadable { items }
    }

    /// plan creation reports only unknown readings used by this branch and module type.
    pub fn for_branch(
        version: &Version,
        k: &KernelInterfaces,
        module_type: Option<KernelModuleType>,
    ) -> HeadersUnreadable {
        let paths = header_paths();
        let mut result = Self::from_interfaces(k);
        result.items.retain(|item| {
            BRANCHES.iter().any(|rule| {
                rule_applies(rule, version, module_type)
                    && match rule.when {
                        Condition::Always => false,
                        Condition::FollowPfnAbsent => item.path == paths[0],
                        Condition::OutputPollChangedAbsent => item.path == paths[1],
                        Condition::PciResizeResourceArgs(_) => item.path == paths[2],
                    }
            })
        });
        result
    }
}

/// versions candidates: versions satisfying the type-independent branch and runtime
/// conditions. Any unreadable header reading fails the whole list.
pub fn items(
    latest: &[Version],
    k: &KernelInterfaces,
    loaded: &Reading<Vec<ModuleLine>>,
    kernel: Option<&str>,
) -> Result<Vec<Version>, HeadersUnreadable> {
    if !k.complete() {
        return Err(HeadersUnreadable::from_interfaces(k));
    }
    Ok(latest
        .iter()
        .filter(|v| {
            // A kernel read failure only closes the exact 470 candidate; it
            // must not erase the other branch results.
            !(v.text() == COMMUNITY_PATCH_VERSION && kernel.is_none())
                && matches!(
                    official_verdict(v, kernel.unwrap_or_default(), k, None),
                    BranchVerdict::Buildable
                )
                && official_module_reasons(v, kernel.unwrap_or_default(), loaded).is_empty()
        })
        .cloned()
        .collect())
}

/// Catalog versions that cannot build explicit open modules on this kernel.
pub fn open_unbuildable<'a>(
    items: &'a [Version],
    kernel: &str,
    interfaces: &KernelInterfaces,
) -> Vec<&'a str> {
    items
        .iter()
        .filter(|version| {
            matches!(
                official_verdict(version, kernel, interfaces, Some(KernelModuleType::Open)),
                BranchVerdict::Unbuildable(_)
            )
        })
        .map(|version| version.text())
        .collect()
}

// ---- GPU table ---------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modules {
    ClosedOnly,
    OpenOnly,
    Either,
}

impl Modules {
    pub fn as_str(self) -> &'static str {
        match self {
            Modules::ClosedOnly => "closed_only",
            Modules::OpenOnly => "open_only",
            Modules::Either => "either",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuEntry {
    pub device: u16,
    pub name: &'static str,
    pub last_branch: Option<u32>,
    pub first: Option<&'static str>,
    pub modules: Modules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelModuleType {
    Open,
    Proprietary,
}

impl KernelModuleType {
    pub fn as_str(self) -> &'static str {
        match self {
            KernelModuleType::Open => "open",
            KernelModuleType::Proprietary => "proprietary",
        }
    }
}

/// Look one device up in a table sorted by device id.
pub fn entry(table: &[GpuEntry], device: u16) -> Option<&GpuEntry> {
    table
        .binary_search_by(|e| e.device.cmp(&device))
        .ok()
        .map(|i| &table[i])
}

/// Which kernel module kinds a GPU can use.
pub fn modules_of(table: &[GpuEntry], device: u16) -> Modules {
    if device < 0x1E00 {
        return Modules::ClosedOnly;
    }
    if let Some(e) = entry(table, device) {
        return e.modules;
    }
    if device <= 0x28FF {
        Modules::Either
    } else {
        Modules::OpenOnly
    }
}

/// The last branch supporting a GPU.
pub fn last_branch_of(table: &[GpuEntry], device: u16) -> Option<u32> {
    if let Some(e) = entry(table, device) {
        return e.last_branch;
    }
    if device < 0x1340 {
        Some(470)
    } else if device <= 0x1DFF {
        Some(580)
    } else {
        None
    }
}

/// The first version supporting a GPU.
pub fn first_of(table: &[GpuEntry], versions: &[&str], device: u16) -> Option<Version> {
    if let Some(e) = entry(table, device) {
        return e.first.and_then(facts::parse_version);
    }
    if device < 0x1E00 {
        return None;
    }
    let high = device >> 8;
    let mut best: Option<Version> = None;
    for e in table {
        if e.device >> 8 != high {
            continue;
        }
        if let Some(text) = e.first {
            if let Some(v) = facts::parse_version(text) {
                if best.as_ref().map(|b| v < *b).unwrap_or(true) {
                    best = Some(v);
                }
            }
        }
    }
    if best.is_some() {
        return best;
    }
    versions
        .iter()
        .filter_map(|t| facts::parse_version(t))
        .max()
}

/// Whether a version can drive one GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuFit {
    Supported,
    Legacy { last_branch: u32 },
    Newer { first: Version },
}

pub fn fit(table: &[GpuEntry], versions: &[&str], device: u16, v: &Version) -> GpuFit {
    if let Some(l) = last_branch_of(table, device) {
        if v.major() > l {
            return GpuFit::Legacy { last_branch: l };
        }
    }
    if let Some(f) = first_of(table, versions, device) {
        if v < &f {
            return GpuFit::Newer { first: f };
        }
    }
    GpuFit::Supported
}

/// Whether the selected GPUs mix closed-only and open-only devices.
pub fn conflict(table: &[GpuEntry], devices: &[u16]) -> bool {
    let mut closed = false;
    let mut open = false;
    for d in devices {
        match modules_of(table, *d) {
            Modules::ClosedOnly => closed = true,
            Modules::OpenOnly => open = true,
            Modules::Either => {}
        }
    }
    closed && open
}

/// Whether a version drives every selected GPU.
pub fn drives_all(table: &[GpuEntry], versions: &[&str], devices: &[u16], v: &Version) -> bool {
    if conflict(table, devices) {
        return false;
    }
    devices
        .iter()
        .all(|d| fit(table, versions, *d, v) == GpuFit::Supported)
}

/// The kernel module type passed to the installer.
pub fn module_type(table: &[GpuEntry], devices: &[u16], v: &Version) -> Option<KernelModuleType> {
    if v.major() < MODULE_TYPE_MIN_BRANCH {
        return None;
    }
    if devices.is_empty() {
        return None;
    }
    if devices
        .iter()
        .any(|d| modules_of(table, *d) == Modules::ClosedOnly)
    {
        Some(KernelModuleType::Proprietary)
    } else {
        Some(KernelModuleType::Open)
    }
}

/// A counted GPU with its table data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuListed {
    pub gpu: facts::HostGpu,
    pub entry: Option<&'static GpuEntry>,
    pub modules: Modules,
    pub last_branch: Option<u32>,
    pub first: Option<Version>,
    pub usable_from: Option<Version>,
    pub too_new: Vec<Version>,
    pub too_old: Vec<Version>,
}

/// Build the versions GPU data for the counted GPUs.
pub fn gpu_list(
    items: &[Version],
    gpus: &[facts::HostGpu],
    table: &'static [GpuEntry],
    versions: &[&str],
    open_unbuildable: Option<&[&str]>,
) -> Vec<GpuListed> {
    let mut counted: Vec<&facts::HostGpu> = facts::counted(gpus);
    counted.sort_by(|a, b| a.address.cmp(&b.address));
    counted
        .into_iter()
        .map(|gpu| {
            let modules = modules_of(table, gpu.device);
            let last_branch = last_branch_of(table, gpu.device);
            let first = first_of(table, versions, gpu.device);
            let too_new = items
                .iter()
                .filter(|v| matches!(fit(table, versions, gpu.device, v), GpuFit::Legacy { .. }))
                .cloned()
                .collect();
            let too_old = items
                .iter()
                .filter(|v| matches!(fit(table, versions, gpu.device, v), GpuFit::Newer { .. }))
                .cloned()
                .collect();
            let usable_from = open_unbuildable.and_then(|excluded| {
                items
                    .iter()
                    .filter(|v| {
                        fit(table, versions, gpu.device, v) == GpuFit::Supported
                            && (modules == Modules::ClosedOnly || !excluded.contains(&v.text()))
                    })
                    .min()
                    .cloned()
            });
            GpuListed {
                gpu: gpu.clone(),
                entry: entry(table, gpu.device),
                modules,
                last_branch,
                first,
                usable_from,
                too_new,
                too_old,
            }
        })
        .collect()
}

/// The versions `gpus` array.
pub fn gpu_readings_json(list: &[GpuListed]) -> serde_json::Value {
    let mut out = Vec::new();
    for g in list {
        let mut obj = serde_json::Map::new();
        obj.insert("address".into(), serde_json::json!(g.gpu.address));
        obj.insert(
            "device".into(),
            serde_json::json!(format!("{:04x}", g.gpu.device)),
        );
        if let Some(e) = g.entry {
            obj.insert("name".into(), serde_json::json!(e.name));
        }
        obj.insert("modules".into(), serde_json::json!(g.modules.as_str()));
        if let Some(b) = g.last_branch {
            obj.insert("last_branch".into(), serde_json::json!(b));
        }
        if let Some(f) = &g.first {
            obj.insert("first_branch".into(), serde_json::json!(f.major()));
        }
        if let Some(version) = &g.usable_from {
            obj.insert(
                "usable_from_branch".into(),
                serde_json::json!(version.major()),
            );
        }
        obj.insert(
            "too_new".into(),
            serde_json::json!(g.too_new.iter().map(|v| v.text()).collect::<Vec<_>>()),
        );
        obj.insert(
            "too_old".into(),
            serde_json::json!(g.too_old.iter().map(|v| v.text()).collect::<Vec<_>>()),
        );
        out.push(serde_json::Value::Object(obj));
    }
    serde_json::Value::Array(out)
}

/// plan creation GPU rejection payload: `{gpus, conflict}`.
pub fn gpu_unsupported_json(
    table: &'static [GpuEntry],
    versions: &[&str],
    selected: &[facts::HostGpu],
    v: &Version,
) -> serde_json::Value {
    let mut ordered: Vec<&facts::HostGpu> = selected.iter().collect();
    ordered.sort_by(|a, b| a.address.cmp(&b.address));
    let mut gpus = Vec::new();
    for gpu in ordered {
        let fit = fit(table, versions, gpu.device, v);
        let mut obj = serde_json::Map::new();
        match fit {
            GpuFit::Supported => continue,
            GpuFit::Legacy { last_branch } => {
                obj.insert("address".into(), serde_json::json!(gpu.address));
                obj.insert(
                    "device".into(),
                    serde_json::json!(format!("{:04x}", gpu.device)),
                );
                if let Some(e) = entry(table, gpu.device) {
                    obj.insert("name".into(), serde_json::json!(e.name));
                }
                obj.insert("last_branch".into(), serde_json::json!(last_branch));
            }
            GpuFit::Newer { first } => {
                obj.insert("address".into(), serde_json::json!(gpu.address));
                obj.insert(
                    "device".into(),
                    serde_json::json!(format!("{:04x}", gpu.device)),
                );
                if let Some(e) = entry(table, gpu.device) {
                    obj.insert("name".into(), serde_json::json!(e.name));
                }
                obj.insert("first_branch".into(), serde_json::json!(first.major()));
            }
        }
        gpus.push(serde_json::Value::Object(obj));
    }
    let conflict = conflict(
        table,
        &selected.iter().map(|g| g.device).collect::<Vec<_>>(),
    );
    serde_json::json!({ "gpus": gpus, "conflict": conflict })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::HostGpu;

    pub const TEST_VERSIONS: &[&str] = &[
        "470.42.01",
        "470.256.02",
        "495.46",
        "550.163.01",
        "565.77",
        "570.86.16",
        "570.190",
        "575.64.05",
        "580.178.04",
        "590.48.01",
        "595.104.02",
        "610.57.04",
        "615.71.09",
    ];

    /// Sample GPUs covering branch limits, module choices and subsystem rows.
    pub const TEST_GPUS: &[GpuEntry] = &[
        GpuEntry {
            device: 0x06C0,
            name: "GeForce GTX 480",
            last_branch: Some(390),
            first: None,
            modules: Modules::ClosedOnly,
        },
        GpuEntry {
            device: 0x0FC0,
            name: "NVIDIA GeForce GT 640",
            last_branch: Some(470),
            first: Some("470.42.01"),
            modules: Modules::ClosedOnly,
        },
        GpuEntry {
            device: 0x0FCD,
            name: "GeForce GT 755M",
            last_branch: Some(470),
            first: None,
            modules: Modules::ClosedOnly,
        },
        GpuEntry {
            device: 0x1180,
            name: "GeForce GTX 680",
            last_branch: Some(470),
            first: Some("470.42.01"),
            modules: Modules::ClosedOnly,
        },
        GpuEntry {
            device: 0x13C0,
            name: "NVIDIA GeForce GTX 980",
            last_branch: Some(580),
            first: Some("470.42.01"),
            modules: Modules::ClosedOnly,
        },
        GpuEntry {
            device: 0x1B80,
            name: "NVIDIA GeForce GTX 1080",
            last_branch: Some(580),
            first: Some("470.42.01"),
            modules: Modules::ClosedOnly,
        },
        GpuEntry {
            device: 0x1DB4,
            name: "Tesla V100-PCIE-16GB",
            last_branch: Some(580),
            first: Some("470.42.01"),
            modules: Modules::ClosedOnly,
        },
        GpuEntry {
            device: 0x1E02,
            name: "NVIDIA TITAN RTX",
            last_branch: None,
            first: Some("470.42.01"),
            modules: Modules::Either,
        },
        GpuEntry {
            device: 0x2204,
            name: "NVIDIA GeForce RTX 3090",
            last_branch: None,
            first: Some("470.42.01"),
            modules: Modules::Either,
        },
        GpuEntry {
            device: 0x2330,
            name: "NVIDIA A100",
            last_branch: None,
            first: Some("525.85.05"),
            modules: Modules::Either,
        },
        GpuEntry {
            device: 0x2684,
            name: "NVIDIA GeForce RTX 4090",
            last_branch: None,
            first: Some("520.56.06"),
            modules: Modules::Either,
        },
        GpuEntry {
            device: 0x2B85,
            name: "NVIDIA GeForce RTX 5090",
            last_branch: None,
            first: Some("570.86.16"),
            modules: Modules::OpenOnly,
        },
        GpuEntry {
            device: 0x2C09,
            name: "NVIDIA GeForce RTX 5070",
            last_branch: None,
            first: Some("615.71.09"),
            modules: Modules::OpenOnly,
        },
        GpuEntry {
            device: 0x2C33,
            name: "NVIDIA RTX PRO 4000 Blackwell SFF",
            last_branch: None,
            first: Some("570.190"),
            modules: Modules::OpenOnly,
        },
    ];

    fn gpu(address: &str, device: u16) -> HostGpu {
        HostGpu {
            address: address.into(),
            device,
            driver: Some("nvidia".into()),
        }
    }

    #[test]
    fn index_parsing_takes_version_directories() {
        let html = r#"<a href="../">..</a> <a href="latest.txt">latest</a>
            <a href='580.178.04/'>580.178.04</a>
            <a href="615.71.09/">615.71.09</a>
            <a href="1.0-4499/">old</a>
            <a href="470.256.02/">470.256.02</a>
            <a class="x" href="495.46/">495.46</a>"#;
        let v = parse_index(html);
        let texts: Vec<&str> = v.iter().map(|x| x.text()).collect();
        assert_eq!(
            texts,
            vec!["580.178.04", "615.71.09", "470.256.02", "495.46"]
        );
        let latest = branch_latest(&v);
        let texts: Vec<&str> = latest.iter().map(|x| x.text()).collect();
        assert_eq!(
            texts,
            vec!["615.71.09", "580.178.04", "495.46", "470.256.02"]
        );
    }

    #[test]
    fn index_bodies_must_be_exact_versions() {
        // The body between the quotes and the trailing slash must be exactly the 2–4
        // dot-separated digit groups; whitespace is not a version directory.
        for target in [
            " 580.178.04/",
            "580.178.04 /",
            "\t580.178.04/",
            "580.178.04\n/",
        ] {
            let html = format!("<a href=\"{target}\">bad</a>");
            assert!(
                parse_index(&html).is_empty(),
                "accepted a non-exact body {target:?}"
            );
        }
        assert_eq!(parse_index("<a href='580.178.04/'>ok</a>").len(), 1);
        assert_eq!(parse_index("<a href=\"495.46/\">ok</a>").len(), 1);
    }

    #[test]
    fn index_parsing_survives_unrelated_quotes_in_utf8() {
        // A quote that is not an href, after a multi-byte character: the parser must not
        // slice across a character boundary.
        let html = "éabcd\" <a href=\"580.178.04/\">580.178.04</a>";
        let v = parse_index(html);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].text(), "580.178.04");
        let html = "日\"x <a href='615.71.09/'>615.71.09</a>";
        assert_eq!(parse_index(html)[0].text(), "615.71.09");
    }

    #[test]
    fn fetch_failure_reason_classification() {
        use crate::host::{Exit, Output};
        let out = |exit, stdout: &[u8], stderr: &[u8]| Output {
            exit,
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
            elapsed: std::time::Duration::ZERO,
        };
        // rc22 gets exit_status with the actual code, not a hard-coded HTTP 404.
        let reason = fetch_failure_reason(&out(
            Exit::Code(22),
            b"\n503",
            b"curl: (22) The requested URL returned error: 503",
        ));
        assert_eq!(
            reason.to_json(),
            serde_json::json!({"key": REASON_EXIT_STATUS, "params": {"code": 22}})
        );
        // curl's own total timeout and the host command deadline have
        // distinct limits; each reason carries the limit that expired.
        for (exit, seconds) in [
            (Exit::Code(28), CATALOG_TOTAL.as_secs()),
            (Exit::TimedOut, CATALOG_CMD_TIMEOUT.as_secs()),
        ] {
            let reason = fetch_failure_reason(&out(exit, b"", b"curl timed out"));
            assert_eq!(
                reason.to_json(),
                serde_json::json!({
                    "key": REASON_TIMED_OUT,
                    "params": {"seconds": seconds},
                })
            );
        }
        assert_eq!(
            fetch_failure_reason(&out(Exit::Code(7), b"", b"")).to_json(),
            serde_json::json!({"key": REASON_CONNECT_FAILED, "params": {}})
        );
    }

    #[test]
    fn kernel_interface_readings_share_one_deadline() {
        struct SlowHeaders {
            seconds: std::sync::atomic::AtomicU64,
            base: std::time::Instant,
        }
        impl Host for SlowHeaders {
            fn run(&self, _: &Cmd) -> crate::host::Output {
                unreachable!()
            }
            fn run_watch(
                &self,
                _: &Cmd,
                _: std::time::Duration,
                _: &mut dyn crate::host::Watch,
            ) -> crate::host::Output {
                unreachable!()
            }
            fn view(&self, p: &std::path::Path) -> std::path::PathBuf {
                p.to_path_buf()
            }
            fn mount_table(&self) -> std::io::Result<Vec<facts::MountLine>> {
                unreachable!()
            }
            fn read(&self, _: &std::path::Path) -> std::io::Result<Vec<u8>> {
                self.seconds
                    .fetch_add(6, std::sync::atomic::Ordering::SeqCst);
                Ok(Vec::new())
            }
            fn open(&self, _: &std::path::Path) -> std::io::Result<Box<dyn std::io::Read + Send>> {
                unreachable!()
            }
            fn create(
                &self,
                _: &std::path::Path,
                _: crate::host::Create,
                _: u32,
            ) -> std::io::Result<Box<dyn crate::host::SyncWrite + Send>> {
                unreachable!()
            }
            fn append(
                &self,
                _: &std::path::Path,
                _: u32,
            ) -> std::io::Result<Box<dyn std::io::Write + Send>> {
                unreachable!()
            }
            fn lstat(&self, _: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
                unreachable!()
            }
            fn read_link(&self, _: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
                unreachable!()
            }
            fn read_dir(&self, _: &std::path::Path) -> std::io::Result<Vec<std::ffi::OsString>> {
                unreachable!()
            }
            fn create_dir(&self, _: &std::path::Path, _: u32) -> std::io::Result<()> {
                unreachable!()
            }
            fn rename(&self, _: &std::path::Path, _: &std::path::Path) -> std::io::Result<()> {
                unreachable!()
            }
            fn remove_file(&self, _: &std::path::Path) -> std::io::Result<()> {
                unreachable!()
            }
            fn remove_dir_all(&self, _: &std::path::Path) -> std::io::Result<()> {
                unreachable!()
            }
            fn fsync_dir(&self, _: &std::path::Path) -> std::io::Result<()> {
                unreachable!()
            }
            fn fsync_file(&self, _: &std::path::Path) -> std::io::Result<()> {
                unreachable!()
            }
            fn uname_release(&self) -> std::io::Result<String> {
                unreachable!()
            }
            fn now(&self) -> std::time::Instant {
                self.base
                    + std::time::Duration::from_secs(
                        self.seconds.load(std::sync::atomic::Ordering::SeqCst),
                    )
            }
            fn wall_ms(&self) -> u64 {
                unreachable!()
            }
        }
        let h = SlowHeaders {
            seconds: std::sync::atomic::AtomicU64::new(0),
            base: std::time::Instant::now(),
        };
        let k = read_interfaces(&h);
        assert_eq!(
            h.now().duration_since(h.base),
            std::time::Duration::from_secs(18)
        );
        assert!(!k.complete());
        match k.pci_resize_resource_args {
            Reading::Unknown(reason) => assert_eq!(reason, REASON_TIMED_OUT),
            other => panic!("expected a timed-out reading, got {other:?}"),
        }
        // The other two readings finished before the deadline.
        assert!(k.follow_pfn.is_value());
        assert!(k.output_poll_changed.is_value());
    }

    #[test]
    fn status_split() {
        let (body, status) = split_status("index\n200");
        assert_eq!(body, "index\n");
        assert_eq!(status, Some(200));
        let (_, status) = split_status("000");
        assert_eq!(status, Some(0));
    }

    #[test]
    fn official_580_pci_adaptation_boundary() {
        for text in ["580.119.02", "580.126.09", "580.178.04"] {
            let version = facts::parse_version_exact(text).unwrap();
            for pci in [
                Reading::Value(Some(3)),
                Reading::Value(Some(4)),
                Reading::Value(None),
                Reading::Unknown(REASON_FILE_MISSING),
            ] {
                let k = KernelInterfaces {
                    follow_pfn: Reading::Unknown(REASON_FILE_MISSING),
                    output_poll_changed: Reading::Unknown(REASON_FILE_MISSING),
                    pci_resize_resource_args: pci.clone(),
                };
                for module_type in [
                    None,
                    Some(KernelModuleType::Proprietary),
                    Some(KernelModuleType::Open),
                ] {
                    let expected =
                        if text == "580.119.02" && module_type == Some(KernelModuleType::Open) {
                            match pci {
                                Reading::Value(Some(4)) => BranchVerdict::Unbuildable(vec![
                                    BRANCH_PCI_RESIZE_RESOURCE_ARGS,
                                ]),
                                Reading::Unknown(_) => BranchVerdict::Undecidable,
                                _ => BranchVerdict::Buildable,
                            }
                        } else {
                            BranchVerdict::Buildable
                        };
                    assert_eq!(
                        official_verdict(&version, "6.12.105", &k, module_type),
                        expected,
                        "{text}/{pci:?}/{module_type:?}"
                    );
                    let unreadable = HeadersUnreadable::for_branch(&version, &k, module_type);
                    let needs_pci = text == "580.119.02"
                        && module_type == Some(KernelModuleType::Open)
                        && matches!(pci, Reading::Unknown(_));
                    assert_eq!(unreadable.items.len(), usize::from(needs_pci));
                    if needs_pci {
                        assert_eq!(unreadable.items[0].path, header_paths()[2]);
                    }
                }
            }
        }
    }

    #[test]
    fn kernel_interface_readings_on_kernel_612_105() {
        let k = KernelInterfaces {
            follow_pfn: Reading::Value(false),
            output_poll_changed: Reading::Value(false),
            pci_resize_resource_args: Reading::Value(Some(4)),
        };
        for text in [
            "615.71.09",
            "610.57.04",
            "595.104.02",
            "590.48.01",
            "580.178.04",
        ] {
            assert_eq!(
                official_verdict(
                    &facts::parse_version_exact(text).unwrap(),
                    "6.12.105",
                    &k,
                    Some(KernelModuleType::Open)
                ),
                BranchVerdict::Buildable,
                "{text}"
            );
        }
        assert_eq!(
            verdict(
                &facts::parse_version_exact("575.0").unwrap(),
                &k,
                Some(KernelModuleType::Open)
            ),
            BranchVerdict::Unbuildable(vec![BRANCH_PCI_RESIZE_RESOURCE_ARGS])
        );
        assert_eq!(
            verdict(
                &facts::parse_version_exact("560.0").unwrap(),
                &k,
                Some(KernelModuleType::Open)
            ),
            BranchVerdict::Unbuildable(vec![
                BRANCH_OUTPUT_POLL_CHANGED_REMOVED,
                BRANCH_PCI_RESIZE_RESOURCE_ARGS
            ])
        );
        assert_eq!(
            verdict(
                &facts::parse_version_exact("470.0").unwrap(),
                &k,
                Some(KernelModuleType::Open)
            ),
            BranchVerdict::Unbuildable(vec![
                BRANCH_FOLLOW_PFN_REMOVED,
                BRANCH_OUTPUT_POLL_CHANGED_REMOVED
            ])
        );
        assert_eq!(
            verdict(
                &facts::parse_version_exact("535.0").unwrap(),
                &k,
                Some(KernelModuleType::Open)
            ),
            BranchVerdict::Unbuildable(vec![BRANCH_INSTALLER_LOADS_MODULE])
        );
    }

    #[test]
    fn kernel_interface_readings_on_kernel_69() {
        let k = KernelInterfaces {
            follow_pfn: Reading::Value(true),
            output_poll_changed: Reading::Value(true),
            pci_resize_resource_args: Reading::Value(Some(3)),
        };
        for branch in [470, 495, 545, 550, 615] {
            assert_eq!(
                verdict(
                    &facts::parse_version_exact(&format!("{}.0", branch)).unwrap(),
                    &k,
                    Some(KernelModuleType::Open)
                ),
                BranchVerdict::Buildable,
                "{branch}"
            );
        }
    }

    #[test]
    fn pci_condition_uses_only_explicit_open_and_its_actual_reading() {
        for pci in [
            Reading::Value(Some(3)),
            Reading::Value(Some(4)),
            Reading::Value(None),
            Reading::Unknown(REASON_FILE_MISSING),
        ] {
            let k = KernelInterfaces {
                follow_pfn: Reading::Value(true),
                output_poll_changed: Reading::Value(true),
                pci_resize_resource_args: pci.clone(),
            };
            for major in [545, 550, 555, 560, 565, 570, 575] {
                for module_type in [None, Some(KernelModuleType::Proprietary)] {
                    assert_eq!(
                        verdict(
                            &facts::parse_version_exact(&format!("{}.0", major)).unwrap(),
                            &k,
                            module_type
                        ),
                        BranchVerdict::Buildable
                    );
                }
                let expected = match pci {
                    Reading::Value(Some(4)) => {
                        BranchVerdict::Unbuildable(vec![BRANCH_PCI_RESIZE_RESOURCE_ARGS])
                    }
                    Reading::Unknown(_) => BranchVerdict::Undecidable,
                    _ => BranchVerdict::Buildable,
                };
                assert_eq!(
                    verdict(
                        &facts::parse_version_exact(&format!("{}.0", major)).unwrap(),
                        &k,
                        Some(KernelModuleType::Open)
                    ),
                    expected
                );
            }
        }
    }

    #[test]
    fn branch_unreadable_lists_only_the_applicable_interfaces() {
        let k = KernelInterfaces::all_unknown(REASON_FILE_MISSING);
        let open = HeadersUnreadable::for_branch(
            &facts::parse_version_exact("570.0").unwrap(),
            &k,
            Some(KernelModuleType::Open),
        );
        assert_eq!(open.items.len(), 1);
        assert_eq!(open.items[0].path, header_paths()[2]);
        assert!(HeadersUnreadable::for_branch(
            &facts::parse_version_exact("570.0").unwrap(),
            &k,
            None
        )
        .items
        .is_empty());
        assert!(HeadersUnreadable::for_branch(
            &facts::parse_version_exact("570.0").unwrap(),
            &k,
            Some(KernelModuleType::Proprietary)
        )
        .items
        .is_empty());
        let generic =
            HeadersUnreadable::for_branch(&facts::parse_version_exact("545.0").unwrap(), &k, None);
        assert_eq!(
            generic.items.iter().map(|i| i.path).collect::<Vec<_>>(),
            header_paths()[..2]
        );
        assert_eq!(HeadersUnreadable::from_interfaces(&k).items.len(), 3);
    }

    #[test]
    fn unreadable_header_makes_a_branch_undecidable() {
        let k = KernelInterfaces {
            follow_pfn: Reading::Unknown(REASON_FILE_MISSING),
            output_poll_changed: Reading::Unknown(REASON_FILE_MISSING),
            pci_resize_resource_args: Reading::Value(Some(4)),
        };
        assert_eq!(
            verdict(
                &facts::parse_version_exact("470.0").unwrap(),
                &k,
                Some(KernelModuleType::Open)
            ),
            BranchVerdict::Undecidable
        );
        assert_eq!(
            verdict(
                &facts::parse_version_exact("580.178.04").unwrap(),
                &k,
                Some(KernelModuleType::Open)
            ),
            BranchVerdict::Buildable
        );
        let err = items(
            &[],
            &k,
            &Reading::Value(vec![]),
            Some("6.12.105-production+truenas"),
        )
        .unwrap_err();
        assert_eq!(err.items.len(), 2);
        assert_eq!(err.items[0].reason, REASON_FILE_MISSING);
    }

    #[test]
    fn runtime_conditions_remove_branches() {
        let loaded =
            facts::parse_proc_modules("nvidia 1 2 a Live 0x0\nnvidia_drm 1 0 - Live 0x0\n")
                .unwrap();
        let reading = Reading::Value(loaded);
        assert!(module_reasons(470, &reading).is_empty());
        assert!(module_reasons(545, &reading).is_empty());
        let loaded = facts::parse_proc_modules("nvidia 1 0 - Live 0x0\n").unwrap();
        let reading = Reading::Value(loaded);
        assert_eq!(
            module_reasons(470, &reading),
            vec![BRANCH_NVIDIA_DRM_NOT_LOADED]
        );
        assert!(module_reasons(545, &reading).is_empty());
        let none = Reading::Unknown(REASON_IO_ERROR);
        assert!(module_reasons(545, &none).is_empty());
    }

    #[test]
    fn fixed_official_470_uses_only_exact_version_and_numeric_kernel_family() {
        for kernel in ["6.12.105-production+truenas", "6.12.96-production+truenas"] {
            assert!(community_patch_470(true, COMMUNITY_PATCH_VERSION, kernel));
        }
        for (official, version, kernel) in [
            (
                false,
                COMMUNITY_PATCH_VERSION,
                "6.12.105-production+truenas",
            ),
            (true, "470.256.03", "6.12.105-production+truenas"),
            (true, COMMUNITY_PATCH_VERSION, "6.13.1-production+truenas"),
            (true, COMMUNITY_PATCH_VERSION, "6.120.1-production+truenas"),
            (true, COMMUNITY_PATCH_VERSION, "6.12evil.1"),
            (true, COMMUNITY_PATCH_VERSION, ""),
        ] {
            assert!(!community_patch_470(official, version, kernel));
        }
        let k = KernelInterfaces {
            follow_pfn: Reading::Value(false),
            output_poll_changed: Reading::Value(false),
            pci_resize_resource_args: Reading::Value(Some(4)),
        };
        let version = facts::parse_version(COMMUNITY_PATCH_VERSION).unwrap();
        let unloaded = Reading::Value(vec![]);
        assert_eq!(
            official_verdict(&version, "6.12.105-production+truenas", &k, None),
            BranchVerdict::Buildable
        );
        assert!(
            official_module_reasons(&version, "6.12.105-production+truenas", &unloaded).is_empty()
        );
        assert_eq!(
            items(
                std::slice::from_ref(&version),
                &k,
                &unloaded,
                Some("6.12.105-production+truenas"),
            )
            .unwrap(),
            vec![version.clone()]
        );
        assert!(items(std::slice::from_ref(&version), &k, &unloaded, None)
            .unwrap()
            .is_empty());
        assert!(
            items(&[version], &k, &unloaded, Some("6.13.1-production+truenas"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn scene_table_rows() {
        let v = |s: &str| facts::parse_version(s).unwrap();
        // 1b80 (GTX 1080) on 590.48.01 lies about the branch.
        assert_eq!(
            fit(TEST_GPUS, TEST_VERSIONS, 0x1B80, &v("590.48.01")),
            GpuFit::Legacy { last_branch: 580 }
        );
        assert_eq!(
            fit(TEST_GPUS, TEST_VERSIONS, 0x1B80, &v("580.178.04")),
            GpuFit::Supported
        );
        // Table entries cover both branch limits and module restrictions.
        assert_eq!(
            fit(TEST_GPUS, TEST_VERSIONS, 0x101F, &v("580.178.04")),
            GpuFit::Legacy { last_branch: 470 }
        );
        assert_eq!(
            fit(TEST_GPUS, TEST_VERSIONS, 0x1B07, &v("590.48.01")),
            GpuFit::Legacy { last_branch: 580 }
        );
        assert_eq!(
            fit(TEST_GPUS, TEST_VERSIONS, 0x1E37, &v("550.163.01")),
            GpuFit::Supported
        );
        // 2bb2 does not exist in this table: first version from high byte 2B.
        assert_eq!(
            fit(TEST_GPUS, TEST_VERSIONS, 0x2BB2, &v("565.77")),
            GpuFit::Newer {
                first: v("570.86.16")
            }
        );
        // 3001: no entry with high byte 30, so the newest table version.
        assert_eq!(
            fit(TEST_GPUS, TEST_VERSIONS, 0x3001, &v("610.57.04")),
            GpuFit::Newer {
                first: v("615.71.09")
            }
        );
        assert_eq!(
            fit(TEST_GPUS, TEST_VERSIONS, 0x3001, &v("615.71.09")),
            GpuFit::Supported
        );
        // Boundary samples.
        assert_eq!(modules_of(TEST_GPUS, 0x133F), Modules::ClosedOnly);
        assert_eq!(last_branch_of(TEST_GPUS, 0x133F), Some(470));
        assert_eq!(first_of(TEST_GPUS, TEST_VERSIONS, 0x133F), None);
        assert_eq!(modules_of(TEST_GPUS, 0x1340), Modules::ClosedOnly);
        assert_eq!(last_branch_of(TEST_GPUS, 0x1340), Some(580));
        assert_eq!(first_of(TEST_GPUS, TEST_VERSIONS, 0x1340), None);
        assert_eq!(modules_of(TEST_GPUS, 0x1E00), Modules::Either);
        assert_eq!(last_branch_of(TEST_GPUS, 0x1E00), None);
        assert_eq!(
            first_of(TEST_GPUS, TEST_VERSIONS, 0x1E00).unwrap().text(),
            "470.42.01"
        );
        assert_eq!(modules_of(TEST_GPUS, 0x28FF), Modules::Either);
        assert_eq!(
            first_of(TEST_GPUS, TEST_VERSIONS, 0x28FF).unwrap().text(),
            "615.71.09"
        );
        assert_eq!(modules_of(TEST_GPUS, 0x2900), Modules::OpenOnly);
        assert_eq!(
            first_of(TEST_GPUS, TEST_VERSIONS, 0x2900).unwrap().text(),
            "615.71.09"
        );
    }

    #[test]
    fn drives_all_and_conflict() {
        let v = |s: &str| facts::parse_version(s).unwrap();
        assert!(drives_all(
            TEST_GPUS,
            TEST_VERSIONS,
            &[0x1B07, 0x2684],
            &v("580.178.04")
        ));
        assert!(!drives_all(
            TEST_GPUS,
            TEST_VERSIONS,
            &[0x1B07, 0x2684],
            &v("590.48.01")
        ));
        assert!(!drives_all(
            TEST_GPUS,
            TEST_VERSIONS,
            &[0x101F],
            &v("580.178.04")
        ));
        assert!(conflict(TEST_GPUS, &[0x1B07, 0x2B85]));
        assert!(conflict(TEST_GPUS, &[0x1B80, 0x2C2C]));
        assert!(!conflict(TEST_GPUS, &[0x1E37, 0x1B80]));
        assert!(!drives_all(
            TEST_GPUS,
            TEST_VERSIONS,
            &[0x1B80, 0x2B85],
            &v("580.178.04")
        ));
        assert!(!drives_all(
            TEST_GPUS,
            TEST_VERSIONS,
            &[0x1B80, 0x2C2C],
            &v("580.178.04")
        ));
        assert!(drives_all(
            TEST_GPUS,
            TEST_VERSIONS,
            &[0x2B85],
            &v("580.178.04")
        ));
    }

    #[test]
    fn module_type_order() {
        let v = |s: &str| facts::parse_version(s).unwrap();
        assert_eq!(
            module_type(TEST_GPUS, &[0x1E37, 0x2B85], &v("580.178.04")),
            Some(KernelModuleType::Open)
        );
        assert_eq!(
            module_type(TEST_GPUS, &[0x1E37], &v("580.178.04")),
            Some(KernelModuleType::Open)
        );
        assert_eq!(
            module_type(TEST_GPUS, &[0x2C2C], &v("580.178.04")),
            Some(KernelModuleType::Open)
        );
        assert_eq!(
            module_type(TEST_GPUS, &[0x1E37, 0x1B80], &v("580.178.04")),
            Some(KernelModuleType::Proprietary)
        );
        assert_eq!(
            module_type(TEST_GPUS, &[0x1B07, 0x2684], &v("580.178.04")),
            Some(KernelModuleType::Proprietary)
        );
        assert_eq!(module_type(TEST_GPUS, &[], &v("580.178.04")), None);
        assert_eq!(module_type(TEST_GPUS, &[0x2B85], &v("550.163.01")), None);
    }

    #[test]
    fn usable_gpu_versions_follow_module_kind_and_complete_readings() {
        const TABLE: &[GpuEntry] = &[
            GpuEntry {
                device: 0x1b80,
                name: "Closed GPU",
                first: Some("565.77"),
                last_branch: Some(580),
                modules: Modules::ClosedOnly,
            },
            GpuEntry {
                device: 0x2684,
                name: "Either GPU",
                first: Some("570.01"),
                last_branch: None,
                modules: Modules::Either,
            },
            GpuEntry {
                device: 0x2b85,
                name: "Open GPU",
                first: Some("570.01"),
                last_branch: None,
                modules: Modules::OpenOnly,
            },
        ];
        let items: Vec<_> = [
            "590.48.01",
            "580.178.04",
            "580.119.02",
            "570.211.01",
            "565.77",
        ]
        .iter()
        .map(|s| facts::parse_version_exact(s).unwrap())
        .collect();
        let gpus = [
            gpu("0000:01:00.0", 0x1b80),
            gpu("0000:02:00.0", 0x2684),
            gpu("0000:03:00.0", 0x2b85),
        ];
        let interfaces = KernelInterfaces {
            follow_pfn: Reading::Value(false),
            output_poll_changed: Reading::Value(false),
            pci_resize_resource_args: Reading::Value(Some(4)),
        };
        let excluded = open_unbuildable(&items, "6.12.105", &interfaces);
        assert_eq!(excluded, ["580.119.02", "570.211.01", "565.77"]);
        let list = gpu_list(&items, &gpus, TABLE, &[], Some(&excluded));
        assert_eq!(
            list.iter()
                .map(|g| g.usable_from.as_ref().map(Version::text))
                .collect::<Vec<_>>(),
            [Some("565.77"), Some("580.178.04"), Some("580.178.04")]
        );
        let json = gpu_readings_json(&list);
        assert_eq!(json[0]["usable_from_branch"], 565);
        assert_eq!(json[1]["usable_from_branch"], 580);
        assert_eq!(json[2]["usable_from_branch"], 580);
        for candidates in [&items[..0], &items[..1]] {
            let list = gpu_list(candidates, &gpus[..1], TABLE, &[], Some(&[]));
            assert!(list[0].usable_from.is_none());
        }
        let unreadable = gpu_list(&items, &gpus, TABLE, &[], None);
        assert!(unreadable.iter().all(|g| g.usable_from.is_none()));
        assert!(gpu_readings_json(&unreadable)
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g.get("usable_from_branch").is_none()));
    }

    #[test]
    fn gpu_list_column_ordering_and_notes() {
        let items = branch_latest(
            &TEST_VERSIONS
                .iter()
                .map(|s| facts::parse_version(s).unwrap())
                .collect::<Vec<_>>(),
        );
        let gpus = vec![gpu("0000:02:00.0", 0x2B85), gpu("0000:01:00.0", 0x1B80)];
        let list = gpu_list(&items, &gpus, TEST_GPUS, TEST_VERSIONS, Some(&[]));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].gpu.address, "0000:01:00.0");
        assert_eq!(list[0].modules, Modules::ClosedOnly);
        assert_eq!(list[0].last_branch, Some(580));
        assert_eq!(
            list[0].too_new.iter().map(|v| v.text()).collect::<Vec<_>>(),
            vec!["615.71.09", "610.57.04", "595.104.02", "590.48.01"]
        );
        assert!(list[0].too_old.is_empty());
        assert_eq!(list[1].modules, Modules::OpenOnly);
        let json = gpu_readings_json(&list);
        assert_eq!(json[0]["device"], "1b80");
        assert_eq!(json[0]["name"], "NVIDIA GeForce GTX 1080");
        assert_eq!(json[1]["device"], "2b85");
        assert!(json[1].get("last_branch").is_none());
        assert_eq!(json[1]["first_branch"], 570);
    }
}

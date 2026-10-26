//! Version index, branch table, GPU table and kernel module type.

use serde::{Deserialize, Serialize};
use serde_json::json;

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
        && fields
            .next()
            .is_some_and(|minor| COMMUNITY_PATCH_KERNEL_SERIES.contains(&minor))
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

/// catalog query with the failure classification. The curl stderr is the `raw` field of the
/// versions item, whatever the reason kind.
pub fn read_versions(h: &dyn Host) -> Result<Vec<Version>, serde_json::Value> {
    let out = match fetch(h) {
        Ok(stdout) => stdout,
        Err(out) => {
            let reason = fetch_failure_reason(&out);
            let (key, params) = match &reason {
                crate::host::Reason::Keyed { key, params } => ((*key), params.clone()),
                crate::host::Reason::Stderr(_) => match out.exit {
                    crate::host::Exit::Code(code) => (REASON_EXIT_STATUS, json!({"code": code})),
                    crate::host::Exit::Signal(signal) => (REASON_SIGNAL, json!({"signal": signal})),
                    crate::host::Exit::TimedOut => (
                        REASON_TIMED_OUT,
                        json!({"seconds": CATALOG_CMD_TIMEOUT.as_secs()}),
                    ),
                    crate::host::Exit::SpawnFailed => (REASON_SPAWN_FAILED, json!({})),
                },
            };
            let raw = out.stderr_text();
            // the log's reason is the real `{key, params}` object and the curl stderr is the
            // external raw of that same object.
            let log_reason = crate::log::reason_object(key, params.clone(), Some(&raw));
            crate::log::error_json(Event::CatalogQueryFailed, &[("reason", log_reason)]);
            return Err(catalog_item(json!({"key": key, "params": params}), &raw));
        }
    };
    let (body, status) = split_status(&out);
    if status != Some(200) {
        let reason = json!({"key": REASON_HTTP_STATUS, "params": {"status": status}});
        crate::log::error_json(Event::CatalogQueryFailed, &[("reason", reason.clone())]);
        return Err(catalog_item(reason, ""));
    }
    let versions = parse_index(&body);
    if versions.is_empty() {
        let reason = json!({"key": REASON_INDEX_UNRECOGNIZED, "params": {}});
        crate::log::error_json(Event::CatalogQueryFailed, &[("reason", reason.clone())]);
        return Err(catalog_item(reason, ""));
    }
    // A successful query writes no failure event.
    Ok(versions)
}

/// One versions `unreadable` item; `raw` is written only when there is text.
pub(crate) fn catalog_item(reason: serde_json::Value, raw: &str) -> serde_json::Value {
    let mut item = json!({"object": "catalog", "reason": reason});
    if !raw.is_empty() {
        item["raw"] = json!(raw);
    }
    item
}

// ---- Branch table -------------------------------------------

pub type BranchSet = &'static [u32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    Absent,
    Present,
    Args(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    FollowPfn,
    OutputPollChanged,
    PciResizeResource,
    ExtraCflags,
    FbCreateFormatInfo,
}

impl Probe {
    const ALL: [Probe; 5] = [
        Self::FollowPfn,
        Self::OutputPollChanged,
        Self::PciResizeResource,
        Self::ExtraCflags,
        Self::FbCreateFormatInfo,
    ];

    fn header(self) -> &'static str {
        match self {
            Self::FollowPfn => "/usr/src/linux-headers-truenas-production-amd64/include/linux/mm.h",
            Self::OutputPollChanged | Self::FbCreateFormatInfo => {
                "/usr/src/linux-headers-truenas-production-amd64/include/drm/drm_mode_config.h"
            }
            Self::PciResizeResource => {
                "/usr/src/linux-headers-truenas-production-amd64/include/linux/pci.h"
            }
            Self::ExtraCflags => {
                "/usr/src/linux-headers-truenas-production-amd64/scripts/Makefile.lib"
            }
        }
    }

    fn parse(self, text: &str) -> Reading<Seen> {
        match self {
            Self::FollowPfn => presence(text.contains("follow_pfn(")),
            Self::OutputPollChanged => presence(text.contains("(*output_poll_changed)(")),
            Self::PciResizeResource => parse_pci_resize_args(text),
            Self::ExtraCflags => presence(supports_extra_cflags(text)),
            Self::FbCreateFormatInfo => parse_fb_create_format_info(text),
        }
    }
}

fn presence(present: bool) -> Reading<Seen> {
    Reading::Value(if present { Seen::Present } else { Seen::Absent })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fires {
    WhenAbsent,
    WhenPresent,
    WhenArgsOtherThan(u32),
}

impl Fires {
    fn matches(self, seen: Seen) -> bool {
        match (self, seen) {
            (Self::WhenAbsent, Seen::Absent) | (Self::WhenPresent, Seen::Present) => true,
            (Self::WhenArgsOtherThan(expected), Seen::Args(actual)) => expected != actual,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Always,
    Reads(Probe, Fires),
}

pub struct BranchRule {
    pub branches: BranchSet,
    pub below: &'static [&'static str],
    pub open_only: bool,
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
        below: &[],
        open_only: false,
        when: Condition::Reads(Probe::FollowPfn, Fires::WhenAbsent),
        reason: BRANCH_FOLLOW_PFN_REMOVED,
        source: "NVIDIA/open-gpu-kernel-modules 470.256.02-545.29.06 kernel/nvidia/os-mlock.c; torvalds/linux v6.9 has follow_pfn(), v6.10 removed it; truenas/linux release/25.10.7 has none (2026-09-28)",
    },
    BranchRule {
        branches: &[470, 495, 510, 515, 520, 525, 530, 545, 555, 560],
        below: &[],
        open_only: false,
        when: Condition::Reads(Probe::OutputPollChanged, Fires::WhenAbsent),
        reason: BRANCH_OUTPUT_POLL_CHANGED_REMOVED,
        source: "driver kernels set .output_poll_changed unconditionally (470.256.02-560.35.03); include/drm/drm_mode_config.h has it in v6.11, not in v6.12 or truenas release/25.10.7 (2026-09-28)",
    },
    BranchRule {
        branches: &[545, 550, 555, 560, 565, 570, 575],
        below: &["580.126.09"],
        open_only: true,
        when: Condition::Reads(Probe::PciResizeResource, Fires::WhenArgsOtherThan(3)),
        reason: BRANCH_PCI_RESIZE_RESOURCE_ARGS,
        source: "kernel-open/nvidia/nv-pci.c calls pci_resize_resource() with 3 arguments (545.29.06-575.64.05); this condition does not apply to proprietary modules or an unspecified installer type; include/linux/pci.h has 3 in v6.12.96 and 4 in v6.12.97/6.12.105 and truenas release/25.10.7; 580.119.02 lacks the detection; 580.126.09 adds it (2026-10-21)",
    },
    BranchRule {
        branches: &[510, 515, 520, 525, 530, 535],
        below: &[],
        open_only: false,
        when: Condition::Always,
        reason: BRANCH_INSTALLER_LOADS_MODULE,
        source: "nvidia-installer 510.108.03-535.247.01 try to load the new module or run modprobe after installing; the zero-capability build container refuses (2026-09-28)",
    },
    BranchRule {
        branches: &[
            470, 495, 510, 515, 520, 525, 530, 535, 545, 550, 555, 560, 565,
        ],
        below: &["570.153.02", "575.57.08"],
        open_only: false,
        when: Condition::Reads(Probe::ExtraCflags, Fires::WhenAbsent),
        reason: BRANCH_EXTRA_CFLAGS_REMOVED,
        source: "Linux v6.15 commit e966ad0edd00 (Masahiro Yamada, 2025-02-06) removed both scripts/Makefile.build initialization and scripts/Makefile.lib ccflags-y += $(EXTRA_CFLAGS); truenas/linux release/25.10.7 has the effective append in Makefile.lib:4, release/27.0.0-RC.1 has none; official kernel and kernel-open Kbuild use EXTRA_CFLAGS in 570.124.04, 570.133.07, 570.144 and 575.51.02, and ccflags-y in 570.153.02, 575.57.08 and 580.65.06; Debian tesla-470 470.256.02-10 patch 0065 backports the change; unpatched 470.256.02, 550.163.01 and 565.77 fail with 6.18.52 headers (2026-10-24)",
    },
    BranchRule {
        branches: &[575],
        below: &[],
        open_only: false,
        when: Condition::Reads(Probe::FbCreateFormatInfo, Fires::WhenPresent),
        reason: BRANCH_FB_CREATE_FORMAT_INFO,
        source: "Known-version rule for 575: Linux v6.17 added a drm_format_info argument to fb_create (joanbm nvidia-470xx-fix-linux-6.17.patch and Debian tesla-470 patch 0074); truenas/linux release/27.0.0-RC.1 include/drm/drm_mode_config.h:83-86 has it, release/25.10.7 does not; 575.64.05 production entry first fails Building kernel modules, then the installer's nvidia-uvm-only rebuild reports unresolved nvUvmInterface* (main-build details were not retained); direct make with the same 6.18.52 headers reports nvidia-drm errors in fb_create, drm_helper_mode_fill_fb_struct and pfn_to_pfn_t; 570.211.01 proprietary and 580.196.07 build (2026-10-24)",
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

/// Fixed probe readings, indexed only through the typed Probe enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelInterfaces([Reading<Seen>; 5]);

impl KernelInterfaces {
    fn from_probes(read: impl FnMut(Probe) -> Reading<Seen>) -> Self {
        Self(Probe::ALL.map(read))
    }

    pub fn all_unknown(reason: &'static str) -> Self {
        Self::from_probes(|_| Reading::Unknown(reason))
    }

    pub fn get(&self, probe: Probe) -> &Reading<Seen> {
        &self.0[probe as usize]
    }

    pub fn complete(&self) -> bool {
        self.0.iter().all(Reading::is_value)
    }

    #[cfg(test)]
    fn with(mut self, probe: Probe, reading: Reading<Seen>) -> Self {
        self.0[probe as usize] = reading;
        self
    }
}

/// Count the arguments of every `pci_resize_resource(` declaration.
pub fn parse_pci_resize_args(text: &str) -> Reading<Seen> {
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
    Reading::Value(best.map_or(Seen::Absent, Seen::Args))
}

/// Read each distinct header once under the shared READING_TIMEOUT deadline.
pub fn read_interfaces(h: &dyn Host) -> KernelInterfaces {
    let start = h.now();
    let mut files: Vec<(&'static str, Reading<String>)> = Vec::new();
    KernelInterfaces::from_probes(|probe| {
        let header = probe.header();
        let index = match files.iter().position(|(path, _)| *path == header) {
            Some(index) => index,
            None => {
                files.push((header, read_header(h, header, start)));
                files.len() - 1
            }
        };
        match &files[index].1 {
            Reading::Value(text) => probe.parse(text),
            Reading::Unknown(reason) => Reading::Unknown(reason),
        }
    })
}

/// Recognize kbuild's effective compatibility append, excluding comments and
/// similarly named variables. Keep the exact assignment and expansion intact.
fn supports_extra_cflags(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.split('#').next().unwrap_or_default().trim();
        line.strip_prefix("ccflags-y")
            .and_then(|rest| rest.trim_start().strip_prefix("+="))
            .is_some_and(|rest| rest.trim() == "$(EXTRA_CFLAGS)")
    })
}

fn strip_c_comments(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/*") {
            let end = bytes[i + 2..].windows(2).position(|w| w == b"*/")?;
            out.push(b' ');
            i += end + 4;
        } else if bytes[i..].starts_with(b"//") {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(b' ');
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Find the unique whitespace-tolerant callback anchor, then inspect only the
/// fragment ending at its first semicolon. This is a header probe, not a C parser.
pub fn parse_fb_create_format_info(text: &str) -> Reading<Seen> {
    let unknown = Reading::Unknown(REASON_FORMAT_UNRECOGNIZED);
    let Some(bytes) = strip_c_comments(text) else {
        return unknown;
    };
    let mut anchor_end = None;
    for start in 0..bytes.len() {
        if bytes[start] != b'(' {
            continue;
        }
        let mut end = start;
        let matched = [b"(".as_slice(), b"*", b"fb_create", b")", b"("]
            .iter()
            .all(|token| {
                while end < bytes.len() && bytes[end].is_ascii_whitespace() {
                    end += 1;
                }
                if !bytes[end..].starts_with(token) {
                    return false;
                }
                end += token.len();
                true
            });
        if matched {
            if anchor_end.is_some() {
                return unknown;
            }
            anchor_end = Some(end);
        }
    }
    let Some(start) = anchor_end else {
        return unknown;
    };
    let Some(end) = bytes[start..].iter().position(|byte| *byte == b';') else {
        return unknown;
    };
    let tokens: Vec<_> = bytes[start..start + end]
        .split(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
        .filter(|token| !token.is_empty())
        .collect();
    presence(
        tokens
            .windows(2)
            .any(|pair| pair == [b"struct".as_slice(), b"drm_format_info".as_slice()]),
    )
}

fn header_deadline_expired(h: &dyn Host, start: std::time::Instant) -> bool {
    h.now().saturating_duration_since(start) >= READING_TIMEOUT
}

fn read_header(h: &dyn Host, path: &str, start: std::time::Instant) -> Reading<String> {
    if header_deadline_expired(h, start) {
        return Reading::Unknown(REASON_TIMED_OUT);
    }
    let p = h.view(std::path::Path::new(path));
    let value = match h.read(&p) {
        Ok(bytes) => Reading::Value(String::from_utf8_lossy(&bytes).into_owned()),
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
        Condition::Reads(probe, fires) => k.get(probe).value().map(|seen| fires.matches(*seen)),
    }
}

fn rule_applies(
    rule: &BranchRule,
    version: &Version,
    module_type: Option<KernelModuleType>,
) -> bool {
    (!rule.open_only || module_type == Some(KernelModuleType::Open))
        && (rule.branches.contains(&version.major())
            || rule
                .below
                .iter()
                .filter_map(|boundary| facts::parse_version_exact(boundary))
                .any(|boundary| boundary.major() == version.major() && version < &boundary))
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

/// Official source verdict, including the fixed 470 exception on 6.12 and 6.18.
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

/// The earliest replacement within this branch, when its failing rules have a boundary.
pub fn branch_range(
    version: &Version,
    kernel: &str,
    interfaces: &KernelInterfaces,
    loaded: &Reading<Vec<ModuleLine>>,
    module_type: Option<KernelModuleType>,
) -> Option<String> {
    if version.major() == 470 && community_patch_470(true, COMMUNITY_PATCH_VERSION, kernel) {
        return Some(COMMUNITY_PATCH_VERSION.to_string());
    }
    let matching: Vec<_> = BRANCHES
        .iter()
        .filter(|rule| {
            rule_applies(rule, version, module_type)
                && condition_verdict(rule.when, interfaces) == Some(true)
        })
        .collect();
    if matching
        .iter()
        .any(|rule| rule.branches.contains(&version.major()))
        || !official_module_reasons(version, kernel, loaded).is_empty()
    {
        return None;
    }
    matching
        .iter()
        .flat_map(|rule| rule.below.iter())
        .filter_map(|boundary| facts::parse_version_exact(boundary))
        .filter(|boundary| boundary.major() == version.major() && version < boundary)
        .max()
        .map(|boundary| boundary.text().to_string())
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

/// One or more kernel interface readings could not be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadersUnreadable {
    pub items: Vec<HeaderUnreadable>,
}

impl HeadersUnreadable {
    fn collect(k: &KernelInterfaces, keep: impl Fn(Probe) -> bool) -> Self {
        let mut items: Vec<HeaderUnreadable> = Vec::new();
        for probe in Probe::ALL {
            if !keep(probe) {
                continue;
            }
            if let Reading::Unknown(reason) = k.get(probe) {
                let path = probe.header();
                if !items.iter().any(|item| item.path == path) {
                    items.push(HeaderUnreadable { reason, path });
                }
            }
        }
        Self { items }
    }

    pub fn from_interfaces(k: &KernelInterfaces) -> Self {
        Self::collect(k, |_| true)
    }

    /// Only unknown probes consumed by an applicable rule contribute a path.
    pub fn for_branch(
        version: &Version,
        k: &KernelInterfaces,
        module_type: Option<KernelModuleType>,
    ) -> Self {
        Self::collect(k, |probe| {
            BRANCHES.iter().any(|rule| {
                rule_applies(rule, version, module_type)
                    && matches!(rule.when, Condition::Reads(p, _) if p == probe)
            })
        })
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

    #[test]
    fn branch_range_tracks_only_the_matching_branch_and_host_conditions() {
        let loaded = Reading::Value(vec![]);
        let interfaces = ifaces(false, false, Some(4), false, true);
        for kernel in ["6.12.105-production+truenas", "6.18.52-production+truenas"] {
            let v = facts::parse_version_exact("470.239.06").unwrap();
            assert_eq!(
                branch_range(&v, kernel, &interfaces, &loaded, None),
                Some(COMMUNITY_PATCH_VERSION.to_string())
            );
        }
        let v = facts::parse_version_exact("570.124.04").unwrap();
        assert_eq!(
            branch_range(
                &v,
                "6.18.52-production+truenas",
                &interfaces,
                &loaded,
                Some(KernelModuleType::Proprietary)
            ),
            Some("570.153.02".to_string())
        );
        let v = facts::parse_version_exact("580.119.02").unwrap();
        assert_eq!(
            branch_range(
                &v,
                "6.18.52-production+truenas",
                &interfaces,
                &loaded,
                Some(KernelModuleType::Open)
            ),
            Some("580.126.09".to_string())
        );
        let v = facts::parse_version_exact("575.64.05").unwrap();
        assert_eq!(
            branch_range(
                &v,
                "6.18.52-production+truenas",
                &interfaces,
                &loaded,
                Some(KernelModuleType::Open)
            ),
            None
        );
        let v = facts::parse_version_exact("545.29.06").unwrap();
        assert_eq!(
            branch_range(
                &v,
                "6.9.1",
                &ifaces(true, true, Some(3), true, false),
                &loaded,
                None
            ),
            None
        );
    }

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

    fn ifaces(
        pfn_absent: bool,
        poll_absent: bool,
        pci: Option<u32>,
        cflags_present: bool,
        fb_present: bool,
    ) -> KernelInterfaces {
        KernelInterfaces::all_unknown(REASON_FILE_MISSING)
            .with(Probe::FollowPfn, presence(!pfn_absent))
            .with(Probe::OutputPollChanged, presence(!poll_absent))
            .with(
                Probe::PciResizeResource,
                Reading::Value(pci.map_or(Seen::Absent, Seen::Args)),
            )
            .with(Probe::ExtraCflags, presence(cflags_present))
            .with(Probe::FbCreateFormatInfo, presence(fb_present))
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
            delay: u64,
            reads: std::sync::Mutex<Vec<std::path::PathBuf>>,
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
            fn read(&self, path: &std::path::Path) -> std::io::Result<Vec<u8>> {
                self.reads.lock().unwrap().push(path.to_path_buf());
                self.seconds
                    .fetch_add(self.delay, std::sync::atomic::Ordering::SeqCst);
                Ok(b"(*fb_create)(void);".to_vec())
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
            delay: 6,
            reads: std::sync::Mutex::new(Vec::new()),
        };
        let k = read_interfaces(&h);
        assert_eq!(
            h.now().duration_since(h.base),
            std::time::Duration::from_secs(18)
        );
        assert!(!k.complete());
        match k.get(Probe::PciResizeResource) {
            Reading::Unknown(reason) => assert_eq!(*reason, REASON_TIMED_OUT),
            other => panic!("expected a timed-out reading, got {other:?}"),
        }
        // The other two readings finished before the deadline.
        assert!(k.get(Probe::FollowPfn).is_value());
        assert!(k.get(Probe::OutputPollChanged).is_value());
        assert_eq!(
            k.get(Probe::FbCreateFormatInfo),
            &Reading::Value(Seen::Absent)
        );
        assert_eq!(
            k.get(Probe::ExtraCflags),
            &Reading::Unknown(REASON_TIMED_OUT)
        );
        let expected: Vec<std::path::PathBuf> = [
            Probe::FollowPfn,
            Probe::OutputPollChanged,
            Probe::PciResizeResource,
            Probe::ExtraCflags,
        ]
        .map(|probe| std::path::PathBuf::from(probe.header()))
        .to_vec();
        assert_eq!(*h.reads.lock().unwrap(), expected[..3]);
        let fast = SlowHeaders {
            seconds: std::sync::atomic::AtomicU64::new(0),
            base: std::time::Instant::now(),
            delay: 0,
            reads: std::sync::Mutex::new(Vec::new()),
        };
        assert!(read_interfaces(&fast).complete());
        assert_eq!(*fast.reads.lock().unwrap(), expected);
    }

    #[test]
    fn fb_create_probe_is_scoped_to_the_callback_declaration() {
        for (text, expected) in [
            // TrueNAS release/25.10.7 and release/27.0.0-RC.1 declarations.
            ("struct drm_framebuffer *(*fb_create)(struct drm_device *dev,\n struct drm_file *file_priv,\n const struct drm_mode_fb_cmd2 *mode_cmd);", false),
            ("struct drm_framebuffer *(*fb_create)(struct drm_device *dev,\n struct drm_file *file_priv,\n const struct drm_format_info *info,\n const struct drm_mode_fb_cmd2 *mode_cmd);", true),
            ("(*fb_create)(struct drm_device *dev); struct drm_format_info *outside;", false),
            ("(*fb_create)(void; struct drm_format_info *outside);", false),
            ("// (*fb_create)(const struct drm_format_info *info);\n(*fb_create)(struct drm_device *dev /* struct drm_format_info unavailable */);", false),
            ("( \t * fb_create \n ) (const struct drm_format_info *info);", true),
            ("(*fb_create)(const struct drm_format_info_x *info);", false),
        ] {
            assert_eq!(parse_fb_create_format_info(text), presence(expected), "{text}");
        }
        for text in [
            "(*fb_create)(void); (*fb_create)(void);",
            "no callback here",
            "/* unterminated",
            "(*fb_create)(void)",
        ] {
            assert_eq!(
                parse_fb_create_format_info(text),
                Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
                "{text}"
            );
        }
    }

    #[test]
    fn probe_rules_and_fires_are_total() {
        assert_eq!(
            Probe::ALL,
            [
                Probe::FollowPfn,
                Probe::OutputPollChanged,
                Probe::PciResizeResource,
                Probe::ExtraCflags,
                Probe::FbCreateFormatInfo
            ]
        );
        for (index, probe) in Probe::ALL.into_iter().enumerate() {
            assert_eq!(probe as usize, index);
            assert!(BRANCHES
                .iter()
                .any(|rule| matches!(rule.when, Condition::Reads(p, _) if p == probe)));
        }
        for seen in [Seen::Absent, Seen::Present, Seen::Args(3), Seen::Args(4)] {
            assert_eq!(Fires::WhenAbsent.matches(seen), seen == Seen::Absent);
            assert_eq!(Fires::WhenPresent.matches(seen), seen == Seen::Present);
            assert_eq!(
                Fires::WhenArgsOtherThan(3).matches(seen),
                seen == Seen::Args(4)
            );
        }
        assert_eq!(
            parse_pci_resize_args("no declaration"),
            Reading::Value(Seen::Absent)
        );
        assert_eq!(parse_pci_resize_args("pci_resize_resource(int a, int b, int c); pci_resize_resource(int a, int b, int c, int d);"), Reading::Value(Seen::Args(4)));
        assert_eq!(
            parse_pci_resize_args("pci_resize_resource(unfinished"),
            Reading::Unknown(REASON_FORMAT_UNRECOGNIZED)
        );
        let unknown = KernelInterfaces::all_unknown(REASON_FILE_MISSING);
        assert_eq!(
            condition_verdict(
                Condition::Reads(Probe::PciResizeResource, Fires::WhenArgsOtherThan(3)),
                &unknown
            ),
            None
        );
        let all = HeadersUnreadable::from_interfaces(&unknown);
        assert_eq!(
            all.items.iter().map(|item| item.path).collect::<Vec<_>>(),
            vec![
                Probe::FollowPfn.header(),
                Probe::OutputPollChanged.header(),
                Probe::PciResizeResource.header(),
                Probe::ExtraCflags.header()
            ]
        );
        let only_fb_unknown = ifaces(false, false, Some(3), true, false).with(
            Probe::FbCreateFormatInfo,
            Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
        );
        assert!(HeadersUnreadable::for_branch(
            &facts::parse_version_exact("545.29.06").unwrap(),
            &only_fb_unknown,
            None
        )
        .items
        .is_empty());
        let both_unknown = only_fb_unknown
            .clone()
            .with(Probe::OutputPollChanged, Reading::Unknown(REASON_IO_ERROR));
        assert_eq!(
            HeadersUnreadable::from_interfaces(&both_unknown).items,
            vec![HeaderUnreadable {
                reason: REASON_IO_ERROR,
                path: Probe::OutputPollChanged.header()
            }]
        );
        assert_eq!(
            HeadersUnreadable::for_branch(
                &facts::parse_version_exact("575.64.05").unwrap(),
                &both_unknown,
                None
            )
            .items,
            vec![HeaderUnreadable {
                reason: REASON_FORMAT_UNRECOGNIZED,
                path: Probe::FbCreateFormatInfo.header()
            }]
        );
        let fb = HeadersUnreadable::for_branch(
            &facts::parse_version_exact("575.64.05").unwrap(),
            &only_fb_unknown,
            None,
        );
        assert_eq!(
            fb.items,
            vec![HeaderUnreadable {
                reason: REASON_FORMAT_UNRECOGNIZED,
                path: Probe::FbCreateFormatInfo.header()
            }]
        );
    }

    #[test]
    fn rule_table_is_well_formed() {
        for rule in BRANCHES {
            assert!(rule
                .below
                .iter()
                .all(|boundary| facts::parse_version_exact(boundary).is_some()));
        }
        let unparseable = BranchRule {
            branches: &[],
            below: &["not-a-version"],
            open_only: false,
            when: Condition::Always,
            reason: BRANCH_EXTRA_CFLAGS_REMOVED,
            source: "test",
        };
        assert!(!rule_applies(
            &unparseable,
            &facts::parse_version_exact("570.124.04").unwrap(),
            None
        ));
        for (reason, boundaries, open_only) in [
            (BRANCH_PCI_RESIZE_RESOURCE_ARGS, &["580.126.09"][..], true),
            (
                BRANCH_EXTRA_CFLAGS_REMOVED,
                &["570.153.02", "575.57.08"][..],
                false,
            ),
        ] {
            let rule = BRANCHES.iter().find(|rule| rule.reason == reason).unwrap();
            assert_eq!(rule.below, boundaries);
            assert_eq!(rule.open_only, open_only);
        }
    }

    #[test]
    fn extra_cflags_version_boundaries_apply_to_every_module_type() {
        for extra_cflags in [true, false] {
            for format_info in [true, false] {
                let k = ifaces(true, true, Some(4), extra_cflags, format_info);
                for text in [
                    "570.124.04",
                    "570.133.07",
                    "570.144",
                    "570.153.02",
                    "570.211.01",
                    "575.51.02",
                    "575.57.08",
                ] {
                    let version = facts::parse_version_exact(text).unwrap();
                    let old = matches!(text, "570.124.04" | "570.133.07" | "570.144" | "575.51.02");
                    for module_type in [
                        None,
                        Some(KernelModuleType::Proprietary),
                        Some(KernelModuleType::Open),
                    ] {
                        let actual = verdict(&version, &k, module_type);
                        let mut expected = vec![];
                        if module_type == Some(KernelModuleType::Open) {
                            expected.push(BRANCH_PCI_RESIZE_RESOURCE_ARGS);
                        }
                        if !extra_cflags && old {
                            expected.push(BRANCH_EXTRA_CFLAGS_REMOVED);
                        }
                        if format_info && version.major() == 575 {
                            expected.push(BRANCH_FB_CREATE_FORMAT_INFO);
                        }
                        let expected = if expected.is_empty() {
                            BranchVerdict::Buildable
                        } else {
                            BranchVerdict::Unbuildable(expected)
                        };
                        assert_eq!(
                            actual, expected,
                            "{text}/{module_type:?}/extra={extra_cflags}/format={format_info}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn official_branch_verdicts_follow_kernel_build_and_drm_interfaces() {
        for (kernel, extra_cflags, format_info) in [
            ("6.12.105-production+truenas", true, false),
            ("6.18.52-production+truenas", false, true),
        ] {
            let k = ifaces(true, true, Some(4), extra_cflags, format_info);
            for version in [
                "470.256.02",
                "550.163.01",
                "565.77",
                "570.211.01",
                "575.64.05",
                "580.196.07",
            ] {
                let expected = if !extra_cflags && matches!(version, "550.163.01" | "565.77") {
                    BranchVerdict::Unbuildable(vec![BRANCH_EXTRA_CFLAGS_REMOVED])
                } else if format_info && version == "575.64.05" {
                    BranchVerdict::Unbuildable(vec![BRANCH_FB_CREATE_FORMAT_INFO])
                } else {
                    BranchVerdict::Buildable
                };
                assert_eq!(
                    official_verdict(
                        &facts::parse_version_exact(version).unwrap(),
                        kernel,
                        &k,
                        Some(KernelModuleType::Proprietary)
                    ),
                    expected,
                    "{kernel}/{version}"
                );
            }
        }
        let k = KernelInterfaces::all_unknown(REASON_FILE_MISSING);
        for (version, path) in [
            ("550.163.01", Probe::ExtraCflags.header()),
            ("575.64.05", Probe::OutputPollChanged.header()),
        ] {
            let version = facts::parse_version_exact(version).unwrap();
            assert_eq!(
                verdict(&version, &k, Some(KernelModuleType::Proprietary)),
                BranchVerdict::Undecidable
            );
            assert_eq!(
                HeadersUnreadable::for_branch(&version, &k, Some(KernelModuleType::Proprietary))
                    .items
                    .iter()
                    .map(|item| item.path)
                    .collect::<Vec<_>>(),
                vec![path]
            );
        }
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
                Reading::Value(Seen::Args(3)),
                Reading::Value(Seen::Args(4)),
                Reading::Value(Seen::Absent),
                Reading::Unknown(REASON_FILE_MISSING),
            ] {
                let k = ifaces(false, false, None, true, false)
                    .with(Probe::FollowPfn, Reading::Unknown(REASON_FILE_MISSING))
                    .with(
                        Probe::OutputPollChanged,
                        Reading::Unknown(REASON_FILE_MISSING),
                    )
                    .with(Probe::PciResizeResource, pci.clone());
                for module_type in [
                    None,
                    Some(KernelModuleType::Proprietary),
                    Some(KernelModuleType::Open),
                ] {
                    let expected =
                        if text == "580.119.02" && module_type == Some(KernelModuleType::Open) {
                            match pci {
                                Reading::Value(Seen::Args(4)) => BranchVerdict::Unbuildable(vec![
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
                        assert_eq!(unreadable.items[0].path, Probe::PciResizeResource.header());
                    }
                }
            }
        }
    }

    #[test]
    fn kernel_interface_readings_on_kernel_612_105() {
        let k = ifaces(true, true, Some(4), true, false);
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
        let k = ifaces(false, false, Some(3), true, false);
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
            Reading::Value(Seen::Args(3)),
            Reading::Value(Seen::Args(4)),
            Reading::Value(Seen::Absent),
            Reading::Unknown(REASON_FILE_MISSING),
        ] {
            let k =
                ifaces(false, false, None, true, false).with(Probe::PciResizeResource, pci.clone());
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
                    Reading::Value(Seen::Args(4)) => {
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
            &facts::parse_version_exact("570.211.01").unwrap(),
            &k,
            Some(KernelModuleType::Open),
        );
        assert_eq!(open.items.len(), 1);
        assert_eq!(open.items[0].path, Probe::PciResizeResource.header());
        assert!(HeadersUnreadable::for_branch(
            &facts::parse_version_exact("570.211.01").unwrap(),
            &k,
            None
        )
        .items
        .is_empty());
        assert!(HeadersUnreadable::for_branch(
            &facts::parse_version_exact("570.211.01").unwrap(),
            &k,
            Some(KernelModuleType::Proprietary)
        )
        .items
        .is_empty());
        let generic =
            HeadersUnreadable::for_branch(&facts::parse_version_exact("545.0").unwrap(), &k, None);
        assert_eq!(
            generic.items.iter().map(|i| i.path).collect::<Vec<_>>(),
            vec![
                Probe::FollowPfn.header(),
                Probe::OutputPollChanged.header(),
                Probe::ExtraCflags.header()
            ]
        );
        assert_eq!(HeadersUnreadable::from_interfaces(&k).items.len(), 4);
    }

    #[test]
    fn unreadable_header_makes_a_branch_undecidable() {
        let k = ifaces(false, false, Some(4), true, false)
            .with(Probe::FollowPfn, Reading::Unknown(REASON_FILE_MISSING))
            .with(
                Probe::OutputPollChanged,
                Reading::Unknown(REASON_FILE_MISSING),
            );
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
        for kernel in [
            "6.12.105-production+truenas",
            "6.12.96-production+truenas",
            "6.18.52-production+truenas",
            "6.18.0",
        ] {
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
            (true, COMMUNITY_PATCH_VERSION, "6.19.1"),
            (true, COMMUNITY_PATCH_VERSION, "6.180.1"),
            (true, COMMUNITY_PATCH_VERSION, "6.18evil.1"),
            (true, COMMUNITY_PATCH_VERSION, "6.1.x"),
            (true, COMMUNITY_PATCH_VERSION, "6.11.x"),
            (true, COMMUNITY_PATCH_VERSION, ""),
        ] {
            assert!(!community_patch_470(official, version, kernel));
        }
        let k = ifaces(true, true, Some(4), true, false);
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
        let interfaces = ifaces(true, true, Some(4), true, false);
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

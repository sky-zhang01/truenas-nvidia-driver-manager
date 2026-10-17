//! Generate `src/gpu_table.rs`, the built-in GPU table.
//!
//! Usage (maintainer machine, before a release):
//!
//! ```text
//! cargo run --locked --example gen-gpu-table -- --write src/gpu_table.rs
//! ```
//!
//! Runs on the maintainer's machine and uses the local `curl`; the program itself never reads
//! these sources.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use truenas_nvidia_driver_manager::catalog::{parse_index, Modules};
use truenas_nvidia_driver_manager::codes::MIN_MAJOR_BRANCH;
use truenas_nvidia_driver_manager::facts::Version;

const INDEX_URL: &str = "https://download.nvidia.com/XFree86/Linux-x86_64/";
const INSTALLER_BASE: &str = "https://raw.githubusercontent.com/NVIDIA/nvidia-installer";

/// `GPU_FLAGS_NO_GSP` (nvidia-installer `nvGpus.h`).
const GPU_FLAGS_NO_GSP: u16 = 1 << 0;
/// `GPU_FLAGS_VGPU_GUEST`.
const GPU_FLAGS_VGPU_GUEST: u16 = 1 << 1;
/// `GPU_FLAGS_VGPU_NO_GSP`.
const GPU_FLAGS_VGPU_NO_GSP: u16 = 1 << 2;
/// `GPU_FLAGS_PROPRIETARY_POSSIBLE`.
const GPU_FLAGS_PROPRIETARY_POSSIBLE: u16 = 1 << 3;

fn page_url(version: &str) -> String {
    format!("{INDEX_URL}{version}/README/supportedchips.html")
}

fn installer_url(version: &str) -> String {
    format!("{INSTALLER_BASE}/{version}/nvGpus.h")
}

/// One fetch performed by `curl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub exit: Option<i32>,
    pub status: Option<u16>,
    pub body: String,
    pub stderr: String,
}

impl Fetched {
    /// A read counts only with exit code 0 and HTTP status 200.
    fn read_ok(&self) -> bool {
        self.exit == Some(0) && self.status == Some(200)
    }
}

/// Split curl's stdout into the body and the HTTP status code on the last line.
fn split_status(raw: &str) -> (String, Option<u16>) {
    let Some(i) = raw.rfind('\n') else {
        return (raw.to_string(), None);
    };
    let (body, last) = raw.split_at(i);
    let code = last[1..].trim();
    let status = if code.len() == 3 && code != "000" && code.bytes().all(|b| b.is_ascii_digit()) {
        code.parse::<u16>().ok()
    } else {
        None
    };
    (body.to_string(), status)
}

/// Read one URL with the local `curl`; argv as catalog query with `--max-time 60`.
pub fn curl_fetch(url: &str) -> Fetched {
    let out = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--proto",
            "=https",
            "--connect-timeout",
            "5",
            "--max-time",
            "60",
            "--output",
            "-",
            "--write-out",
            "\\n%{http_code}",
            url,
        ])
        .output();
    match out {
        Ok(o) => {
            let raw = String::from_utf8_lossy(&o.stdout).into_owned();
            let (body, status) = split_status(&raw);
            Fetched {
                exit: o.status.code(),
                status,
                body,
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            }
        }
        Err(e) => Fetched {
            exit: None,
            status: None,
            body: String::new(),
            stderr: e.to_string(),
        },
    }
}

/// One supported-chips page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// Device ids listed in the supported section.
    pub supported: BTreeSet<u16>,
    /// Device id -> branch of the `legacy_<branch>.xx` section listing it.
    pub legacy: BTreeMap<u16, u32>,
    /// Device id -> product name.
    pub names: BTreeMap<u16, String>,
}

/// Why a supported-chips page could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageError(pub String);

impl fmt::Display for PageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The `GpuFlagList` of one installer release.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installer {
    /// Two-part PCI id -> flag bits.
    pub flags: BTreeMap<u16, u16>,
}

/// Why `nvGpus.h` could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallerError(pub String);

impl fmt::Display for InstallerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One row of a GPU table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub device: u16,
    pub name: String,
    pub last_branch: Option<u32>,
    pub first: Option<Version>,
    pub modules: Modules,
}

/// Why `build` refused to produce a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError {
    pub device: u16,
    pub version: String,
    pub reason: String,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gpu 0x{:04X} (version {}): {}",
            self.device, self.version, self.reason
        )
    }
}

/// Why `generate` stopped; `Display` writes what that step must print.
#[derive(Debug)]
pub enum GenError {
    Fetch { url: String, stderr: String },
    NoVersions { url: String },
    Page { version: String, reason: String },
    Installer { version: String, reason: String },
    Build(BuildError),
    Write { path: PathBuf, error: String },
}

impl fmt::Display for GenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenError::Fetch { url, stderr } => write!(f, "could not read {url}: {}", stderr.trim()),
            GenError::NoVersions { url } => {
                write!(
                    f,
                    "{url}: no version {MIN_MAJOR_BRANCH} or later in the index"
                )
            }
            GenError::Page { version, reason } => write!(f, "{version}: {reason}"),
            GenError::Installer { version, reason } => write!(f, "{version}: {reason}"),
            GenError::Build(e) => write!(f, "{e}"),
            GenError::Write { path, error } => {
                write!(f, "could not write {}: {error}", path.display())
            }
        }
    }
}

/// The numbers `generate` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub versions: usize,
    pub bytes: usize,
    pub entries: usize,
    /// Last branch -> number of entries, `None` (still supported) first.
    pub by_last_branch: Vec<(Option<u32>, usize)>,
    pub first_none: usize,
    pub closed_only: usize,
    pub open_only: usize,
    pub either: usize,
    pub installer: String,
}

impl Summary {
    fn of(versions: &[Version], installer: &Version, entries: &[Entry], bytes: usize) -> Summary {
        let mut by_last_branch: BTreeMap<Option<u32>, usize> = BTreeMap::new();
        let (mut first_none, mut closed_only, mut open_only, mut either) = (0, 0, 0, 0);
        for e in entries {
            *by_last_branch.entry(e.last_branch).or_insert(0) += 1;
            if e.first.is_none() {
                first_none += 1;
            }
            match e.modules {
                Modules::ClosedOnly => closed_only += 1,
                Modules::OpenOnly => open_only += 1,
                Modules::Either => either += 1,
            }
        }
        Summary {
            versions: versions.len(),
            bytes,
            entries: entries.len(),
            by_last_branch: by_last_branch.into_iter().collect(),
            first_none,
            closed_only,
            open_only,
            either,
            installer: installer.text().to_string(),
        }
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "versions: {}", self.versions)?;
        writeln!(f, "bytes: {}", self.bytes)?;
        writeln!(f, "entries: {}", self.entries)?;
        write!(f, "last_branch:")?;
        for (branch, n) in &self.by_last_branch {
            match branch {
                Some(b) => write!(f, " {b}={n}")?,
                None => write!(f, " supported={n}")?,
            }
        }
        writeln!(f)?;
        writeln!(f, "first_none: {}", self.first_none)?;
        writeln!(
            f,
            "modules: closed_only={}, open_only={}, either={}",
            self.closed_only, self.open_only, self.either
        )?;
        write!(f, "installer: {}", self.installer)
    }
}

/// Parse a `0x` + four hex digits id.
fn parse_hex4(s: &str) -> Option<u16> {
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    if h.len() != 4 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(h, 16).ok()
}

/// Parse `legacy_<branch>(.<branch>)*.xx`; the branch is the first number.
fn parse_legacy_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("legacy_")?.strip_suffix(".xx")?;
    let mut branch = None;
    for (i, seg) in rest.split('.').enumerate() {
        if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if i == 0 {
            branch = seg.parse::<u32>().ok();
        }
    }
    branch
}

struct Anchor {
    at: usize,
    branch: u32,
}

struct Row {
    at: usize,
    device: u16,
    subsystem: bool,
    name: String,
}

/// All `legacy_...` anchors, in page order.
fn legacy_anchors(html: &str) -> Result<Vec<Anchor>, PageError> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = html[from..].find("<a name=\"legacy_") {
        let at = from + rel;
        let name_start = at + "<a name=\"".len();
        let rest = &html[name_start..];
        let end = rest
            .find('"')
            .ok_or_else(|| PageError("legacy anchor without a closing quote".into()))?;
        let name = &rest[..end];
        let branch = parse_legacy_name(name).ok_or_else(|| {
            PageError(format!(
                "legacy anchor name {name:?} does not match legacy_<branch>.xx"
            ))
        })?;
        out.push(Anchor { at, branch });
        from = name_start + end;
    }
    Ok(out)
}

/// All `<tr id="devid...">` rows, in page order.
fn rows(html: &str) -> Result<Vec<Row>, PageError> {
    const MARK: &str = "<tr id=\"devid";
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = html[from..].find(MARK) {
        let at = from + rel;
        let mut i = at + MARK.len();
        let hex = |start: usize| -> Option<&str> {
            let s = html.get(start..start + 4)?;
            if s.bytes().all(|b| b.is_ascii_hexdigit()) {
                Some(s)
            } else {
                None
            }
        };
        let device_hex = hex(i)
            .ok_or_else(|| PageError(format!("row at {at}: device id is not four hex digits")))?;
        let device = u16::from_str_radix(device_hex, 16)
            .map_err(|_| PageError(format!("row at {at}: device id {device_hex:?}")))?;
        i += 4;
        let subsystem = if bytes.get(i) == Some(&b'_') {
            if hex(i + 1).is_none() || bytes.get(i + 5) != Some(&b'_') || hex(i + 6).is_none() {
                return Err(PageError(format!(
                    "row at {at}: subsystem part is not _<4 hex>_<4 hex>"
                )));
            }
            i += 10;
            true
        } else {
            false
        };
        if bytes.get(i) != Some(&b'"') || bytes.get(i + 1) != Some(&b'>') {
            return Err(PageError(format!(
                "row at {at}: id does not end with \">\" after the device id"
            )));
        }
        i += 2;
        if bytes.get(i) != Some(&b'\n') {
            return Err(PageError(format!(
                "row at {at}: product name is not on the next line"
            )));
        }
        let line_start = i + 1;
        let line_end = html[line_start..]
            .find('\n')
            .map(|e| line_start + e)
            .unwrap_or(html.len());
        let line = &html[line_start..line_end];
        let name = line
            .strip_prefix("<td>")
            .and_then(|s| s.strip_suffix("</td>"))
            .filter(|s| !s.contains('<'))
            .ok_or_else(|| {
                PageError(format!(
                    "row at {at}: name line {line:?} is not <td>name</td>"
                ))
            })?;
        out.push(Row {
            at,
            device,
            subsystem,
            name: name.to_string(),
        });
        from = i;
    }
    Ok(out)
}

/// Parse one supported-chips page.
pub fn parse_page(html: &str) -> Result<Page, PageError> {
    let anchors = legacy_anchors(html)?;
    let Some(first_anchor) = anchors.first().map(|a| a.at) else {
        return Err(PageError("page has no <a name=\"legacy_ anchor".into()));
    };
    let rows = rows(html)?;

    let mut page = Page::default();
    // Device id -> (subsystem row seen, name), in page order.
    let mut seen: BTreeMap<u16, (bool, String)> = BTreeMap::new();
    let mut supported_rows = 0usize;

    for row in &rows {
        let section = if row.at < first_anchor {
            None
        } else {
            let anchor = anchors
                .iter()
                .rev()
                .find(|a| a.at <= row.at)
                .ok_or_else(|| PageError(format!("row at {} is before every anchor", row.at)))?;
            Some(anchor.branch)
        };
        match section {
            None => {
                if page.legacy.contains_key(&row.device) {
                    return Err(PageError(format!(
                        "device 0x{:04X} is listed in both the supported section and legacy_{}.xx",
                        row.device, page.legacy[&row.device]
                    )));
                }
                page.supported.insert(row.device);
                supported_rows += 1;
            }
            Some(branch) => {
                if page.supported.contains(&row.device) {
                    return Err(PageError(format!(
                        "device 0x{:04X} is listed in both the supported section and legacy_{branch}.xx",
                        row.device
                    )));
                }
                if let Some(prev) = page.legacy.insert(row.device, branch) {
                    if prev != branch {
                        return Err(PageError(format!(
                            "device 0x{:04X} is listed in legacy_{prev}.xx and legacy_{branch}.xx",
                            row.device
                        )));
                    }
                }
            }
        }
        match seen.get_mut(&row.device) {
            Some(entry) if entry.0 && !row.subsystem => {
                entry.0 = false;
                entry.1 = row.name.clone();
            }
            Some(_) => {}
            None => {
                seen.insert(row.device, (row.subsystem, row.name.clone()));
            }
        }
    }

    if supported_rows == 0 {
        return Err(PageError(
            "the supported section has no <tr id=\"devid...\"> row".into(),
        ));
    }
    page.names = seen.into_iter().map(|(d, (_, n))| (d, n)).collect();
    Ok(page)
}

/// Parse the `GpuFlagList` of `nvGpus.h`.
pub fn parse_nvgpus(h: &str) -> Result<Installer, InstallerError> {
    let at = h
        .find("GpuFlagList[]")
        .ok_or_else(|| InstallerError("GpuFlagList is not in the file".into()))?;
    let open = h[at..]
        .find('{')
        .ok_or_else(|| InstallerError("GpuFlagList has no { after its name".into()))?
        + at;
    let mut flags = BTreeMap::new();
    for raw in h[open + 1..].lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if line == "};" {
            return Ok(Installer { flags });
        }
        let (device, value) = parse_flag_line(line)?;
        flags.insert(device, value);
    }
    Err(InstallerError("GpuFlagList is not terminated by };".into()))
}

fn parse_flag_line(line: &str) -> Result<(u16, u16), InstallerError> {
    let inner = line
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix("},").or_else(|| s.strip_suffix('}')))
        .ok_or_else(|| InstallerError(format!("unrecognized GpuFlagList line: {line:?}")))?;
    let (id, rest) = inner
        .split_once(',')
        .ok_or_else(|| InstallerError(format!("unrecognized GpuFlagList line: {line:?}")))?;
    let device = parse_hex4(id.trim()).ok_or_else(|| {
        InstallerError(format!("unrecognized PCI id in GpuFlagList line: {line:?}"))
    })?;
    let rest = rest.trim();
    let value = if rest == "0" {
        0
    } else {
        let mut value = 0u16;
        for token in rest.split('|') {
            value |= match token.trim() {
                "GPU_FLAGS_NO_GSP" => GPU_FLAGS_NO_GSP,
                "GPU_FLAGS_VGPU_GUEST" => GPU_FLAGS_VGPU_GUEST,
                "GPU_FLAGS_VGPU_NO_GSP" => GPU_FLAGS_VGPU_NO_GSP,
                "GPU_FLAGS_PROPRIETARY_POSSIBLE" => GPU_FLAGS_PROPRIETARY_POSSIBLE,
                other => {
                    return Err(InstallerError(format!(
                        "unknown flag {other:?} in GpuFlagList line: {line:?}"
                    )))
                }
            };
        }
        value
    };
    Ok((device, value))
}

/// Which kernel module kinds a GPU can use.
pub fn modules(installer: &Installer, device: u16) -> Modules {
    if device < 0x1E00 {
        return Modules::ClosedOnly;
    }
    let flags = installer.flags.get(&device).copied().unwrap_or(0);
    if flags & GPU_FLAGS_NO_GSP != 0 {
        Modules::ClosedOnly
    } else if flags & GPU_FLAGS_PROPRIETARY_POSSIBLE != 0 {
        Modules::Either
    } else {
        Modules::OpenOnly
    }
}

/// Build the table from the pages, newest version last.
pub fn build(pages: &[(Version, Page)], installer: &Installer) -> Result<Vec<Entry>, BuildError> {
    let Some((newest_version, newest)) = pages.iter().max_by(|a, b| a.0.cmp(&b.0)) else {
        return Err(BuildError {
            device: 0,
            version: "-".into(),
            reason: "no supported-chips page was read".into(),
        });
    };

    // A device is listed in one legacy section only; a device listed in a
    // legacy_<branch>.xx section is never in the supported section of a page
    // of a later branch, and never in a legacy section of a page of the same
    // branch or earlier.
    let mut legacy_branch: BTreeMap<u16, (u32, String)> = BTreeMap::new();
    for (v, page) in pages {
        for (device, branch) in &page.legacy {
            match legacy_branch.get(device) {
                Some((prev, prev_version)) if prev != branch => {
                    return Err(BuildError {
                        device: *device,
                        version: v.text().to_string(),
                        reason: format!(
                            "legacy_{branch}.xx here but legacy_{prev}.xx in {prev_version}"
                        ),
                    })
                }
                _ => {
                    legacy_branch.insert(*device, (*branch, v.text().to_string()));
                }
            }
        }
    }
    for (v, page) in pages {
        for (device, branch) in &page.legacy {
            for (v2, page2) in pages {
                if page2.supported.contains(device) && v2.major() > *branch {
                    return Err(BuildError {
                        device: *device,
                        version: v2.text().to_string(),
                        reason: format!(
                            "in the supported section but legacy_{branch}.xx in {}",
                            v.text()
                        ),
                    });
                }
            }
        }
    }
    // The branch of every page that lists the device in a legacy section must be greater than
    // that legacy branch: a page of branch <= L never lists the device under legacy_<L>.xx.
    for (v, page) in pages {
        for (device, branch) in &page.legacy {
            if v.major() <= *branch {
                return Err(BuildError {
                    device: *device,
                    version: v.text().to_string(),
                    reason: format!(
                        "legacy_{branch}.xx in a version of branch {} (must be greater than {branch})",
                        v.major()
                    ),
                });
            }
        }
    }

    let lists = |page: &Page, device: u16| {
        page.supported.contains(&device) || page.legacy.contains_key(&device)
    };

    let mut devices: BTreeSet<u16> = newest.supported.iter().copied().collect();
    devices.extend(newest.legacy.keys().copied());
    // A pre-Turing device that dropped out of the newest page but was in the supported
    // section of an earlier page is kept.
    for (_, page) in pages {
        for device in &page.supported {
            if !lists(newest, *device) && *device < 0x1E00 {
                devices.insert(*device);
            }
        }
    }

    let mut entries = Vec::with_capacity(devices.len());
    for device in devices {
        let in_newest = lists(newest, device);
        let (source_version, source_page): (&Version, &Page) = if in_newest {
            (newest_version, newest)
        } else {
            let (v, page) = pages
                .iter()
                .filter(|(_, page)| lists(page, device))
                .max_by(|a, b| a.0.cmp(&b.0))
                .expect("a device came from some page");
            (v, page)
        };
        let name = source_page
            .names
            .get(&device)
            .cloned()
            .ok_or_else(|| BuildError {
                device,
                version: source_version.text().to_string(),
                reason: "the page lists the device without a product name".into(),
            })?;
        let last_branch = match source_page.legacy.get(&device) {
            Some(branch) => Some(*branch),
            // A device still in the newest page's supported section has no
            // last branch; one that dropped out takes that page's branch.
            None if in_newest => None,
            None => Some(source_version.major()),
        };
        let first = pages
            .iter()
            .filter(|(_, page)| page.supported.contains(&device))
            .map(|(v, _)| v.clone())
            .min();
        entries.push(Entry {
            device,
            name,
            last_branch,
            first,
            modules: modules(installer, device),
        });
    }
    Ok(entries)
}

/// A Rust string literal for `text` (the names are ASCII in practice).
fn rust_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render the whole `src/gpu_table.rs`. `GPUS` is written with ascending `device`; the caller
/// does not have to pass a sorted slice.
pub fn render(versions: &[Version], installer: &Version, entries: &[Entry]) -> String {
    let mut ordered: Vec<&Entry> = entries.iter().collect();
    ordered.sort_by_key(|entry| entry.device);
    let mut out = String::new();
    out.push_str(
        "// Generated by `cargo run --locked --example gen-gpu-table -- --write src/gpu_table.rs`.\n",
    );
    out.push_str("// Do not edit; regenerate before a release.\n");
    out.push_str(
        "// Source: https://download.nvidia.com/XFree86/Linux-x86_64/<version>/README/supportedchips.html\n",
    );
    out.push_str("//         for every version in VERSIONS below (470 and later).\n");
    out.push_str(&format!("// Source: {}\n", installer_url(installer.text())));
    out.push_str("//         the installer release with the largest version number.\n");
    out.push_str("\nuse crate::catalog::{GpuEntry, Modules};\n\n");

    // The two statics are data: one entry per line, kept stable by `#[rustfmt::skip]`.
    out.push_str("#[rustfmt::skip]\npub static VERSIONS: &[&str] = &[\n");
    for chunk in versions.chunks(8) {
        out.push_str("    ");
        for (i, version) in chunk.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&rust_string(version.text()));
        }
        out.push_str(",\n");
    }
    out.push_str("];\n#[rustfmt::skip]\npub static GPUS: &[GpuEntry] = &[\n");
    for e in &ordered {
        let last_branch = match e.last_branch {
            Some(branch) => format!("Some({branch})"),
            None => "None".to_string(),
        };
        let first = match &e.first {
            Some(version) => format!("Some({})", rust_string(version.text())),
            None => "None".to_string(),
        };
        let modules = match e.modules {
            Modules::ClosedOnly => "ClosedOnly",
            Modules::OpenOnly => "OpenOnly",
            Modules::Either => "Either",
        };
        out.push_str(&format!(
            "    GpuEntry {{ device: 0x{:04X}, name: {}, last_branch: {last_branch}, first: {first}, modules: Modules::{modules} }},\n",
            e.device,
            rust_string(&e.name),
        ));
    }
    out.push_str("];\n");
    out
}

fn read(fetch: &dyn Fn(&str) -> Fetched, url: &str) -> Result<String, GenError> {
    let got = fetch(url);
    if !got.read_ok() {
        return Err(GenError::Fetch {
            url: url.to_string(),
            stderr: got.stderr,
        });
    }
    Ok(got.body)
}

fn write_table(out: &Path, text: &str) -> Result<(), GenError> {
    let tmp = PathBuf::from(format!("{}.tmp", out.display()));
    let result = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, out));
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(GenError::Write {
                path: out.to_path_buf(),
                error: e.to_string(),
            })
        }
    }
}

/// Fetch the source tables, validate their relationships and publish the generated table.
pub fn generate(fetch: &dyn Fn(&str) -> Fetched, out: &Path) -> Result<Summary, GenError> {
    let index = read(fetch, INDEX_URL)?;
    let mut versions: Vec<Version> = parse_index(&index)
        .into_iter()
        .filter(|v| v.major() >= MIN_MAJOR_BRANCH)
        .collect();
    versions.sort();
    versions.dedup();
    let Some(newest) = versions.last().cloned() else {
        return Err(GenError::NoVersions {
            url: INDEX_URL.to_string(),
        });
    };

    let mut pages = Vec::with_capacity(versions.len());
    for version in &versions {
        let url = page_url(version.text());
        let html = read(fetch, &url)?;
        let page = parse_page(&html).map_err(|e| GenError::Page {
            version: version.text().to_string(),
            reason: e.0,
        })?;
        pages.push((version.clone(), page));
    }

    let url = installer_url(newest.text());
    let header = read(fetch, &url)?;
    let installer = parse_nvgpus(&header).map_err(|e| GenError::Installer {
        version: newest.text().to_string(),
        reason: e.0,
    })?;

    let entries = build(&pages, &installer).map_err(GenError::Build)?;

    let text = render(&versions, &newest, &entries);
    write_table(out, &text)?;

    Ok(Summary::of(&versions, &newest, &entries, text.len()))
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.as_slice() {
        [_, flag, path] if flag == "--write" => match generate(&curl_fetch, Path::new(path)) {
            Ok(summary) => {
                print!("{summary}");
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: gen-gpu-table --write <path>");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use truenas_nvidia_driver_manager::facts::parse_version;

    /// Empty input table for checking generation without existing entries.
    const EMPTY_TABLE: &str = "use crate::catalog::{GpuEntry, Modules};\n\npub static VERSIONS: &[&str] = &[];\npub static GPUS: &[GpuEntry] = &[];\n";

    /// The fixed sample versions. The 11 pages below plus the newest version's `nvGpus.h` are
    /// the fixed inputs of the whole-table comparison. They cover every named case: `0FC0`
    /// last seen in a 470 page whose name differs from 470.42.01, `1DFE`/`1DFD` dropped from
    /// the newest page, `2E03` excluded, `2C33` merged from four subsystem rows into 570.190,
    /// `1180` only in a legacy section, `1DB4` with a plain and a subsystem row, and a device
    /// with only subsystem rows.
    const SAMPLE_VERSIONS: &[&str] = &[
        "470.42.01",
        "470.256.02",
        "495.29.05",
        "570.172.08",
        "570.190",
        "575.64.05",
        "580.76.05",
        "580.178.04",
        "590.48.01",
        "610.57.04",
        "615.71.09",
    ];

    fn page_html(supported: &[(&str, &str)], legacy: &[(u32, &[(&str, &str)])]) -> String {
        let mut s = String::from("<html>\n<body>\n");
        s.push_str("<h2><a name=\"supportedchips\" id=\"supportedchips\"></a>Supported GPUs</h2>\n<tbody>\n");
        for (id, name) in supported {
            s.push_str(&format!(
                "<tr id=\"devid{id}\">\n<td>{name}</td>\n<td>X</td>\n</tr>\n"
            ));
        }
        s.push_str("</tbody>\n");
        for (branch, rows) in legacy {
            s.push_str(&format!(
                "<p><a name=\"legacy_{branch}.xx\" id=\"legacy_{branch}.xx\"></a> The {branch}.xx driver supports the following set of GPUs:</p>\n<tbody>\n"
            ));
            for (id, name) in *rows {
                s.push_str(&format!(
                    "<tr id=\"devid{id}\">\n<td>{name}</td>\n<td>X</td>\n</tr>\n"
                ));
            }
            s.push_str("</tbody>\n");
        }
        s.push_str("</body>\n</html>\n");
        s
    }

    fn sample_index() -> String {
        let mut s = String::from("<html>\n<body>\n");
        for version in SAMPLE_VERSIONS {
            s.push_str(&format!("<a href=\"{version}/\">{version}/</a>\n"));
        }
        s.push_str("</body>\n</html>\n");
        s
    }

    /// The fixed sample `nvGpus.h`: flags come from `GpuFlagList` only, its `{` is followed
    /// by a column comment, blank lines are skipped, flag names join with `|`, and neither
    /// the `LegacyList` nor the `GpuSubDeviceFlagList` rows are flags.
    const SAMPLE_NVGPUS: &str = "/* nvGpus.h sample */\n\
static const GPU_FLAGS LegacyList[] = {\n\
// Legacy columns\n\
    { 0x2b85, GPU_FLAGS_NO_GSP },\n\
};\n\
static const GPU_FLAGS GpuFlagList[] = {\n\
//    PCI-ID  Flags\n\
\n\
    { 0x1b80, GPU_FLAGS_NO_GSP | GPU_FLAGS_PROPRIETARY_POSSIBLE },\n\
    { 0x1dff, GPU_FLAGS_PROPRIETARY_POSSIBLE },\n\
    { 0x2684, GPU_FLAGS_PROPRIETARY_POSSIBLE },\n\
    { 0x233b, GPU_FLAGS_NO_GSP | GPU_FLAGS_PROPRIETARY_POSSIBLE },\n\
    { 0x1e37, GPU_FLAGS_VGPU_GUEST | GPU_FLAGS_VGPU_NO_GSP | GPU_FLAGS_PROPRIETARY_POSSIBLE },\n\
};\n\
static const GPU_SUBDEVICE_FLAGS GpuSubDeviceFlagList[] = {\n\
// Subsystem columns\n\
    { 0x2b85, 0x10de, 0x1111, GPU_FLAGS_NO_GSP },\n\
};\n";

    /// Sample supported-device pages spanning the branch and subsystem cases.
    fn sample_pages() -> BTreeMap<String, String> {
        let mut pages = BTreeMap::new();
        let gtx4090 = ("2684", "NVIDIA GeForce RTX 4090");
        let gtx1080_earlier = ("1B80", "Earlier GTX 1080");
        let gtx1080 = ("1B80", "NVIDIA GeForce GTX 1080");
        let gtx480 = [("06C0", "GeForce GTX 480")];
        let legacy_390: &[(u32, &[(&str, &str)])] = &[(390, &gtx480)];

        pages.insert(
            "470.42.01".into(),
            page_html(
                &[
                    gtx4090,
                    gtx1080_earlier,
                    ("0FC0", "Older name for GT 640"),
                    ("1DFE", "Synthetic 1dfe old"),
                    ("1DFD", "Synthetic 1dfd old"),
                ],
                legacy_390,
            ),
        );
        pages.insert(
            "470.256.02".into(),
            page_html(
                &[gtx4090, gtx1080_earlier, ("0FC0", "NVIDIA GeForce GT 640")],
                legacy_390,
            ),
        );
        pages.insert(
            "495.29.05".into(),
            page_html(
                &[gtx4090, gtx1080_earlier],
                &[(470, &[("1180", "GeForce GTX 680")]), (390, &gtx480)],
            ),
        );
        pages.insert(
            "570.172.08".into(),
            page_html(&[gtx4090, gtx1080_earlier], legacy_390),
        );
        pages.insert(
            "570.190".into(),
            page_html(
                &[
                    gtx4090,
                    gtx1080_earlier,
                    ("2C33_1028_2053", "Earlier subsystem 1028 name"),
                ],
                legacy_390,
            ),
        );
        pages.insert(
            "575.64.05".into(),
            page_html(&[gtx4090, gtx1080_earlier], legacy_390),
        );
        pages.insert(
            "580.76.05".into(),
            page_html(
                &[
                    gtx4090,
                    gtx1080_earlier,
                    ("2C33_10DE_2053", "Earlier subsystem 10de name"),
                ],
                legacy_390,
            ),
        );
        pages.insert(
            "580.178.04".into(),
            page_html(
                &[gtx4090, gtx1080_earlier, ("1DFE", "Synthetic 1dfe latest")],
                &[(470, &[("1DFD", "Synthetic 1dfd latest")]), (390, &gtx480)],
            ),
        );
        pages.insert(
            "590.48.01".into(),
            page_html(&[gtx4090], &[(580, &[gtx1080]), (390, &gtx480)]),
        );
        pages.insert(
            "610.57.04".into(),
            page_html(
                &[gtx4090, ("2E03", "Dropped post-Turing device")],
                &[(580, &[gtx1080]), (390, &gtx480)],
            ),
        );
        pages.insert(
            "615.71.09".into(),
            page_html(
                &[
                    gtx4090,
                    ("1DB4_10DE_1111", "Tesla V100-PCIE-16GB-LS"),
                    ("1DB4", "Tesla V100-PCIE-16GB"),
                    ("1DFF", "Synthetic pre-Turing"),
                    ("233B", "Synthetic NO_GSP"),
                    ("2B85", "NVIDIA GeForce RTX 5090"),
                    ("2C33_1028_2053", "NVIDIA RTX 4000 Blackwell SFF"),
                    ("2C33_103C_2053", "Subsystem second"),
                    ("2C33_17AA_2053", "Subsystem third"),
                    ("2C33_10DE_2053", "NVIDIA RTX PRO 4000 Blackwell SFF"),
                    ("2F00_1028_0001", "Subsystem first"),
                    ("2F00_10DE_0002", "Subsystem later"),
                ],
                &[
                    (580, &[gtx1080]),
                    (470, &[("1180", "GeForce GTX 680")]),
                    (390, &gtx480),
                ],
            ),
        );
        pages
    }

    /// The parsed sample pages, ascending by version.
    fn sample_page_pairs() -> Vec<(Version, Page)> {
        SAMPLE_VERSIONS
            .iter()
            .map(|version| {
                (
                    parse_version(version).unwrap(),
                    parse_page(&sample_pages()[*version]).unwrap(),
                )
            })
            .collect()
    }

    fn ok(body: String) -> Fetched {
        Fetched {
            exit: Some(0),
            status: Some(200),
            body,
            stderr: String::new(),
        }
    }

    fn not_found() -> Fetched {
        Fetched {
            exit: Some(22),
            status: Some(404),
            body: String::new(),
            stderr: "curl: (22) The requested URL returned error: 404\n".into(),
        }
    }

    /// The happy-path responses; `patch` changes or removes single answers.
    fn responses(installer: Option<&str>) -> BTreeMap<String, Fetched> {
        let mut map = BTreeMap::new();
        map.insert(INDEX_URL.to_string(), ok(sample_index()));
        for (version, html) in sample_pages() {
            map.insert(page_url(&version), ok(html));
        }
        if let Some(header) = installer {
            map.insert(installer_url("615.71.09"), ok(header.to_string()));
        }
        map
    }

    fn fetcher(map: BTreeMap<String, Fetched>) -> impl Fn(&str) -> Fetched {
        move |url: &str| map.get(url).cloned().unwrap_or_else(not_found)
    }

    fn temp_out(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("gen-gpu-table-{}-{tag}.rs", std::process::id()));
        path
    }

    fn tmp_for(out: &Path) -> PathBuf {
        PathBuf::from(format!("{}.tmp", out.display()))
    }

    /// Start from the empty table and check the table survives a failure. Returns the error,
    /// so a test can also assert the failing step.
    fn expect_failure_error(fetch: &dyn Fn(&str) -> Fetched, tag: &str) -> GenError {
        let out = temp_out(tag);
        std::fs::write(&out, EMPTY_TABLE).unwrap();
        let err = generate(fetch, &out).expect_err("generate must fail");
        assert!(!err.to_string().is_empty());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), EMPTY_TABLE);
        assert!(
            !tmp_for(&out).exists(),
            "{} must be removed",
            tmp_for(&out).display()
        );
        let _ = std::fs::remove_file(&out);
        err
    }

    /// `expect_failure_error` for the tests that only need what it prints.
    fn expect_failure(fetch: &dyn Fn(&str) -> Fetched, tag: &str) -> String {
        expect_failure_error(fetch, tag).to_string()
    }

    /// `expect_failure` while recording every URL the fetcher sees.
    fn expect_failure_recording(
        map: &BTreeMap<String, Fetched>,
        tag: &str,
    ) -> (String, Vec<String>) {
        let calls: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let recording = |url: &str| {
            calls.borrow_mut().push(url.to_string());
            map.get(url).cloned().unwrap_or_else(not_found)
        };
        let message = expect_failure(&recording, tag);
        (message, calls.into_inner())
    }

    /// the only `nvGpus.h` version asked for is the newest one.
    fn assert_nvgpus_requests_are_only_the_newest(calls: &[String]) {
        let nvgpus: Vec<&String> = calls
            .iter()
            .filter(|url| url.ends_with("/nvGpus.h"))
            .collect();
        assert_eq!(nvgpus.len(), 1, "one nvGpus.h request, got {nvgpus:?}");
        assert_eq!(nvgpus[0].as_str(), installer_url("615.71.09").as_str());
    }

    /// Move a supported device into its last-supported branch section.
    fn page_with_1b80_in_legacy_580(version: &str) -> String {
        let row = "<tr id=\"devid1B80\">\n<td>Earlier GTX 1080</td>\n<td>X</td>\n</tr>\n";
        let page = sample_pages()[version].replace(row, "");
        assert_ne!(
            page,
            sample_pages()[version],
            "{version} must list 1B80 as supported"
        );
        let injected = "<p><a name=\"legacy_580.xx\" id=\"legacy_580.xx\"></a> The 580.xx driver supports the following set of GPUs:</p>\n<tbody>\n<tr id=\"devid1B80\">\n<td>NVIDIA GeForce GTX 1080</td>\n<td>X</td>\n</tr>\n</tbody>\n";
        page.replacen(
            "<p><a name=\"legacy_",
            &format!("{injected}<p><a name=\"legacy_"),
            1,
        )
    }

    /// with differing lines the test prints the number of the first differing line and both
    /// contents; a missing or extra line is a difference at the first line past the end of
    /// the shorter file.
    fn first_difference(actual: &str, expected: &str) -> Option<String> {
        let actual: Vec<&str> = actual.lines().collect();
        let expected: Vec<&str> = expected.lines().collect();
        for i in 0..actual.len().max(expected.len()) {
            let a = actual.get(i);
            let e = expected.get(i);
            if a != e {
                let show = |line: Option<&&str>| {
                    line.map(|s| format!("{s:?}"))
                        .unwrap_or_else(|| "<end of file>".to_string())
                };
                return Some(format!(
                    "first differing line {}:\n  actual:   {}\n  expected: {}",
                    i + 1,
                    show(a),
                    show(e)
                ));
            }
        }
        None
    }

    fn assert_lines_equal(actual: &str, expected: &str) {
        if let Some(message) = first_difference(actual, expected) {
            panic!("{message}");
        }
    }

    #[test]
    fn generate_writes_the_expected_table() {
        let out = temp_out("expected");
        std::fs::write(&out, EMPTY_TABLE).unwrap();
        let summary = generate(&fetcher(responses(Some(SAMPLE_NVGPUS))), &out).unwrap();
        let written = std::fs::read_to_string(&out).unwrap();
        assert_lines_equal(
            &written,
            include_str!("../tests/fixtures/gpu-table.expected.txt"),
        );
        assert!(!tmp_for(&out).exists());
        assert_eq!(summary.versions, SAMPLE_VERSIONS.len());
        assert_eq!(summary.entries, 13);
        assert_eq!(summary.bytes, written.len());
        assert_eq!(summary.installer, "615.71.09");
        assert_eq!(
            (
                summary.closed_only,
                summary.open_only,
                summary.either,
                summary.first_none
            ),
            (9, 3, 1, 2)
        );
        assert_eq!(
            summary.by_last_branch,
            vec![(None, 7), (Some(390), 1), (Some(470), 3), (Some(580), 2)]
        );
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn parse_page_reads_sections_and_names() {
        let pages = sample_pages();
        let page = parse_page(&pages["615.71.09"]).unwrap();
        assert_eq!(page.legacy.get(&0x1B80), Some(&580));
        assert_eq!(page.legacy.get(&0x1180), Some(&470));
        assert_eq!(page.legacy.get(&0x06C0), Some(&390));
        assert!(page.supported.contains(&0x2B85));
        assert!(page.supported.contains(&0x2C33));
        // Four subsystem rows of one device collapse to one entry, and the
        // first row on the page gives the name.
        assert_eq!(page.names[&0x2C33], "NVIDIA RTX 4000 Blackwell SFF");
        // A device with only subsystem rows takes its first row's name.
        assert_eq!(page.names[&0x2F00], "Subsystem first");
        // A plain row wins over the subsystem rows around it.
        assert_eq!(page.names[&0x1DB4], "Tesla V100-PCIE-16GB");
        // `legacy_173.14.xx` names branch 173.
        assert_eq!(parse_legacy_name("legacy_173.14.xx"), Some(173));
        let renamed = pages["615.71.09"].replace("legacy_390.xx", "legacy_173.14.xx");
        assert_eq!(parse_page(&renamed).unwrap().legacy[&0x06C0], 173);
    }

    #[test]
    fn parse_page_rejects_the_fixed_defects() {
        let page = sample_pages()["615.71.09"].clone();
        // No `legacy_` anchor at all.
        assert!(parse_page(&page.replace("<a name=\"legacy_", "<a name=\"wrong_")).is_err());
        // A row id that is not four hex digits (`devidZZZZ`).
        assert!(parse_page(&page.replace("devid2684", "devidZZZZ")).is_err());
        // The line after the row is not `<td>name</td>`.
        assert!(parse_page(&page.replace(
            "<td>NVIDIA GeForce RTX 4090</td>",
            "<th>NVIDIA GeForce RTX 4090</th>"
        ))
        .is_err());
        // The product name contains a tag.
        assert!(parse_page(&page.replace(
            "<td>NVIDIA GeForce RTX 4090</td>",
            "<td>NVIDIA <em>RTX</em></td>"
        ))
        .is_err());
        // The same device in the supported section and a legacy section.
        assert!(parse_page(&page.replace("devid2684", "devid1B80")).is_err());
        // The same device in two legacy sections.
        assert!(parse_page(&page.replace("devid1180", "devid06C0")).is_err());
        // A legacy anchor name that is not `legacy_<branch>.xx`.
        assert!(parse_page(&page.replace("legacy_390.xx", "legacy_bad.xx")).is_err());
        // No supported row at all.
        let empty_supported = "<html>\n<body>\n<a name=\"legacy_470.xx\"></a>\n<tbody>\n<tr id=\"devid1180\">\n<td>GeForce GTX 680</td>\n<td>X</td>\n</tr>\n</tbody>\n</body>\n</html>\n";
        assert!(parse_page(empty_supported).is_err());
    }

    #[test]
    fn parse_nvgpus_reads_the_flag_list() {
        let installer = parse_nvgpus(SAMPLE_NVGPUS).unwrap();
        assert_eq!(installer.flags.len(), 5);
        assert_eq!(installer.flags[&0x1B80], 0x9);
        assert_eq!(installer.flags[&0x2684], 0x8);
        assert_eq!(installer.flags[&0x233B], 0x9);
        assert_eq!(installer.flags[&0x1DFF], GPU_FLAGS_PROPRIETARY_POSSIBLE);
        assert_eq!(
            installer.flags[&0x1E37],
            GPU_FLAGS_VGPU_GUEST | GPU_FLAGS_VGPU_NO_GSP | GPU_FLAGS_PROPRIETARY_POSSIBLE
        );
        // `LegacyList` and `GpuSubDeviceFlagList` rows do not become flags.
        assert!(!installer.flags.contains_key(&0x2B85));
    }

    #[test]
    fn parse_nvgpus_rejects_the_fixed_defects() {
        assert!(parse_nvgpus(&SAMPLE_NVGPUS.replace("GpuFlagList[]", "OtherList[]")).is_err());
        assert!(parse_nvgpus(&SAMPLE_NVGPUS.replace(
            "{ 0x2684, GPU_FLAGS_PROPRIETARY_POSSIBLE }",
            "{ 0x2684 GPU_FLAGS_PROPRIETARY_POSSIBLE }"
        ))
        .is_err());
        assert!(
            parse_nvgpus(&SAMPLE_NVGPUS.replace("GPU_FLAGS_VGPU_GUEST", "GPU_FLAGS_FOO")).is_err()
        );
    }

    #[test]
    fn modules_follow_the_device_id_and_the_flags() {
        let installer = parse_nvgpus(SAMPLE_NVGPUS).unwrap();
        // Pre-Turing device ids are ClosedOnly without looking at the flags.
        assert_eq!(modules(&installer, 0x1B80), Modules::ClosedOnly);
        assert_eq!(modules(&installer, 0x1DFF), Modules::ClosedOnly);
        assert_eq!(modules(&installer, 0x1180), Modules::ClosedOnly);
        // `GPU_FLAGS_NO_GSP` is ClosedOnly.
        assert_eq!(modules(&installer, 0x233B), Modules::ClosedOnly);
        // `GPU_FLAGS_PROPRIETARY_POSSIBLE` is Either.
        assert_eq!(modules(&installer, 0x2684), Modules::Either);
        assert_eq!(modules(&installer, 0x1E37), Modules::Either);
        // A device in no `GpuFlagList` row has no flags at all: OpenOnly.
        // The `LegacyList` and `GpuSubDeviceFlagList` rows for 2B85 must not
        // count.
        assert_eq!(modules(&installer, 0x2B85), Modules::OpenOnly);
        assert_eq!(modules(&installer, 0x2C33), Modules::OpenOnly);

        // `2b85` is in none of the three tables -> OpenOnly. The sample above lists it in the
        // other two tables, so rename both rows and check the premise -- no `2b85` anywhere
        // -- before the module kind.
        assert_eq!(SAMPLE_NVGPUS.matches("0x2b85").count(), 2);
        let renamed = SAMPLE_NVGPUS.replace("0x2b85", "0x2fff");
        assert!(!renamed.contains("0x2b85"), "{renamed}");
        assert_eq!(renamed.matches("0x2fff").count(), 2);
        assert!(renamed.contains("{ 0x2fff, GPU_FLAGS_NO_GSP }"));
        assert!(renamed.contains("{ 0x2fff, 0x10de, 0x1111, GPU_FLAGS_NO_GSP }"));
        for table in ["LegacyList[]", "GpuFlagList[]", "GpuSubDeviceFlagList[]"] {
            assert!(renamed.contains(table), "{table} must stay in the sample");
        }
        let renamed_installer = parse_nvgpus(&renamed).unwrap();
        assert!(!renamed_installer.flags.contains_key(&0x2B85));
        assert_eq!(modules(&renamed_installer, 0x2B85), Modules::OpenOnly);
    }

    #[test]
    fn build_follows_the_fields_of_the_newest_page() {
        let installer = parse_nvgpus(SAMPLE_NVGPUS).unwrap();
        let entries = build(&sample_page_pairs(), &installer).unwrap();

        // The fixed table: `0FC0` takes the newer 470 name, `1DFE`/`1DFD` keep the newest
        // name with the right last branch, `2C33` starts at 570.190, `1180` has no first,
        // `1DB4` takes its plain row and `2F00` its first subsystem row.
        let expected = vec![
            (
                0x06C0,
                "GeForce GTX 480",
                Some(390),
                None,
                Modules::ClosedOnly,
            ),
            (
                0x0FC0,
                "NVIDIA GeForce GT 640",
                Some(470),
                Some("470.42.01"),
                Modules::ClosedOnly,
            ),
            (
                0x1180,
                "GeForce GTX 680",
                Some(470),
                None,
                Modules::ClosedOnly,
            ),
            (
                0x1B80,
                "NVIDIA GeForce GTX 1080",
                Some(580),
                Some("470.42.01"),
                Modules::ClosedOnly,
            ),
            (
                0x1DB4,
                "Tesla V100-PCIE-16GB",
                None,
                Some("615.71.09"),
                Modules::ClosedOnly,
            ),
            (
                0x1DFD,
                "Synthetic 1dfd latest",
                Some(470),
                Some("470.42.01"),
                Modules::ClosedOnly,
            ),
            (
                0x1DFE,
                "Synthetic 1dfe latest",
                Some(580),
                Some("470.42.01"),
                Modules::ClosedOnly,
            ),
            (
                0x1DFF,
                "Synthetic pre-Turing",
                None,
                Some("615.71.09"),
                Modules::ClosedOnly,
            ),
            (
                0x233B,
                "Synthetic NO_GSP",
                None,
                Some("615.71.09"),
                Modules::ClosedOnly,
            ),
            (
                0x2684,
                "NVIDIA GeForce RTX 4090",
                None,
                Some("470.42.01"),
                Modules::Either,
            ),
            (
                0x2B85,
                "NVIDIA GeForce RTX 5090",
                None,
                Some("615.71.09"),
                Modules::OpenOnly,
            ),
            (
                0x2C33,
                "NVIDIA RTX 4000 Blackwell SFF",
                None,
                Some("570.190"),
                Modules::OpenOnly,
            ),
            (
                0x2F00,
                "Subsystem first",
                None,
                Some("615.71.09"),
                Modules::OpenOnly,
            ),
        ];
        let actual: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.device,
                    entry.name.as_str(),
                    entry.last_branch,
                    entry.first.as_ref().map(|version| version.text()),
                    entry.modules,
                )
            })
            .collect();
        assert_eq!(actual, expected);

        // `last_branch` comes from the newest page that lists the device, `first` from the
        // smallest page that supports it.
        let pair: Vec<(Version, Page)> = sample_page_pairs()
            .into_iter()
            .filter(|(version, _)| version.text() == "580.178.04" || version.text() == "590.48.01")
            .collect();
        let one = build(&pair, &installer)
            .unwrap()
            .into_iter()
            .find(|entry| entry.device == 0x1B80)
            .unwrap();
        assert_eq!(one.last_branch, Some(580));
        assert_eq!(one.first, parse_version("580.178.04"));

        // The page order does not change the table.
        let mut reversed = sample_page_pairs();
        reversed.reverse();
        assert_eq!(build(&reversed, &installer).unwrap(), entries);

        // `2E03` (>= 0x1E00, only in the 610.57.04 supported section) stays
        // out of the table.
        assert!(!entries.iter().any(|entry| entry.device == 0x2E03));
        assert!(entries.windows(2).all(|w| w[0].device < w[1].device));
    }

    #[test]
    fn first_difference_reports_the_first_line_and_both_sides() {
        assert_eq!(first_difference("a\nb\nc\n", "a\nb\nc\n"), None);
        let same_length = first_difference("a\nb\nc\n", "a\nx\nc\n").unwrap();
        assert!(same_length.contains("line 2"), "{same_length}");
        assert!(same_length.contains("\"b\""), "{same_length}");
        assert!(same_length.contains("\"x\""), "{same_length}");
        // A missing line is reported at the first line past the end.
        let missing = first_difference("a\nb\n", "a\nb\nc\n").unwrap();
        assert!(missing.contains("line 3"), "{missing}");
        assert!(missing.contains("<end of file>"), "{missing}");
        assert!(missing.contains("\"c\""), "{missing}");
        // An extra line is reported the same way.
        let extra = first_difference("a\nb\nc\n", "a\nb\n").unwrap();
        assert!(extra.contains("line 3"), "{extra}");
        assert!(extra.contains("\"c\""), "{extra}");
        assert!(extra.contains("<end of file>"), "{extra}");
    }

    #[test]
    fn render_sorts_entries_by_device() {
        let versions: Vec<Version> = SAMPLE_VERSIONS
            .iter()
            .map(|version| parse_version(version).unwrap())
            .collect();
        let installer = parse_version("615.71.09").unwrap();
        let mut entries =
            build(&sample_page_pairs(), &parse_nvgpus(SAMPLE_NVGPUS).unwrap()).unwrap();
        entries.reverse();
        let rendered = render(&versions, &installer, &entries);
        let devices: Vec<&str> = rendered
            .lines()
            .filter_map(|line| line.strip_prefix("    GpuEntry { device: 0x"))
            .map(|rest| &rest[..4])
            .collect();
        assert_eq!(
            devices,
            [
                "06C0", "0FC0", "1180", "1B80", "1DB4", "1DFD", "1DFE", "1DFF", "233B", "2684",
                "2B85", "2C33", "2F00"
            ]
        );
        // Same input twice is byte-identical, and the header names the
        // sources and every version.
        assert_eq!(rendered, render(&versions, &installer, &entries));
        assert!(rendered.contains(INDEX_URL));
        assert!(rendered.contains(&installer_url("615.71.09")));
        for version in SAMPLE_VERSIONS {
            assert!(rendered.contains(&format!("\"{version}\"")), "{version}");
        }
    }

    #[test]
    fn failure_page_without_a_legacy_anchor_keeps_the_table() {
        let mut map = responses(Some(SAMPLE_NVGPUS));
        let page = sample_pages()["470.256.02"].replace("<a name=\"legacy_390.xx\"", "<p");
        map.insert(page_url("470.256.02"), ok(page));
        expect_failure(&fetcher(map), "no-anchor");
        // The reference page really does have anchors.
        assert!(sample_pages()["470.256.02"].contains("<a name=\"legacy_"));
    }

    #[test]
    fn failure_device_in_two_sections_keeps_the_table() {
        let mut map = responses(Some(SAMPLE_NVGPUS));
        let page = sample_pages()["615.71.09"].replace(
            "<tr id=\"devid2684\">\n<td>NVIDIA GeForce RTX 4090</td>",
            "<tr id=\"devid1B80\">\n<td>NVIDIA GeForce GTX 1080</td>",
        );
        map.insert(page_url("615.71.09"), ok(page));
        expect_failure(&fetcher(map), "two-sections");
    }

    #[test]
    fn failure_page_unreadable_keeps_the_table() {
        let mut map = responses(Some(SAMPLE_NVGPUS));
        map.remove(&page_url("580.178.04"));
        expect_failure(&fetcher(map), "page-404");
    }

    #[test]
    fn failure_index_unreadable_keeps_the_table() {
        let mut map = responses(Some(SAMPLE_NVGPUS));
        map.remove(INDEX_URL);
        let message = expect_failure(&fetcher(map), "index-404");
        assert!(message.contains(INDEX_URL), "{message}");
    }

    #[test]
    fn failure_index_without_a_usable_version_keeps_the_table() {
        let mut map = responses(Some(SAMPLE_NVGPUS));
        map.insert(
            INDEX_URL.to_string(),
            ok("<a href=\"390.157/\">390.157/</a>\n".into()),
        );
        expect_failure(&fetcher(map), "index-410");
    }

    #[test]
    fn failure_latest_nvgpus_unreadable_keeps_the_table() {
        // HTTP 404 for the newest version's `nvGpus.h`.
        let (message, calls) = expect_failure_recording(&responses(None), "latest-404");
        assert!(message.contains(&installer_url("615.71.09")), "{message}");
        assert_nvgpus_requests_are_only_the_newest(&calls);

        // The fetch itself failed: `curl` did not run or was killed.
        let mut map = responses(Some(SAMPLE_NVGPUS));
        map.insert(
            installer_url("615.71.09"),
            Fetched {
                exit: None,
                status: None,
                body: String::new(),
                stderr: "curl: (28) connection timed out\n".into(),
            },
        );
        let (message, calls) = expect_failure_recording(&map, "latest-fetch-error");
        assert!(message.contains("curl: (28)"), "{message}");
        assert_nvgpus_requests_are_only_the_newest(&calls);
    }

    #[test]
    fn failure_installer_line_without_a_comma_keeps_the_table() {
        let broken = SAMPLE_NVGPUS.replace(
            "{ 0x2684, GPU_FLAGS_PROPRIETARY_POSSIBLE }",
            "{ 0x2684 GPU_FLAGS_PROPRIETARY_POSSIBLE }",
        );
        let map = responses(Some(&broken));
        expect_failure(&fetcher(map), "installer-line");
    }

    #[test]
    fn failure_installer_unknown_flag_keeps_the_table() {
        let broken = SAMPLE_NVGPUS.replace(
            "{ 0x2684, GPU_FLAGS_PROPRIETARY_POSSIBLE }",
            "{ 0x2684, GPU_FLAGS_NOT_A_FLAG }",
        );
        let map = responses(Some(&broken));
        expect_failure(&fetcher(map), "installer-flag");
    }

    #[test]
    fn failure_legacy_device_in_a_later_supported_section_keeps_the_table() {
        // 615.71.09 puts 1B80 under legacy_580.xx, so the 590.48.01 page must not list it in
        // the supported section. This control adds that supported row but keeps the
        // legacy_580.xx row: the same-page double section stops `parse_page`, and the failing
        // step is asserted here. The true move that reaches `build` is the next test.
        let base = sample_pages()["590.48.01"].clone();
        assert_eq!(base.matches("<p><a name=\"legacy_580.xx\"").count(), 1);
        let appended = base.replace(
            "<p><a name=\"legacy_580.xx\"",
            "<tr id=\"devid1B80\">\n<td>NVIDIA GeForce GTX 1080</td>\n<td>X</td>\n</tr>\n<p><a name=\"legacy_580.xx\"",
        );
        assert_ne!(appended, base);
        assert_eq!(appended.matches("<tr id=\"devid1B80\">").count(), 2);
        assert!(parse_page(&appended).is_err());
        let mut map = responses(Some(SAMPLE_NVGPUS));
        map.insert(page_url("590.48.01"), ok(appended));
        let error = expect_failure_error(&fetcher(map), "later-supported");
        match &error {
            GenError::Page { version, reason } => {
                assert_eq!(version, "590.48.01");
                assert!(reason.contains("1B80"), "{reason}");
            }
            other => panic!("parse_page must be the failing step, got {other:?}"),
        }
    }

    #[test]
    fn failure_legacy_device_moved_to_a_later_supported_section_reaches_build() {
        // 590.48.01 (branch 590 > 580) must not list in its supported section a device that
        // 615.71.09 lists under legacy_580.xx. Move the 1B80 row of that page from the
        // legacy_580.xx section into the supported section: the page parses, and `build`
        // refuses, naming the device and that version.
        let page_615 = parse_page(&sample_pages()["615.71.09"]).unwrap();
        assert_eq!(page_615.legacy.get(&0x1B80), Some(&580));
        let base = sample_pages()["590.48.01"].clone();
        let row = "<tr id=\"devid1B80\">\n<td>NVIDIA GeForce GTX 1080</td>\n<td>X</td>\n</tr>\n";
        assert_eq!(base.matches(row).count(), 1, "1B80 is listed once");
        assert_eq!(base.matches("<p><a name=\"legacy_580.xx\"").count(), 1);
        let moved = base.replace(row, "").replacen(
            "<p><a name=\"legacy_580.xx\"",
            &format!("{row}<p><a name=\"legacy_580.xx\""),
            1,
        );
        assert_eq!(moved.matches(row).count(), 1, "the row moved, not copied");
        let parsed = parse_page(&moved).unwrap();
        assert!(parsed.supported.contains(&0x1B80));
        assert!(!parsed.legacy.contains_key(&0x1B80));

        let mut map = responses(Some(SAMPLE_NVGPUS));
        map.insert(page_url("590.48.01"), ok(moved));
        let error = expect_failure_error(&fetcher(map), "later-supported-build");
        match &error {
            GenError::Build(problem) => {
                assert_eq!(problem.device, 0x1B80);
                assert_eq!(problem.version, "590.48.01");
            }
            other => panic!("build must be the failing step, got {other:?}"),
        }
    }

    #[test]
    fn failure_legacy_in_a_not_newer_page_keeps_the_table() {
        // a page of branch <= 580 must not list 1B80 under legacy_580.xx when 615.71.09 puts
        // it there. These cases move the row on the 470.42.01 (<580) and the
        // 580.178.04 (=580) pages; both must fail and name the device and the version.
        for (version, tag) in [
            ("470.42.01", "legacy-not-newer-470"),
            ("580.178.04", "legacy-not-newer-580"),
        ] {
            let mut map = responses(Some(SAMPLE_NVGPUS));
            map.insert(page_url(version), ok(page_with_1b80_in_legacy_580(version)));
            let message = expect_failure(&fetcher(map), tag);
            assert!(message.contains("1B80"), "{message}");
            assert!(message.contains(version), "{message}");
        }
    }

    #[test]
    fn split_status_reads_the_last_line() {
        assert_eq!(split_status("body\n200"), ("body".into(), Some(200)));
        assert_eq!(split_status("000"), ("000".into(), None));
        assert_eq!(split_status("body\n"), ("body".into(), None));
        assert_eq!(split_status(""), ("".into(), None));
    }

    #[test]
    fn cli_generate_uses_the_given_fetch() {
        // `generate` is the only path the CLI takes; the fetcher here is the
        // one the CLI passes (`curl_fetch`).
        let names: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let map = responses(Some(SAMPLE_NVGPUS));
        let recording = |url: &str| {
            names.borrow_mut().push(url.to_string());
            map.get(url).cloned().unwrap_or_else(not_found)
        };
        let out = temp_out("cli");
        let summary = generate(&recording, &out).unwrap();
        assert_eq!(summary.entries, 13);
        assert!(!tmp_for(&out).exists());
        let names = names.borrow();
        assert_eq!(names.len(), SAMPLE_VERSIONS.len() + 2);
        assert_eq!(names.first().map(String::as_str), Some(INDEX_URL));
        assert_eq!(
            names.last().map(String::as_str),
            Some(installer_url("615.71.09").as_str())
        );
        let _ = std::fs::remove_file(&out);
    }
}

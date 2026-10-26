//! Downloading the driver and the toolkit on the host.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::codes::*;
use crate::host::{At, Cmd, Stream, Watch};
use crate::runner::RunCtx;
use crate::upload;

pub type DownloadFailure = crate::runner::build::BuildFailure;

fn timeout_failure() -> DownloadFailure {
    DownloadFailure {
        code: ErrorCode::BuildTimeout,
        params: json!({}),
        raw: None,
        command: None,
    }
}

/// A build timeout for a `curl` that really ran: the executed command and its stderr travel
/// with the failure. Only a budget that expired before `curl` started may lack the raw
/// output.
fn timeout_failure_with_output(cmd: &Cmd, out: &crate::host::Output) -> DownloadFailure {
    DownloadFailure {
        code: ErrorCode::BuildTimeout,
        params: json!({}),
        raw: Some(out.raw_tail(ERROR_RAW_LINES)),
        command: Some((cmd.argv(), out.exit)),
    }
}

fn request_failure(file_name: &str, raw: String) -> DownloadFailure {
    DownloadFailure::new(
        ErrorCode::DownloadFailed,
        DOWNLOAD_REQUEST_FAILED,
        json!({"file_name": file_name}),
        Some(raw),
    )
}

fn checksum_failure(file_name: &str) -> DownloadFailure {
    DownloadFailure::new(
        ErrorCode::DownloadFailed,
        DOWNLOAD_CHECKSUM_MISMATCH,
        json!({"file_name": file_name}),
        None,
    )
}

fn community_patch(ctx: &RunCtx) -> bool {
    let plan = ctx.state.plan.as_ref().expect("running task has a plan");
    crate::catalog::community_patch_470(
        matches!(&plan.source, crate::task_state::PlanSource::Official),
        &plan.version,
        &plan.kernel,
    )
}

fn hash_fixed_input(
    ctx: &mut RunCtx,
    path: &std::path::Path,
    deadline: Instant,
    file_name: &str,
) -> Result<crate::store::Copied, DownloadFailure> {
    let digest = match crate::store::hash_file_within(ctx.host.as_ref(), path, deadline) {
        Ok(digest) => digest,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            ctx.log(LogKey::BuildTimeout, json!({}));
            return Err(timeout_failure());
        }
        Err(e) => return Err(request_failure(file_name, e.to_string())),
    };
    if remaining(ctx.host.now(), deadline).is_none() {
        ctx.log(LogKey::BuildTimeout, json!({}));
        return Err(timeout_failure());
    }
    Ok(digest)
}

/// One selected toolkit entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolkitFile {
    pub package: String,
    pub filename: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolkitPick {
    pub version: String,
    pub files: Vec<ToolkitFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickError {
    IndexUnreadable,
    NoCommonVersion,
    EntryUnexpected(String),
}

/// One toolkit package paragraph that carries a selectable version. `Filename`, `Size` and
/// `SHA256` are kept as they are: only the selected version's entry must be complete, so the
/// metadata of a version that is not selected can never spoil the selection.
struct IndexEntry {
    upstream: Vec<u32>,
    revision: Option<u32>,
    version: String,
    filename: Option<String>,
    size: Option<u64>,
    sha256: Option<String>,
}

/// Parse the toolkit `Packages` index and pick the newest common version.
pub fn pick_toolkit(packages: &str) -> Result<ToolkitPick, PickError> {
    let mut by_package: BTreeMap<String, Vec<IndexEntry>> = BTreeMap::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for paragraph in packages.split("\n\n") {
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        for line in paragraph.lines() {
            if line.starts_with(' ') || line.starts_with('\t') {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim();
                if !key.is_empty() {
                    fields.insert(key.to_string(), v.trim().to_string());
                }
            }
        }
        let Some(package) = fields.get("Package") else {
            continue;
        };
        if !TOOLKIT_PACKAGES.contains(&package.as_str()) {
            continue;
        }
        if fields.get("Architecture").map(|v| v.as_str()) != Some("amd64") {
            continue;
        }
        let Some(version) = fields.get("Version") else {
            // Without a version this paragraph cannot be selected; it does not make the index
            // unreadable.
            continue;
        };
        // The same (Package, Version) must not appear twice.
        if !seen.insert((package.clone(), version.clone())) {
            return Err(PickError::IndexUnreadable);
        }
        let Some((upstream, revision)) = upload::deb_version_ok(version) else {
            // A version outside the rule never takes part in the selection.
            continue;
        };
        by_package
            .entry(package.clone())
            .or_default()
            .push(IndexEntry {
                upstream,
                revision,
                version: version.clone(),
                filename: fields.get("Filename").cloned(),
                size: fields.get("Size").and_then(|v| v.parse::<u64>().ok()),
                sha256: fields.get("SHA256").cloned(),
            });
    }
    if by_package.len() != TOOLKIT_PACKAGES.len() {
        return Err(PickError::IndexUnreadable);
    }
    // Versions present in all four packages, compared as the complete (upstream version,
    // revision) pair: the selection must be one version that every package really offers,
    // never a revision mixed from different upstream versions.
    let mut common: Option<Vec<(Vec<u32>, Option<u32>)>> = None;
    for name in TOOLKIT_PACKAGES {
        let Some(entries) = by_package.get(name) else {
            return Err(PickError::IndexUnreadable);
        };
        let versions: Vec<(Vec<u32>, Option<u32>)> = entries
            .iter()
            .map(|e| (e.upstream.clone(), e.revision))
            .collect();
        common = Some(match common {
            None => versions,
            Some(prev) => prev.into_iter().filter(|v| versions.contains(v)).collect(),
        });
    }
    let common = common.unwrap_or_default();
    let Some(best) = common.into_iter().max() else {
        return Err(PickError::NoCommonVersion);
    };
    let mut files = Vec::new();
    let mut selected_version = String::new();
    for name in TOOLKIT_PACKAGES {
        let entries = &by_package[name];
        let Some(entry) = entries
            .iter()
            .find(|e| e.upstream == best.0 && e.revision == best.1)
        else {
            return Err(PickError::NoCommonVersion);
        };
        // Only the selected version is required to be complete; an unselected version's
        // metadata never takes part.
        let expected = format!("./{name}_{}_amd64.deb", entry.version);
        let (Some(filename), Some(size), Some(sha256)) =
            (&entry.filename, entry.size, &entry.sha256)
        else {
            return Err(PickError::EntryUnexpected(name.to_string()));
        };
        if filename != &expected {
            return Err(PickError::EntryUnexpected(name.to_string()));
        }
        if selected_version.is_empty() {
            selected_version = entry.version.clone();
        }
        files.push(ToolkitFile {
            package: name.to_string(),
            filename: format!("{name}_{}_amd64.deb", entry.version),
            size,
            sha256: sha256.clone(),
        });
    }
    Ok(ToolkitPick {
        version: selected_version,
        files,
    })
}

struct ProgressWatch<'a> {
    ctx: &'a mut RunCtx,
    part: PathBuf,
    headers: PathBuf,
}

impl<'a> Watch for ProgressWatch<'a> {
    fn line(&mut self, _from: Stream, _line: &[u8]) {}
    fn tick(&mut self) {
        let done = self
            .ctx
            .host
            .lstat(&self.part)
            .map(|m| m.len())
            .unwrap_or(0);
        let total = read_content_length(self.ctx, &self.headers);
        match total {
            Some(total) => {
                self.ctx.state.download = Some(crate::task_state::DownloadProgress {
                    done_bytes: done,
                    total_bytes: Some(total),
                });
            }
            None => {
                self.ctx.state.download = Some(crate::task_state::DownloadProgress {
                    done_bytes: done,
                    total_bytes: None,
                });
            }
        }
        let _ = self.ctx.write_state();
    }
}

/// The `Content-Length` of the *last* response segment in a curl header file: an earlier
/// segment -- a proxy `CONNECT` answer or a `100 Continue` -- must never supply the total for
/// the final response, and a final response without the header has an unknown total.
fn read_content_length(ctx: &RunCtx, headers: &std::path::Path) -> Option<u64> {
    let bytes = ctx.host.read(headers).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut current: Option<u64> = None;
    let mut last: Option<u64> = None;
    let mut in_segment = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            // The segment ended; its value (or the absence of one) is the
            // reading so far.
            last = current;
            current = None;
            in_segment = false;
            continue;
        }
        if !in_segment {
            // A line after a blank line starts a new response segment; its
            // headers replace the previous segment's value.
            in_segment = true;
            current = None;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                current = v.trim().parse::<u64>().ok();
            }
        }
    }
    if in_segment {
        // The last segment is still open (the file was read mid-write); its
        // value so far is the reading.
        last = current;
    }
    last
}

pub(crate) fn remaining(now: Instant, deadline: Instant) -> Option<Duration> {
    // `curl --max-time` takes whole seconds; the rule is not to start a command when less
    // than one second remains, which counts as having reached the deadline.
    if now + Duration::from_secs(1) > deadline {
        None
    } else {
        Some(deadline.saturating_duration_since(now))
    }
}

/// `curl` ended by its `--max-time` or by the host deadline is a build timeout, not a
/// download failure.
fn curl_timed_out(exit: crate::host::Exit) -> bool {
    matches!(
        exit,
        crate::host::Exit::TimedOut | crate::host::Exit::Code(28)
    )
}

/// Whether this one download follows redirects. Chosen at the call site.
#[derive(Clone, Copy)]
pub enum CurlRedirects {
    /// The first response is the whole transfer. A 3xx stays that status.
    Stop,
    /// Follow https redirects, at most five times.
    ///
    /// Build-kit packages and the community archive use this. Bytes are kept
    /// only when their fixed SHA256 and size match.
    Https,
}

/// Redirects a build-kit or community archive download may follow. snapshot.debian.org answers an
/// `/archive/…` URL with one same-host https redirect; five is the cap.
const CURL_MAX_REDIRS: u32 = 5;

/// Connect timeout of one download invocation: a connect that never opens
/// fails at this bound instead of waiting for the task deadline.
const CURL_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Transient errors one `curl` invocation retries on its own; the host command
/// timeout of the invocation stays the bound on the whole download.
const CURL_RETRIES: u32 = 2;

/// curl invocation.
///
/// Every caller passes `CurlRedirects` itself. `Https` adds `--location`, `--proto-redir
/// =https` and `--max-redirs 5`. `Stop` leaves those off, so `%{http_code}` is the first
/// response.
fn curl_cmd(
    url: &str,
    output: &std::path::Path,
    headers: &std::path::Path,
    timeout: Duration,
    redirects: CurlRedirects,
) -> Cmd {
    let mut cmd = Cmd::new(At::Host, "curl", timeout)
        .arg("--silent")
        .arg("--show-error")
        .arg("--fail")
        .arg("--proto")
        .arg("=https");
    if matches!(redirects, CurlRedirects::Https) {
        cmd = cmd
            .arg("--proto-redir")
            .arg("=https")
            .arg("--location")
            .arg("--max-redirs")
            .arg(CURL_MAX_REDIRS.to_string());
    }
    cmd.arg("--connect-timeout")
        .arg(CURL_CONNECT_TIMEOUT.as_secs().to_string())
        .arg("--retry")
        .arg(CURL_RETRIES.to_string())
        .arg("--retry-connrefused")
        .arg("--max-time")
        .arg(timeout.as_secs().to_string())
        .arg("--write-out")
        .arg("%{http_code}")
        .arg("--dump-header")
        .arg(headers.as_os_str())
        .arg("--output")
        .arg(output.as_os_str())
        .arg(url)
}

fn http_status(out: &crate::host::Output) -> Option<u16> {
    out.stdout_text().trim().parse::<u16>().ok()
}

/// One download's transport policy, verification and final destination.
/// The caller creates the parent directories; the final name is written only after verification.
pub struct FixedInput {
    pub url: String,
    pub file_name: String,
    pub part: PathBuf,
    pub headers: PathBuf,
    pub target: PathBuf,
    pub redirects: CurlRedirects,
    pub expected: Option<(u64, String)>,
    pub progress: bool,
}

/// Download, verify and flush one input before moving it to its final name.
pub fn fetch(
    ctx: &mut RunCtx,
    spec: &FixedInput,
    deadline: Instant,
) -> Result<u64, DownloadFailure> {
    let Some(left) = remaining(ctx.host.now(), deadline) else {
        ctx.log(LogKey::BuildTimeout, json!({}));
        return Err(timeout_failure());
    };
    let cmd = curl_cmd(&spec.url, &spec.part, &spec.headers, left, spec.redirects);
    let out = if spec.progress {
        let host = ctx.host.clone();
        let mut watch = ProgressWatch {
            ctx: &mut *ctx,
            part: spec.part.clone(),
            headers: spec.headers.clone(),
        };
        let out = host.run_watch(&cmd, DOWNLOAD_PROGRESS_INTERVAL, &mut watch);
        watch.ctx.log_cmd(&cmd, &out);
        out
    } else {
        ctx.cmd(&cmd)
    };
    if curl_timed_out(out.exit) {
        ctx.log(LogKey::BuildTimeout, json!({}));
        return Err(timeout_failure_with_output(&cmd, &out));
    }
    if spec.expected.is_some() && remaining(ctx.host.now(), deadline).is_none() {
        ctx.log(LogKey::BuildTimeout, json!({}));
        return Err(timeout_failure_with_output(&cmd, &out));
    }
    if !out.exit.ok() {
        return Err(
            request_failure(&spec.file_name, out.raw_tail(ERROR_RAW_LINES))
                .with_command(cmd.argv(), out.exit),
        );
    }
    if http_status(&out) != Some(200) {
        return Err(DownloadFailure::new(
            ErrorCode::DownloadFailed,
            DOWNLOAD_HTTP_STATUS,
            json!({"file_name": spec.file_name, "status": http_status(&out).unwrap_or(0)}),
            Some(out.stdout_text()),
        ));
    }
    let actual = ctx.host.lstat(&spec.part).map(|m| m.len()).unwrap_or(0);
    if spec.progress {
        if let Some(expected) = read_content_length(ctx, &spec.headers) {
            if expected != actual {
                return Err(checksum_failure(&spec.file_name));
            }
        }
    }
    if let Some((bytes, sha256)) = &spec.expected {
        if actual != *bytes {
            return Err(checksum_failure(&spec.file_name));
        }
        let digest = hash_fixed_input(ctx, &spec.part, deadline, &spec.file_name)?;
        if digest.size != *bytes || digest.sha256 != *sha256 {
            return Err(checksum_failure(&spec.file_name));
        }
    }
    if ctx.host.fsync_file(&spec.part).is_err() {
        return Err(request_failure(&spec.file_name, "fsync failed".into()));
    }
    if spec.expected.is_some() && remaining(ctx.host.now(), deadline).is_none() {
        ctx.log(LogKey::BuildTimeout, json!({}));
        return Err(timeout_failure());
    }
    if ctx.host.rename(&spec.part, &spec.target).is_err() {
        return Err(request_failure(&spec.file_name, "rename failed".into()));
    }
    ctx.log(
        LogKey::FileDownloaded,
        json!({"file_name": spec.file_name, "bytes": actual}),
    );
    Ok(actual)
}

/// toolkit first, then the driver.
pub fn all(ctx: &mut RunCtx, deadline: Instant) -> Result<(), DownloadFailure> {
    toolkit(ctx, deadline)?;
    driver(ctx, deadline)?;
    if community_patch(ctx) {
        community_archive(ctx, deadline)?;
    }
    Ok(())
}

/// The official-source reader and the upload download links share HTTPS/status/index validation.
pub(crate) fn read_toolkit_index(
    h: &dyn crate::host::Host,
    dl: &std::path::Path,
    host_dl: &std::path::Path,
    left: Duration,
    mut execute: impl FnMut(&Cmd) -> crate::host::Output,
) -> Result<ToolkitPick, DownloadFailure> {
    let index_path = dl.join("Packages");
    let host_index_path = host_dl.join("Packages");
    let headers = host_dl.join("Packages.headers");
    let cmd = curl_cmd(
        TOOLKIT_INDEX_URL,
        &host_index_path,
        &headers,
        left,
        CurlRedirects::Stop,
    );
    let out = execute(&cmd);
    if curl_timed_out(out.exit) {
        return Err(timeout_failure_with_output(&cmd, &out));
    }
    if !out.exit.ok() {
        return Err(request_failure("Packages", out.raw_tail(ERROR_RAW_LINES))
            .with_command(cmd.argv(), out.exit));
    }
    if http_status(&out) != Some(200) {
        return Err(DownloadFailure {
            code: ErrorCode::DownloadFailed,
            params: json!({
                "reason": {
                    "key": DOWNLOAD_HTTP_STATUS,
                    "params": {"file_name": "Packages", "status": http_status(&out)},
                },
            }),
            raw: Some(out.stdout_text()),
            command: None,
        });
    }
    let index = h.read(&index_path).unwrap_or_default();
    match pick_toolkit(&String::from_utf8_lossy(&index)) {
        Ok(p) => Ok(p),
        Err(e) => {
            let (key, params) = match e {
                PickError::IndexUnreadable => (DOWNLOAD_TOOLKIT_INDEX_UNREADABLE, json!({})),
                PickError::NoCommonVersion => (DOWNLOAD_TOOLKIT_NO_COMMON_VERSION, json!({})),
                PickError::EntryUnexpected(package) => (
                    DOWNLOAD_TOOLKIT_ENTRY_UNEXPECTED,
                    json!({"package": package}),
                ),
            };
            Err(DownloadFailure {
                code: ErrorCode::DownloadFailed,
                params: json!({"reason": {"key": key, "params": params}}),
                raw: None,
                command: None,
            })
        }
    }
}

pub fn toolkit(ctx: &mut RunCtx, deadline: Instant) -> Result<(), DownloadFailure> {
    let dl = ctx.task_dir().join("dl");
    let _ = ctx.host.create_dir(&dl, 0o755);
    let Some(left) = remaining(ctx.host.now(), deadline) else {
        ctx.log(LogKey::BuildTimeout, json!({}));
        return Err(timeout_failure());
    };
    let pick = match read_toolkit_index(ctx.host.as_ref(), &dl, &dl, left, |cmd| ctx.cmd(cmd)) {
        Ok(pick) => pick,
        Err(error) => {
            if error.code == ErrorCode::BuildTimeout {
                ctx.log(LogKey::BuildTimeout, json!({}));
            }
            return Err(error);
        }
    };
    ctx.log(LogKey::ToolkitSelected, json!({"version": pick.version}));
    if remaining(ctx.host.now(), deadline).is_none() {
        ctx.log(LogKey::BuildTimeout, json!({}));
        return Err(timeout_failure());
    }
    let driver_version = ctx.state.plan.as_ref().unwrap().version.clone();
    let driver_major = crate::facts::parse_version(&driver_version)
        .expect("validated plan version")
        .major();
    if upload::toolkit_too_old(driver_major, &pick.version) {
        let toolkit_version = pick
            .version
            .split_once('-')
            .map_or(&*pick.version, |(upstream, _)| upstream);
        return Err(DownloadFailure {
            code: ErrorCode::DownloadFailed,
            params: json!({"reason": {"key": DOWNLOAD_TOOLKIT_TOO_OLD, "params": {
                "driver_version": driver_version, "toolkit_version": toolkit_version,
            }}}),
            raw: None,
            command: None,
        });
    }
    let toolkit_dir = ctx.task_dir().join("in").join("toolkit");
    if ctx.host.create_dir(&toolkit_dir, 0o755).is_err() {
        return Err(request_failure("toolkit", "write failed".into()));
    }
    for file in &pick.files {
        let spec = FixedInput {
            url: format!(
                "https://nvidia.github.io/libnvidia-container/stable/deb/amd64/{}",
                file.filename
            ),
            file_name: file.filename.clone(),
            part: dl.join(format!("{}.deb.part", file.package)),
            headers: dl.join(format!("{}.headers", file.package)),
            target: toolkit_dir.join(format!("{}.deb", file.package)),
            redirects: CurlRedirects::Stop,
            expected: Some((file.size, file.sha256.clone())),
            progress: false,
        };
        fetch(ctx, &spec, deadline)?;
    }
    Ok(())
}

pub fn driver(ctx: &mut RunCtx, deadline: Instant) -> Result<(), DownloadFailure> {
    let version = ctx.state.plan.as_ref().unwrap().version.clone();
    let file_name = format!("NVIDIA-Linux-x86_64-{version}-no-compat32.run");
    let in_dir = ctx.task_dir().join("in");
    let _ = ctx.host.create_dir(&in_dir, 0o755);
    let dl = ctx.task_dir().join("dl");
    let _ = ctx.host.create_dir(&dl, 0o755);
    let spec = FixedInput {
        url: format!("https://download.nvidia.com/XFree86/Linux-x86_64/{version}/{file_name}"),
        file_name,
        part: in_dir.join("driver.run.part"),
        headers: dl.join("driver.headers"),
        target: in_dir.join("driver.run"),
        redirects: CurlRedirects::Stop,
        expected: community_patch(ctx).then(|| {
            (
                COMMUNITY_PATCH_RUN_BYTES,
                COMMUNITY_PATCH_RUN_SHA256.to_string(),
            )
        }),
        progress: true,
    };
    fetch(ctx, &spec, deadline)?;
    ctx.state.download = None;
    let _ = ctx.write_state();
    Ok(())
}

fn community_archive(ctx: &mut RunCtx, deadline: Instant) -> Result<(), DownloadFailure> {
    let dl = ctx.task_dir().join("dl");
    let spec = FixedInput {
        url: COMMUNITY_PATCH_ARCHIVE_URL.into(),
        file_name: COMMUNITY_PATCH_ARCHIVE_NAME.into(),
        part: dl.join(COMMUNITY_PATCH_PART_NAME),
        headers: dl.join(COMMUNITY_PATCH_HEADERS_NAME),
        target: ctx.task_dir().join("in").join(COMMUNITY_PATCH_INPUT_NAME),
        redirects: CurlRedirects::Https,
        expected: Some((
            COMMUNITY_PATCH_ARCHIVE_BYTES,
            COMMUNITY_PATCH_ARCHIVE_SHA256.into(),
        )),
        progress: false,
    };
    fetch(ctx, &spec, deadline).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "Package: libnvidia-container1\nArchitecture: amd64\nVersion: 1.20.1-1\nFilename: ./libnvidia-container1_1.20.1-1_amd64.deb\nSize: 100\nSHA256: aa\n\n\
Package: libnvidia-container-tools\nArchitecture: amd64\nVersion: 1.20.1-1\nFilename: ./libnvidia-container-tools_1.20.1-1_amd64.deb\nSize: 101\nSHA256: bb\n\n\
Package: nvidia-container-toolkit-base\nArchitecture: amd64\nVersion: 1.20.1-1\nFilename: ./nvidia-container-toolkit-base_1.20.1-1_amd64.deb\nSize: 102\nSHA256: cc\n\n\
Package: nvidia-container-toolkit\nArchitecture: amd64\nVersion: 1.20.1-1\nFilename: ./nvidia-container-toolkit_1.20.1-1_amd64.deb\nSize: 103\nSHA256: dd\n\n\
Package: libnvidia-container1\nArchitecture: amd64\nVersion: 1.19.0-1\nFilename: ./libnvidia-container1_1.19.0-1_amd64.deb\nSize: 90\nSHA256: ee\n\n\
Package: unrelated\nArchitecture: amd64\nVersion: 9.9.9\nFilename: ./unrelated_9.9.9_amd64.deb\nSize: 1\nSHA256: ff\n\n";

    #[test]
    fn picks_the_newest_common_version() {
        let pick = pick_toolkit(INDEX).unwrap();
        assert_eq!(pick.files.len(), 4);
        assert_eq!(
            pick.files[0].filename,
            "libnvidia-container1_1.20.1-1_amd64.deb"
        );
        // The selected version is the complete Debian version.
        assert_eq!(pick.version, "1.20.1-1");
        assert_eq!(pick.files[3].size, 103);
    }

    #[test]
    fn picks_one_complete_version_not_a_mixed_revision() {
        let entry = |pkg: &str, ver: &str, size: u64| {
            format!(
                "Package: {pkg}\nArchitecture: amd64\nVersion: {ver}\nFilename: ./{pkg}_{ver}_amd64.deb\nSize: {size}\nSHA256: aa\n\n"
            )
        };
        // All four packages offer 1.19.1-1; only the first also offers 1.20.1-2 while the
        // others have 1.20.1-1. The selection must be one complete version, never a revision
        // mixed from another.
        let mut index = String::new();
        for (i, name) in TOOLKIT_PACKAGES.iter().enumerate() {
            index.push_str(&entry(name, "1.19.1-1", 10));
            index.push_str(&entry(
                name,
                if i == 0 { "1.20.1-2" } else { "1.20.1-1" },
                20,
            ));
        }
        let pick = pick_toolkit(&index).unwrap();
        assert_eq!(pick.version, "1.19.1-1");
        assert!(pick.files.iter().all(|f| f.filename.contains("1.19.1-1")));
    }

    #[test]
    fn a_subsecond_remainder_does_not_start_a_command() {
        // Less than one second left counts as the deadline.
        let now = Instant::now();
        assert!(remaining(now, now + Duration::from_millis(500)).is_none());
        assert!(remaining(now, now + Duration::from_secs(2)).is_some());
        // `curl` ended by `--max-time` is a build timeout, whatever the host reported.
        assert!(curl_timed_out(crate::host::Exit::TimedOut));
        assert!(curl_timed_out(crate::host::Exit::Code(28)));
        assert!(!curl_timed_out(crate::host::Exit::Code(1)));
    }

    #[test]
    fn rejects_unexpected_entries() {
        let bad = INDEX.replace(
            "./libnvidia-container1_1.20.1-1_amd64.deb",
            "./other_1.20.1-1_amd64.deb",
        );
        assert!(matches!(
            pick_toolkit(&bad),
            Err(PickError::EntryUnexpected(_))
        ));
        assert_eq!(
            pick_toolkit("Package: x\nArchitecture: amd64\nVersion: 1.2.3-1\nFilename: ./x_1.2.3-1_amd64.deb\nSize: 1\nSHA256: a\n\n"),
            Err(PickError::IndexUnreadable)
        );
        let entry = |pkg: &str, ver: &str, size: u64| {
            format!(
                "Package: {pkg}\nArchitecture: amd64\nVersion: {ver}\nFilename: ./{pkg}_{ver}_amd64.deb\nSize: {size}\nSHA256: aa\n\n"
            )
        };
        let no_common = TOOLKIT_PACKAGES
            .iter()
            .enumerate()
            .map(|(i, p)| entry(p, &format!("1.19.{i}"), 10))
            .collect::<String>();
        assert_eq!(pick_toolkit(&no_common), Err(PickError::NoCommonVersion));
    }
}

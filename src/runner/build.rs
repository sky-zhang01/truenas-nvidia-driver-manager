//! The build container and artifact delivery.

use std::ffi::OsString;
use std::time::Duration;

use serde_json::{json, Value};

use crate::codes::ErrorCode;
use crate::codes::*;
use crate::facts::{self, EntryKind, ImageEntry, Reading};
use crate::host::{At, Cmd, Exit, Stream, Watch};
use crate::runner::build_env::{self, BuildEnv};
use crate::runner::RunCtx;
use crate::store;
use crate::task_state::RowKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildFailure {
    pub code: ErrorCode,
    pub params: Value,
    pub raw: Option<String>,
    /// The host command that failed, for `error.command`/`error.exit`.
    pub command: Option<(Vec<String>, Exit)>,
}

impl BuildFailure {
    /// A build-stage failure with an explicit error code and the `{key, params}` reason
    /// object. The code decides which failure the
    /// interface shows.
    pub(crate) fn new(
        code: ErrorCode,
        reason: &str,
        params: Value,
        raw: Option<String>,
    ) -> BuildFailure {
        BuildFailure {
            code,
            params: json!({"reason": {"key": reason, "params": params}}),
            raw,
            command: None,
        }
    }

    pub(crate) fn with_command(mut self, argv: Vec<String>, exit: Exit) -> BuildFailure {
        self.command = Some((argv, exit));
        self
    }
}

fn build_failure(reason: &str, params: Value, raw: Option<String>) -> BuildFailure {
    BuildFailure::new(ErrorCode::BuildFailed, reason, params, raw)
}

/// Record the pre-delivery failure log and build the failure.
fn check_failure(
    ctx: &RunCtx,
    key: &'static str,
    params: Value,
    raw: Option<String>,
) -> BuildFailure {
    // the log carries the real nested `{key, params}` reason, so the observed values of the
    // parametered reasons (built/current kernel, built/target version, missing program) reach
    // the interface.
    let reason = json!({"key": key, "params": params.clone()});
    ctx.log(LogKey::ArtifactCheckFailed, json!({"build_reason": reason}));
    build_failure(key, params, raw)
}

/// One machine-readable line of the build script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineLine {
    Step(String),
    Info(String, String),
    Fail(String),
}

/// Machine-readable values are bounded to prevent untrusted output from expanding state.
const MACHINE_VALUE_MAX: usize = 64;

fn value_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MACHINE_VALUE_MAX
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b':' | b'-'))
}

/// Compiler values accept a numeric GCC suffix only.
fn cc_value_ok(value: &str) -> bool {
    value
        .strip_prefix("gcc-")
        .map(|digits| {
            !digits.is_empty()
                && digits.len() <= MACHINE_VALUE_MAX
                && digits.bytes().all(|b| b.is_ascii_digit())
        })
        .unwrap_or(false)
}

/// Each INFO key has its own closed set or shape.
fn info_value_ok(key: &str, value: &str) -> bool {
    if !value_ok(value) {
        return false;
    }
    match key {
        "cc" => cc_value_ok(value),
        "install_mode" => matches!(value, "single" | "two-pass" | "userspace-make"),
        "flag_unsupported" => PROBE_FLAGS.contains(&value),
        _ => false,
    }
}

fn bounded_digits(rest: &str) -> bool {
    !rest.is_empty() && rest.len() <= MACHINE_VALUE_MAX && rest.bytes().all(|b| b.is_ascii_digit())
}

fn fail_reason_ok(reason: &str) -> bool {
    const PLAIN: [&str; 12] = [
        BUILD_RUN_CHECK_FAILED,
        BUILD_MODULES_COPY_FAILED,
        BUILD_KERNEL_GCC_UNKNOWN,
        BUILD_EXTRACT_FAILED,
        BUILD_OPTIONS_UNREADABLE,
        BUILD_KERNEL_MODULE_TYPE_UNSUPPORTED,
        BUILD_INSTALLER_FAILED,
        BUILD_INSTALLER_USERSPACE_FAILED,
        BUILD_INSTALLER_KERNEL_MODULE_FAILED,
        BUILD_DEPMOD_FAILED,
        BUILD_CAPTURE_EMPTY,
        BUILD_STAGE_COPY_FAILED,
    ];
    if PLAIN.contains(&reason) {
        return true;
    }
    if let Some(rest) = reason.strip_prefix("compiler_missing:gcc-") {
        return bounded_digits(rest);
    }
    if let Some(rest) = reason.strip_prefix("deb_extract_failed:") {
        return TOOLKIT_PACKAGES.contains(&rest);
    }
    if let Some(rest) = reason.strip_prefix("unexpected:") {
        return bounded_digits(rest);
    }
    if let Some(rest) = reason.strip_prefix("community_patch_failed:") {
        return rest == "archive" || COMMUNITY_PATCH_IDS.contains(&rest);
    }
    if let Some(rest) = reason.strip_prefix("kernel_module_missing:") {
        return facts::DRIVER_MODULES.contains(&rest);
    }
    if let Some(rest) = reason.strip_prefix("kernel_module_install_failed:") {
        return facts::DRIVER_MODULES.contains(&rest);
    }
    reason == BUILD_RELEASE_WRITE_FAILED
        || reason == BUILD_MKSQUASHFS_FAILED
        || reason == BUILD_RESULT_WRITE_FAILED
        || reason == BUILD_KERNEL_MODULE_BUILD_FAILED
}

pub fn parse_machine_line(line: &[u8]) -> Option<MachineLine> {
    let line = std::str::from_utf8(line).ok()?;
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(step) = line.strip_prefix("NVM:STEP:") {
        if BUILD_STEP_NAMES.contains(&step) {
            return Some(MachineLine::Step(step.to_string()));
        }
        return None;
    }
    if let Some(rest) = line.strip_prefix("NVM:INFO:") {
        let (key, value) = rest.split_once('=')?;
        if info_value_ok(key, value) {
            return Some(MachineLine::Info(key.to_string(), value.to_string()));
        }
        return None;
    }
    if let Some(reason) = line.strip_prefix("NVM:FAIL:") {
        if fail_reason_ok(reason) {
            return Some(MachineLine::Fail(reason.to_string()));
        }
        return None;
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildOutcome {
    Ok,
    Fail { reason: String, params: Value },
    Timeout,
}

/// Runner classification of the build container result.
pub fn classify(
    exit: Exit,
    saw_step: bool,
    last_fail: Option<&str>,
    deadline_hit: bool,
) -> BuildOutcome {
    if deadline_hit {
        return BuildOutcome::Timeout;
    }
    if exit.ok() {
        return BuildOutcome::Ok;
    }
    if let Some(reason) = last_fail {
        return BuildOutcome::Fail {
            reason: reason.to_string(),
            params: json!({}),
        };
    }
    match exit {
        Exit::Code(code) if !saw_step && matches!(code, 125..=127) => BuildOutcome::Fail {
            reason: BUILD_DOCKER_RUN_FAILED.into(),
            params: json!({}),
        },
        Exit::Code(code) => BuildOutcome::Fail {
            reason: BUILD_CONTAINER_EXIT.into(),
            params: json!({"code": code}),
        },
        Exit::Signal(signal) => BuildOutcome::Fail {
            reason: REASON_SIGNAL.into(),
            params: json!({"signal": signal}),
        },
        Exit::TimedOut => BuildOutcome::Timeout,
        Exit::SpawnFailed => BuildOutcome::Fail {
            reason: BUILD_DOCKER_RUN_FAILED.into(),
            params: json!({}),
        },
    }
}

/// The argv of with the pinned build environment of the official image instead of the program
/// image, the read-only offline package folder, and the embedded installer as the entrypoint
/// (it installs the closure and then runs the build script from the task's `in` directory).
pub fn docker_run_argv(
    id: &str,
    kernel: &str,
    host_data_dir: &std::path::Path,
    image_id: &str,
    module_type: Option<crate::catalog::KernelModuleType>,
    community_patch: bool,
) -> Vec<OsString> {
    let task_dir = host_data_dir.join("manager").join("tasks").join(id);
    let mut argv: Vec<OsString> = vec![
        "run".into(),
        "--rm".into(),
        format!("--name={}", crate::tasks::build_container_name(id)).into(),
        "--pull".into(),
        "never".into(),
        "--runtime".into(),
        "runc".into(),
        "--network".into(),
        "none".into(),
        // The build environment is amd64; naming the platform keeps a host or
        // an image store that could pick another one from picking it.
        "--platform".into(),
        crate::build_kit::BUILD_ENV_PLATFORM.into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
        "--env".into(),
        "TAR_OPTIONS=--no-same-owner".into(),
        "--env".into(),
        "LC_ALL=C".into(),
    ];
    if let Some(t) = module_type {
        argv.push("--env".into());
        argv.push(format!("NVM_KERNEL_MODULE_TYPE={}", t.as_str()).into());
    }
    if community_patch {
        argv.push("--env".into());
        argv.push(format!("NVM_COMMUNITY_PATCH={COMMUNITY_PATCH_VERSION}").into());
    }
    argv.push("--entrypoint".into());
    argv.push("/bin/bash".into());
    // The installer reads both paths from the environment so the program's
    // constants stay the single source of them.
    argv.push("--env".into());
    argv.push(format!("NVM_BUILD_KIT_LIST={}", build_env::KIT_LIST_PATH).into());
    argv.push("--env".into());
    argv.push(format!("NVM_BUILD_SCRIPT={}", build_env::BUILD_SCRIPT_PATH).into());
    argv.push("--mount".into());
    argv.push(
        format!("type=bind,src={KERNEL_HEADERS_PATH},dst={KERNEL_HEADERS_PATH},readonly").into(),
    );
    argv.push("--mount".into());
    argv.push(
        format!("type=bind,src=/boot/config-{kernel},dst=/boot/config-{kernel},readonly").into(),
    );
    argv.push("--mount".into());
    argv.push(
        format!("type=bind,src=/usr/lib/modules/{kernel},dst=/host/modules/{kernel},readonly")
            .into(),
    );
    argv.push("--mount".into());
    argv.push(
        format!(
            "type=bind,src={},dst=/in,readonly",
            task_dir.join("in").display()
        )
        .into(),
    );
    argv.push("--mount".into());
    argv.push(format!("type=bind,src={},dst=/out", task_dir.join("out").display()).into());
    // The offline package folder of the data directory; the runner creates it
    // before the build, so the daemon never has to.
    argv.push("--mount".into());
    argv.push(
        format!(
            "type=bind,src={},dst={},readonly",
            build_env::offline_dir(host_data_dir).display(),
            build_env::OFFLINE_KIT_MOUNT
        )
        .into(),
    );
    argv.push(image_id.into());
    argv.push(build_env::INSTALL_KIT_PATH.into());
    argv
}

struct DockerWatch<'a> {
    ctx: &'a mut RunCtx,
    saw_step: bool,
    last_fail: Option<String>,
    raw: Vec<String>,
}

impl<'a> Watch for DockerWatch<'a> {
    fn line(&mut self, _from: Stream, line: &[u8]) {
        match parse_machine_line(line) {
            Some(MachineLine::Step(step)) => {
                self.saw_step = true;
                self.ctx.log(LogKey::BuildStep, json!({"build_step": step}));
            }
            Some(MachineLine::Info(key, value)) => match key.as_str() {
                "cc" => {
                    self.ctx
                        .log(LogKey::BuildCompiler, json!({"compiler": value}));
                }
                "install_mode" => {
                    self.ctx
                        .log(LogKey::BuildInstallMode, json!({"mode": value}));
                }
                "flag_unsupported" => {
                    self.ctx
                        .log(LogKey::BuildFlagUnsupported, json!({"flag": value}));
                }
                _ => {}
            },
            Some(MachineLine::Fail(reason)) => {
                self.last_fail = Some(reason);
            }
            None => {
                self.raw.push(String::from_utf8_lossy(line).into_owned());
                if self.raw.len() > ERROR_RAW_LINES {
                    let excess = self.raw.len() - ERROR_RAW_LINES;
                    self.raw.drain(..excess);
                }
            }
        }
    }

    fn tick(&mut self) {}
}

fn raw_tail(raw: &[String]) -> String {
    raw.join("\n")
}

/// run the build container and deliver the artifact. `env` is the
/// build environment [`crate::runner::build_env::acquire`] prepared for this
/// task (both the official and the upload source go through it).
pub fn build(ctx: &mut RunCtx, env: &BuildEnv) -> Result<(), BuildFailure> {
    // The build script is part of the program now; the container reads it from
    // the task's read-only `in` directory.
    build_env::write_build_script(ctx)?;
    let plan = ctx.state.plan.as_ref().unwrap();
    let image_id = env.image_id.clone();
    let module_type = plan.kernel_module_type;
    let community_patch = crate::catalog::community_patch_470(
        matches!(&plan.source, crate::task_state::PlanSource::Official),
        &plan.version,
        &plan.kernel,
    );
    let planned_kernel = plan.kernel.clone();
    let id = ctx.id.clone();
    let data_dir = ctx.paths.root.clone();
    let kernel = match facts::kernel(ctx.host.as_ref()) {
        Reading::Value(k) => k,
        Reading::Unknown(reason) => {
            return Err(build_failure(
                BUILD_DOCKER_RUN_FAILED,
                json!({}),
                Some(format!("current kernel release: {reason}")),
            ))
        }
    };
    if community_patch && kernel != planned_kernel {
        return Err(build_failure(
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!(
                "current kernel {kernel} differs from planned kernel {planned_kernel}"
            )),
        ));
    }
    let out_dir = ctx.task_dir().join("out");
    let _ = ctx.host.create_dir(&out_dir, 0o755);
    let argv = docker_run_argv(
        &id,
        &kernel,
        &data_dir,
        &image_id,
        module_type,
        community_patch,
    );
    let deadline = ctx
        .build_deadline
        .unwrap_or_else(|| ctx.host.now() + Duration::from_secs(1800));
    let left = deadline.saturating_duration_since(ctx.host.now());
    if left.is_zero() {
        ctx.log(LogKey::BuildTimeout, json!({}));
        return Err(BuildFailure {
            code: ErrorCode::BuildTimeout,
            params: json!({}),
            raw: None,
            command: None,
        });
    }
    let mut cmd = Cmd::new(At::Host, "docker", left);
    cmd.args = argv;
    ctx.log(
        LogKey::BuildContainer,
        json!({"container": crate::tasks::build_container_name(&id)}),
    );
    let mut watch = DockerWatch {
        ctx,
        saw_step: false,
        last_fail: None,
        raw: Vec::new(),
    };
    let host = watch.ctx.host.clone();
    let out = host.run_watch(&cmd, Duration::from_secs(1), &mut watch);
    let out_exit = out.exit;
    let out_argv = cmd.argv();
    let ctx = watch.ctx;
    ctx.log(
        LogKey::Cmd,
        json!({
            "argv": cmd.argv(),
            "exit": out.exit.to_json(),
            "ms": out.elapsed.as_millis() as u64,
        }),
    );
    let deadline_hit = out_exit == Exit::TimedOut || ctx.host.now() >= deadline;
    match classify(
        out_exit,
        watch.saw_step,
        watch.last_fail.as_deref(),
        deadline_hit,
    ) {
        BuildOutcome::Ok => {}
        BuildOutcome::Timeout => {
            let _ = host.run(
                &Cmd::new(At::Host, "docker", HOST_CMD_TIMEOUT)
                    .arg("rm")
                    .arg("-f")
                    .arg(crate::tasks::build_container_name(&id)),
            );
            ctx.log(LogKey::BuildTimeout, json!({}));
            return Err(BuildFailure {
                code: ErrorCode::BuildTimeout,
                params: json!({}),
                raw: Some(raw_tail(&watch.raw)),
                command: Some((out_argv, out_exit)),
            });
        }
        BuildOutcome::Fail { reason, params } => {
            return Err(build_failure(&reason, params, Some(raw_tail(&watch.raw)))
                .with_command(out_argv, out_exit));
        }
    }
    let artifact = check_artifact(ctx, &kernel)?;
    deliver(ctx, &artifact)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub sha256: String,
    pub size: u64,
    pub version: String,
    pub target_kernel: String,
}

/// The `result.txt` of exactly two lines, `ARTIFACT_SHA256=<64 lowercase hex>` and
/// `ARTIFACT_BYTES=<decimal>`, each exactly once, with no other line. Anything else is a
/// format error.
fn read_result_txt(ctx: &RunCtx) -> Option<(String, u64)> {
    let bytes = ctx
        .host
        .read(&ctx.task_dir().join("out").join("result.txt"))
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    parse_result_txt(&text)
}

/// The exact `result.txt` shape.
fn parse_result_txt(text: &str) -> Option<(String, u64)> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != 2 {
        return None;
    }
    let mut sha: Option<String> = None;
    let mut bytes_count: Option<u64> = None;
    for line in lines {
        if let Some(rest) = line.strip_prefix("ARTIFACT_SHA256=") {
            if sha.is_some() {
                return None;
            }
            // The shape is exact: no whitespace around the value.
            let value = rest;
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return None;
            }
            sha = Some(value.to_string());
        } else {
            let rest = line.strip_prefix("ARTIFACT_BYTES=")?;
            if bytes_count.is_some() {
                return None;
            }
            let value = rest;
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            bytes_count = Some(value.parse::<u64>().ok()?);
        }
    }
    Some((sha?, bytes_count?))
}

const TOOLKIT_TOOLS: [(&str, &str); 3] = [
    (
        "nvidia-container-runtime",
        "usr/bin/nvidia-container-runtime",
    ),
    ("nvidia-ctk", "usr/bin/nvidia-ctk"),
    (
        "nvidia-container-runtime-hook",
        "usr/bin/nvidia-container-runtime-hook",
    ),
];

/// The `nvidia.ko` path rule.
pub fn nvidia_ko_path_ok(path: &str, kernel: &str) -> bool {
    let prefix = format!("usr/lib/modules/{kernel}/");
    let Some(rest) = path.strip_prefix(&prefix) else {
        return false;
    };
    if !rest.ends_with("nvidia.ko") || path.contains("..") {
        return false;
    }
    if path.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f) {
        return false;
    }
    path.split('/').all(|segment| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
    })
}

/// The pre-delivery inputs must be regular files. `lstat` does not follow the
/// final component, so a symlink, FIFO, directory or socket is refused here
/// instead of being followed, read or waited on. `name` is the file name the
/// failure reports.
fn regular_file(ctx: &RunCtx, path: &std::path::Path, name: &str) -> Result<(), BuildFailure> {
    if let Ok(meta) = ctx.host.lstat(path) {
        if meta.file_type().is_file() {
            return Ok(());
        }
    }
    Err(check_failure(
        ctx,
        BUILD_ARTIFACT_UNREADABLE,
        json!({}),
        Some(format!("{name} is not a regular file")),
    ))
}

/// The four pre-delivery checks. `kernel` is the validated current release used for this
/// build's Docker mounts; a running unit cannot survive a host reboot and resume with another
/// release.
pub fn check_artifact(ctx: &mut RunCtx, kernel: &str) -> Result<Artifact, BuildFailure> {
    if !facts::kernel_ok(kernel) {
        return Err(build_failure(
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!(
                "current kernel release: {REASON_FORMAT_UNRECOGNIZED}"
            )),
        ));
    }
    let out_result = ctx.task_dir().join("out").join("result.txt");
    let out_raw = ctx.task_dir().join("out").join("nvidia.raw");
    regular_file(ctx, &out_result, "result.txt")?;
    regular_file(ctx, &out_raw, "nvidia.raw")?;
    let recorded = read_result_txt(ctx);
    let Some((recorded_sha, recorded_bytes)) = recorded else {
        return Err(check_failure(
            ctx,
            BUILD_ARTIFACT_UNREADABLE,
            json!({}),
            None,
        ));
    };
    let copied = match store::hash_file(ctx.host.as_ref(), &out_raw) {
        Ok(c) => c,
        Err(e) => {
            return Err(check_failure(
                ctx,
                BUILD_ARTIFACT_UNREADABLE,
                json!({}),
                Some(e.to_string()),
            ))
        }
    };
    if copied.sha256 != recorded_sha || copied.size != recorded_bytes {
        return Err(check_failure(
            ctx,
            BUILD_ARTIFACT_SHA_MISMATCH,
            json!({}),
            None,
        ));
    }

    // listing.
    let listing_cmd = Cmd::new(At::Host, "unsquashfs", HOST_CMD_TIMEOUT)
        .arg("-lls")
        .arg(out_raw.as_os_str());
    let listing = ctx.cmd(&listing_cmd);
    if !listing.exit.ok() {
        return Err(check_failure(
            ctx,
            BUILD_ARTIFACT_UNREADABLE,
            json!({}),
            Some(listing.stderr_text()),
        )
        .with_command(listing_cmd.argv(), listing.exit));
    }
    let entries = match facts::parse_unsquashfs_lls(&listing.stdout_text()) {
        Ok(e) => e,
        Err(e) => {
            return Err(check_failure(
                ctx,
                BUILD_ARTIFACT_UNREADABLE,
                json!({}),
                Some(e.to_string()),
            ))
        }
    };
    let facts = facts::image_facts(&entries);
    let built_kernel = match &facts.target_kernel {
        Reading::Value(k) => k.clone(),
        Reading::Unknown(_) => {
            return Err(check_failure(
                ctx,
                BUILD_ARTIFACT_KERNEL_AMBIGUOUS,
                json!({}),
                None,
            ))
        }
    };
    if built_kernel != kernel {
        return Err(check_failure(
            ctx,
            BUILD_ARTIFACT_KERNEL_MISMATCH,
            json!({"built_kernel": built_kernel, "current_kernel": kernel}),
            None,
        ));
    }

    // Third check: the driver version inside the image and in nvidia.ko.
    let plan_version = ctx
        .state
        .plan
        .as_ref()
        .map(|p| p.version.clone())
        .unwrap_or_default();
    let file_version = match &facts.file_version {
        Reading::Value(v) => v.clone(),
        Reading::Unknown(_) => {
            return Err(check_failure(
                ctx,
                BUILD_ARTIFACT_VERSION_UNREADABLE,
                json!({}),
                None,
            ))
        }
    };
    let prefix = format!("usr/lib/modules/{built_kernel}/");
    let ko_candidates: Vec<&ImageEntry> = entries
        .iter()
        .filter(|e| {
            e.kind == EntryKind::File
                && e.path.starts_with(&prefix)
                && e.path.ends_with("/nvidia.ko")
        })
        .collect();
    if ko_candidates.len() != 1 {
        return Err(check_failure(
            ctx,
            BUILD_ARTIFACT_VERSION_UNREADABLE,
            json!({}),
            None,
        ));
    }
    let ko_path = &ko_candidates[0].path;
    if !nvidia_ko_path_ok(ko_path, &built_kernel) {
        return Err(check_failure(
            ctx,
            BUILD_ARTIFACT_VERSION_UNREADABLE,
            json!({}),
            None,
        ));
    }
    if file_version != plan_version {
        return Err(check_failure(
            ctx,
            BUILD_ARTIFACT_VERSION_MISMATCH,
            json!({"built_version": file_version, "target": plan_version}),
            None,
        ));
    }
    let inspect = ctx.task_dir().join("inspect");
    let _ = ctx.host.remove_dir_all(&inspect);
    let extract_cmd = Cmd::new(At::Host, "unsquashfs", HOST_CMD_TIMEOUT)
        .arg("-no-progress")
        .arg("-no-xattrs")
        .arg("-d")
        .arg(inspect.as_os_str())
        .arg(out_raw.as_os_str())
        .arg(ko_path);
    let extract = ctx.cmd(&extract_cmd);
    if !extract.exit.ok() {
        return Err(check_failure(
            ctx,
            BUILD_ARTIFACT_VERSION_UNREADABLE,
            json!({}),
            Some(extract.stderr_text()),
        )
        .with_command(extract_cmd.argv(), extract.exit));
    }
    let ko_file = inspect.join(ko_path);
    match ctx.host.lstat(&ko_file) {
        Ok(meta) if meta.is_file() => {}
        _ => {
            return Err(check_failure(
                ctx,
                BUILD_ARTIFACT_VERSION_UNREADABLE,
                json!({}),
                None,
            ));
        }
    }
    let modinfo_cmd = Cmd::new(At::Host, "modinfo", HOST_CMD_TIMEOUT)
        .arg("-F")
        .arg("version")
        .arg(ko_file.as_os_str());
    let modinfo = ctx.cmd(&modinfo_cmd);
    if !modinfo.exit.ok() {
        return Err(
            check_failure(ctx, BUILD_ARTIFACT_VERSION_UNREADABLE, json!({}), None)
                .with_command(modinfo_cmd.argv(), modinfo.exit),
        );
    }
    let module_version = modinfo
        .stdout_text()
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if module_version != plan_version {
        return Err(check_failure(
            ctx,
            BUILD_ARTIFACT_VERSION_MISMATCH,
            json!({"built_version": module_version, "target": plan_version}),
            None,
        ));
    }

    // Fourth check: the three toolkit programs.
    for (name, path) in TOOLKIT_TOOLS {
        let ok = entries.iter().any(|e| {
            e.path == path && e.kind == EntryKind::File && e.mode.as_bytes().get(3) == Some(&b'x')
        });
        if !ok {
            return Err(check_failure(
                ctx,
                BUILD_ARTIFACT_TOOL_MISSING,
                json!({"program": name}),
                None,
            ));
        }
    }

    ctx.log(
        LogKey::ArtifactChecked,
        json!({
            "target_kernel": built_kernel,
            "version": plan_version,
        }),
    );
    let _ = ctx.write_state();
    Ok(Artifact {
        sha256: copied.sha256,
        size: copied.size,
        version: plan_version,
        target_kernel: built_kernel,
    })
}

/// Deliver the artifact as a new local driver row.
pub fn deliver(ctx: &mut RunCtx, artifact: &Artifact) -> Result<String, BuildFailure> {
    let row_dir = ctx.task_dir().join("row-build");
    if ctx.host.create_dir(&row_dir, 0o755).is_err() {
        return Err(build_failure(
            BUILD_ARTIFACT_DELIVER_FAILED,
            json!({}),
            None,
        ));
    }
    if ctx
        .host
        .rename(
            &ctx.task_dir().join("out").join("nvidia.raw"),
            &row_dir.join("nvidia.raw"),
        )
        .is_err()
    {
        return Err(build_failure(
            BUILD_ARTIFACT_DELIVER_FAILED,
            json!({}),
            None,
        ));
    }
    // The artifact was written by the build container, not through this process: flush the
    // file itself before the row directory is published.
    if ctx.host.fsync_file(&row_dir.join("nvidia.raw")).is_err() {
        return Err(build_failure(
            BUILD_ARTIFACT_DELIVER_FAILED,
            json!({}),
            None,
        ));
    }
    let plan = ctx.state.plan.as_ref().unwrap();
    let (kind, fingerprint) = match &plan.source {
        crate::task_state::PlanSource::Official => (RowKind::Official, None),
        crate::task_state::PlanSource::Upload { fingerprint, .. } => {
            (RowKind::Upload, Some(fingerprint.clone()))
        }
        crate::task_state::PlanSource::Local { .. } => (RowKind::Official, None),
    };
    let record = crate::drivers::Record {
        kind,
        version: artifact.version.clone(),
        sha256: artifact.sha256.clone(),
        size: artifact.size,
        target_kernel: artifact.target_kernel.clone(),
        created_at: ctx.host.wall_ms(),
        fingerprint,
        original_of: None,
        keep: None,
    };
    let bytes = match serde_json::to_vec_pretty(&record) {
        Ok(b) => b,
        Err(_) => {
            return Err(build_failure(
                BUILD_ARTIFACT_DELIVER_FAILED,
                json!({}),
                None,
            ))
        }
    };
    if store::write_atomic(
        ctx.host.as_ref(),
        &row_dir.join("record.json"),
        &bytes,
        0o644,
    )
    .is_err()
    {
        return Err(build_failure(
            BUILD_ARTIFACT_DELIVER_FAILED,
            json!({}),
            None,
        ));
    }
    let row_id = format!("{}-build", ctx.id);
    if let Err(e) = crate::drivers::publish(ctx.host.as_ref(), &ctx.paths, &row_dir, &row_id) {
        return Err(build_failure(
            BUILD_ARTIFACT_DELIVER_FAILED,
            json!({}),
            Some(e.to_string()),
        ));
    }
    ctx.log(
        LogKey::RowCreated,
        json!({
            "version": artifact.version,
            // the line shows the published row's own source; an upload row also carries the
            // fingerprint of that row.
            "source": json!(record.kind),
            "fingerprint": record.fingerprint,
        }),
    );
    Ok(row_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_argv_matches_design() {
        let argv = docker_run_argv(
            "20260928T101530Z-3f9a0c12",
            "6.12.105-production+truenas",
            std::path::Path::new("/mnt/<pool>/data"),
            &format!("sha256:{}", "a".repeat(64)),
            Some(crate::catalog::KernelModuleType::Open),
            false,
        );
        let text: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(text[0], "run");
        assert!(text.contains(&"--rm".into()));
        assert!(text.contains(&"--name=nvm-build-20260928T101530Z-3f9a0c12".into()));
        assert!(text.contains(&"--pull".into()) && text.contains(&"never".into()));
        assert!(text.contains(&"--network".into()) && text.contains(&"none".into()));
        assert!(text.contains(&"--cap-drop".into()) && text.contains(&"ALL".into()));
        assert!(text.contains(&"no-new-privileges".into()));
        assert!(text.contains(&"TAR_OPTIONS=--no-same-owner".into()));
        assert!(text.contains(&"NVM_KERNEL_MODULE_TYPE=open".into()));
        assert!(!text.iter().any(|a| a == concat!("--", "collect")));
        // The pinned build environment: amd64, the embedded installer as the
        // entrypoint, and the offline package folder read-only.
        assert!(text.contains(&"--platform".into()) && text.contains(&"linux/amd64".into()));
        let entrypoint = text.iter().position(|a| a == "--entrypoint").unwrap();
        assert_eq!(text[entrypoint + 1], "/bin/bash");
        assert_eq!(text.last().unwrap(), "/in/install-build-kit.sh");
        assert_eq!(text[text.len() - 2], format!("sha256:{}", "a".repeat(64)));
        assert!(text.contains(&"NVM_BUILD_KIT_LIST=/in/build-kit.list".into()));
        assert!(text.contains(&"NVM_BUILD_SCRIPT=/in/build-driver.sh".into()));
        assert!(text.contains(
            &"type=bind,src=/mnt/<pool>/data/manager/build-kit,dst=/build-kit,readonly".to_string()
        ));
        assert!(text.contains(
            &"type=bind,src=/mnt/<pool>/data/manager/tasks/20260928T101530Z-3f9a0c12/in,dst=/in,readonly".to_string()
        ));
        assert!(text.contains(
            &"type=bind,src=/mnt/<pool>/data/manager/tasks/20260928T101530Z-3f9a0c12/out,dst=/out"
                .to_string()
        ));
        let no_type = docker_run_argv(
            "20260928T101530Z-3f9a0c12",
            "k",
            std::path::Path::new("/d"),
            "sha256:x",
            None,
            true,
        );
        let text: Vec<String> = no_type
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!text
            .iter()
            .any(|a| a.starts_with("NVM_KERNEL_MODULE_TYPE=")));
        assert!(text.contains(&"NVM_COMMUNITY_PATCH=470.256.02".to_string()));
    }

    #[test]
    fn machine_lines_are_closed_sets() {
        assert_eq!(
            parse_machine_line(b"NVM:STEP:check"),
            Some(MachineLine::Step("check".into()))
        );
        assert_eq!(
            parse_machine_line(b"NVM:INFO:cc=gcc-12"),
            Some(MachineLine::Info("cc".into(), "gcc-12".into()))
        );
        assert_eq!(
            parse_machine_line(b"NVM:INFO:install_mode=single"),
            Some(MachineLine::Info("install_mode".into(), "single".into()))
        );
        assert_eq!(
            parse_machine_line(b"NVM:FAIL:installer_failed"),
            Some(MachineLine::Fail("installer_failed".into()))
        );
        assert_eq!(
            parse_machine_line(b"NVM:FAIL:compiler_missing:gcc-99"),
            Some(MachineLine::Fail("compiler_missing:gcc-99".into()))
        );
        assert_eq!(parse_machine_line(b"NVM:STEP:not_a_step"), None);
        assert_eq!(parse_machine_line(b"NVM:FAIL:whatever"), None);
        assert_eq!(parse_machine_line(b"plain output"), None);
        assert_eq!(
            parse_machine_line(b"NVM:STEP:community_patch"),
            Some(MachineLine::Step("community_patch".into()))
        );
        assert_eq!(
            parse_machine_line(b"NVM:INFO:install_mode=userspace-make"),
            Some(MachineLine::Info(
                "install_mode".into(),
                "userspace-make".into()
            ))
        );
        for key in [
            "community_patch_failed:archive",
            "community_patch_failed:0033",
            "community_patch_failed:0043",
            "community_patch_failed:0047",
            "community_patch_failed:0055",
            "kernel_module_build_failed",
            "kernel_module_missing:nvidia_uvm",
            "kernel_module_install_failed:nvidia_drm",
        ] {
            assert_eq!(
                parse_machine_line(format!("NVM:FAIL:{key}").as_bytes()),
                Some(MachineLine::Fail(key.into()))
            );
        }
        for key in [
            "community_patch_failed:",
            "community_patch_failed:0034",
            "community_patch_failed:../0033",
            "kernel_module_missing:",
            "kernel_module_missing:nvidia-uvm",
            "kernel_module_missing:nvidia_extra",
            "kernel_module_install_failed:nvidia_drm/other",
        ] {
            assert_eq!(
                parse_machine_line(format!("NVM:FAIL:{key}").as_bytes()),
                None
            );
        }
    }

    #[test]
    fn machine_info_values_follow_their_closed_sets() {
        // Reject unknown compiler values, modes outside the closed set, flags
        // outside the probe table, and FAIL values past the 64-character bound.
        assert_eq!(parse_machine_line(b"NVM:INFO:cc=unknown"), None);
        assert_eq!(parse_machine_line(b"NVM:INFO:cc=gcc-12x"), None);
        assert_eq!(
            parse_machine_line(b"NVM:INFO:install_mode=not-a-mode"),
            None
        );
        assert_eq!(
            parse_machine_line(b"NVM:INFO:flag_unsupported=--not-a-probed-flag"),
            None
        );
        let long = format!("NVM:FAIL:compiler_missing:gcc-{}", "9".repeat(100));
        assert_eq!(parse_machine_line(long.as_bytes()), None);
        let long_unexpected = format!("NVM:FAIL:unexpected:{}", "1".repeat(MACHINE_VALUE_MAX + 1));
        assert_eq!(parse_machine_line(long_unexpected.as_bytes()), None);
        // The approved boundary still parses.
        let ok_digits = "9".repeat(MACHINE_VALUE_MAX);
        assert_eq!(
            parse_machine_line(format!("NVM:FAIL:compiler_missing:gcc-{ok_digits}").as_bytes()),
            Some(MachineLine::Fail(format!(
                "compiler_missing:gcc-{ok_digits}"
            )))
        );
        assert_eq!(
            parse_machine_line(b"NVM:INFO:flag_unsupported=--skip-module-load"),
            Some(MachineLine::Info(
                "flag_unsupported".into(),
                "--skip-module-load".into()
            ))
        );
        assert_eq!(
            parse_machine_line(b"NVM:FAIL:unexpected:217"),
            Some(MachineLine::Fail("unexpected:217".into()))
        );
    }

    #[test]
    fn result_txt_takes_only_the_exact_shape() {
        let sha = "a".repeat(64);
        assert!(parse_result_txt(&format!("ARTIFACT_SHA256={sha}\nARTIFACT_BYTES=12\n")).is_some());
        // Whitespace around either value is a format error, not a value to normalize.
        assert!(
            parse_result_txt(&format!("ARTIFACT_SHA256= {sha}\nARTIFACT_BYTES=12\n")).is_none()
        );
        assert!(
            parse_result_txt(&format!("ARTIFACT_SHA256={sha} \nARTIFACT_BYTES=12\n")).is_none()
        );
        assert!(
            parse_result_txt(&format!("ARTIFACT_SHA256={sha}\nARTIFACT_BYTES= 12\n")).is_none()
        );
        assert!(
            parse_result_txt(&format!("ARTIFACT_SHA256={sha}\nARTIFACT_BYTES=12 \n")).is_none()
        );
        // Upper-case hex, a short value, a third line and a missing field all
        // stay rejected.
        assert!(parse_result_txt(&format!(
            "ARTIFACT_SHA256={}\nARTIFACT_BYTES=12\n",
            sha.to_uppercase()
        ))
        .is_none());
        assert!(parse_result_txt("ARTIFACT_SHA256=aa\nARTIFACT_BYTES=12\n").is_none());
        assert!(
            parse_result_txt(&format!("ARTIFACT_SHA256={sha}\nARTIFACT_BYTES=12\nX=1\n")).is_none()
        );
        assert!(parse_result_txt(&format!("ARTIFACT_SHA256={sha}\n")).is_none());
    }

    #[test]
    fn classify_cases() {
        assert_eq!(classify(Exit::Code(0), true, None, false), BuildOutcome::Ok);
        assert_eq!(
            classify(Exit::Code(0), true, Some(BUILD_INSTALLER_FAILED), false),
            BuildOutcome::Ok
        );
        assert_eq!(
            classify(Exit::Code(1), true, Some("installer_failed"), false),
            BuildOutcome::Fail {
                reason: "installer_failed".into(),
                params: json!({}),
            }
        );
        assert_eq!(
            classify(Exit::Code(125), false, None, false),
            BuildOutcome::Fail {
                reason: BUILD_DOCKER_RUN_FAILED.into(),
                params: json!({}),
            }
        );
        assert_eq!(
            classify(Exit::Code(137), true, None, false),
            BuildOutcome::Fail {
                reason: BUILD_CONTAINER_EXIT.into(),
                params: json!({"code": 137}),
            }
        );
        assert_eq!(
            classify(Exit::TimedOut, true, None, true),
            BuildOutcome::Timeout
        );
        assert_eq!(
            classify(Exit::Signal(9), true, None, false),
            BuildOutcome::Fail {
                reason: REASON_SIGNAL.into(),
                params: json!({"signal": 9}),
            }
        );
        assert_eq!(
            classify(Exit::Signal(15), true, Some(BUILD_INSTALLER_FAILED), false),
            BuildOutcome::Fail {
                reason: BUILD_INSTALLER_FAILED.into(),
                params: json!({}),
            }
        );
        assert_eq!(
            classify(Exit::Signal(15), true, None, true),
            BuildOutcome::Timeout
        );
        assert_eq!(
            classify(Exit::Code(15), true, None, false),
            BuildOutcome::Fail {
                reason: BUILD_CONTAINER_EXIT.into(),
                params: json!({"code": 15}),
            }
        );
        let patch = "community_patch_failed:0043";
        assert_eq!(
            classify(Exit::Signal(15), true, Some(patch), false),
            BuildOutcome::Fail {
                reason: patch.into(),
                params: json!({}),
            }
        );
        assert_eq!(
            classify(Exit::Code(0), true, Some(patch), false),
            BuildOutcome::Ok
        );
        assert_eq!(
            classify(Exit::Code(137), true, Some(patch), true),
            BuildOutcome::Timeout
        );
    }

    #[test]
    fn ko_path_rule() {
        let kernel = "6.12.105-production+truenas";
        assert!(nvidia_ko_path_ok(
            &format!("usr/lib/modules/{kernel}/kernel/drivers/video/nvidia.ko"),
            kernel
        ));
        assert!(!nvidia_ko_path_ok(
            &format!("usr/lib/modules/{kernel}/nvidia-uvm.ko"),
            kernel
        ));
        assert!(!nvidia_ko_path_ok(
            &format!("usr/lib/modules/{kernel}/../nvidia.ko"),
            kernel
        ));
        assert!(!nvidia_ko_path_ok(
            "usr/lib/modules/other/nvidia.ko",
            kernel
        ));
    }
}

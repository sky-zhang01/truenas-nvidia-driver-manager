//! The pre-delivery check of the tensor unlock files in a built image.
//!
//! Runs after `check_artifact`, on its listing and its extracted `nvidia.ko`. An image built with
//! the unlock carries the module, its rule and the firmware names the module requests; an image
//! built without it carries none of them.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::codes::*;
use crate::facts::{EntryKind, ImageEntry};
use crate::gpu_unlock::{firmware_names, module_member, rule_file, rule_member, FIRMWARE_VERSION};
use crate::host::{At, Cmd, Exit};
use crate::runner::build::{check_failure, Artifact, BuildFailure};
use crate::runner::RunCtx;

fn fail(ctx: &RunCtx, key: &'static str, raw: Option<String>) -> BuildFailure {
    check_failure(ctx, key, json!({}), raw)
}

/// `modinfo -F vermagic` of an extracted module; empty when it cannot be read.
fn vermagic(ctx: &RunCtx, file: &Path) -> String {
    let cmd = Cmd::new(At::Host, "modinfo", HOST_CMD_TIMEOUT)
        .arg("-F")
        .arg("vermagic")
        .arg(file.as_os_str());
    let out = ctx.cmd(&cmd);
    let text = if out.exit.ok() {
        out.stdout_text()
    } else {
        String::new()
    };
    text.lines().next().unwrap_or("").trim().to_string()
}

/// Check the image of this task for the unlock files its plan asks for.
pub fn check_gpu_unlock(
    ctx: &mut RunCtx,
    kernel: &str,
    artifact: &Artifact,
) -> Result<(), BuildFailure> {
    let entries = &artifact.entries;
    let plan = ctx.state.plan.as_ref();
    let targets = plan.map_or(Vec::new(), |p| p.gpu_unlock_targets.clone());
    if targets.is_empty() {
        let related =
            |e: &&ImageEntry| e.path.contains("gpu_unlock") || e.path.contains("gpu-unlock");
        return match entries.iter().find(related) {
            None => Ok(()),
            Some(e) => Err(fail(
                ctx,
                BUILD_ARTIFACT_GPU_UNLOCK_UNEXPECTED,
                Some(e.path.clone()),
            )),
        };
    }
    let modules = format!("usr/lib/modules/{kernel}");
    let module = module_member(kernel);
    let dep = format!("{modules}/modules.dep");
    let rule = rule_member();
    let file = |path: &str| {
        entries
            .iter()
            .any(|e| e.kind == EntryKind::File && e.path == path)
    };
    let named = |e: &&ImageEntry| e.kind == EntryKind::File && e.path.ends_with("/gpu_unlock.ko");
    if entries.iter().filter(named).count() != 1 || !file(&module) || !file(&dep) {
        return Err(fail(ctx, BUILD_ARTIFACT_GPU_UNLOCK_MODULE, None));
    }
    if !file(&rule) || !firmware_complete(entries) {
        return Err(fail(ctx, BUILD_ARTIFACT_GPU_UNLOCK_FILES, None));
    }
    let image = ctx.task_dir().join("out").join("nvidia.raw");
    let inspect = extract(ctx, &image, &[&module, &dep, &rule]).map_err(|failed| {
        fail(ctx, BUILD_ARTIFACT_GPU_UNLOCK_MODULE, Some(failed.raw))
            .with_command(failed.argv, failed.exit)
    })?;
    // The module loads on the kernel the image is for, as `nvidia` does, and kmod finds it
    // through the dependency index.
    let built = vermagic(ctx, &inspect.join(&module));
    let driver = vermagic(ctx, &artifact.nvidia_ko);
    let index = ctx.host.read(&inspect.join(&dep)).unwrap_or_default();
    let indexed = String::from_utf8_lossy(&index)
        .lines()
        .any(|line| line.starts_with("extra/gpu_unlock.ko:"));
    if built != driver || !built.starts_with(&format!("{kernel} ")) || !indexed {
        let raw = format!("gpu_unlock: {built}\nnvidia: {driver}");
        return Err(fail(ctx, BUILD_ARTIFACT_GPU_UNLOCK_MODULE, Some(raw)));
    }
    let text = ctx.host.read(&inspect.join(&rule)).unwrap_or_default();
    if text != rule_file(&targets).as_bytes() {
        return Err(fail(ctx, BUILD_ARTIFACT_GPU_UNLOCK_FILES, None));
    }
    Ok(())
}

/// The directory below the task directory that holds what was extracted from an image.
pub const INSPECT_DIR: &str = "inspect-unlock";

/// A host command that failed, with what the task record needs of it.
pub struct CommandFailed {
    pub argv: Vec<String>,
    pub exit: Exit,
    pub raw: String,
}

/// Extract `members` of `image` into a fresh directory of the task and return it.
pub fn extract(ctx: &RunCtx, image: &Path, members: &[&str]) -> Result<PathBuf, CommandFailed> {
    let inspect = ctx.task_dir().join(INSPECT_DIR);
    let _ = ctx.host.remove_dir_all(&inspect);
    let cmd = Cmd::new(At::Host, "unsquashfs", HOST_CMD_TIMEOUT)
        .args(["-no-progress", "-no-xattrs", "-d"])
        .arg(inspect.as_os_str())
        .arg(image.as_os_str())
        .args(members);
    let out = ctx.cmd(&cmd);
    if !out.exit.ok() {
        return Err(CommandFailed {
            argv: cmd.argv(),
            exit: out.exit,
            raw: out.stderr_text(),
        });
    }
    Ok(inspect)
}

/// The firmware directory holds exactly the files the module requests, and nothing else.
fn firmware_complete(entries: &[ImageEntry]) -> bool {
    let directory = format!("usr/lib/firmware/gpu-unlock/{FIRMWARE_VERSION}/");
    let below: Vec<&ImageEntry> = entries
        .iter()
        .filter(|e| e.path.starts_with(&directory))
        .collect();
    let mut shipped: Vec<&str> = below
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .map(|e| &e.path[directory.len()..])
        .collect();
    shipped.sort_unstable();
    let mut wanted: Vec<&str> = firmware_names().collect();
    wanted.sort_unstable();
    shipped == wanted && below.len() == wanted.len()
}

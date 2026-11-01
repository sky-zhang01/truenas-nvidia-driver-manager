//! The trial load of the unlock module.
//!
//! The kernel is the only judge of whether `gpu_unlock.ko` loads, so an image that carries the
//! rule has its module loaded once, with no parameters, before the image is written: the module
//! only registers itself and touches no card. The file that is loaded is extracted from the
//! very image that is about to be written. A module that is in the kernel already is not loaded
//! again, and a restore of the original goes on whatever the kernel says.

use std::path::Path;

use serde_json::json;

use crate::codes::*;
use crate::facts::{self, EntryKind, Reading};
use crate::gpu_unlock::artifact::{extract, CommandFailed, INSPECT_DIR};
use crate::gpu_unlock::{module_member, rule_member};
use crate::host::{At, Cmd, Exit};
use crate::runner::build::BuildFailure;
use crate::runner::RunCtx;
use crate::task_state::Phase;

/// Why the module was not loaded.
enum Fault {
    /// The image does not give the module file.
    Image {
        raw: String,
        command: Option<(Vec<String>, Exit)>,
    },
    /// The kernel refused the module.
    Refused(CommandFailed),
}

impl Fault {
    fn raw(&self) -> &str {
        match self {
            Fault::Image { raw, .. } => raw,
            Fault::Refused(failed) => &failed.raw,
        }
    }

    fn failure(self) -> BuildFailure {
        let (key, raw, command) = match self {
            Fault::Image { raw, command } => (BUILD_ARTIFACT_GPU_UNLOCK_MODULE, raw, command),
            Fault::Refused(f) => (GPU_UNLOCK_MODULE_REJECTED, f.raw, Some((f.argv, f.exit))),
        };
        let failure = BuildFailure::new(ErrorCode::BuildFailed, key, json!({}), Some(raw));
        match command {
            Some((argv, exit)) => failure.with_command(argv, exit),
            None => failure,
        }
    }
}

/// `insmod` the module file with no parameters, unless the kernel has `gpu_unlock` already.
fn load(ctx: &RunCtx, module: &Path) -> Result<(), CommandFailed> {
    let modules = facts::proc_modules_now(ctx.host.as_ref());
    if matches!(&modules, Reading::Value(list) if list.iter().any(|m| m.name == "gpu_unlock")) {
        ctx.log(LogKey::UnlockPreloadSkipped, json!({}));
        return Ok(());
    }
    let cmd = Cmd::new(At::Host, "insmod", HOST_CMD_TIMEOUT).arg(module.as_os_str());
    let out = ctx.cmd(&cmd);
    if !out.exit.ok() {
        return Err(CommandFailed {
            argv: cmd.argv(),
            exit: out.exit,
            raw: out.raw_tail(ERROR_RAW_LINES),
        });
    }
    ctx.log(LogKey::UnlockPreloaded, json!({}));
    Ok(())
}

/// After the pre-delivery check of an image built with the unlock: the module that check
/// extracted is the one that is loaded. Delivering the local row that follows is part of the
/// build again.
pub fn after_build(ctx: &mut RunCtx, kernel: &str) -> Result<(), BuildFailure> {
    ctx.enter(Phase::UnlockPreload);
    let module = ctx.task_dir().join(INSPECT_DIR).join(module_member(kernel));
    load(ctx, &module).map_err(|failed| Fault::Refused(failed).failure())?;
    ctx.enter(Phase::Build);
    Ok(())
}

/// Whether `image` carries the rule; the error is the listing's failure.
fn carries_rule(ctx: &RunCtx, image: &Path) -> Result<bool, CommandFailed> {
    let rule = rule_member();
    let list = Cmd::new(At::Host, "unsquashfs", HOST_CMD_TIMEOUT)
        .arg("-lls")
        .arg(image.as_os_str())
        .arg(&rule);
    let out = ctx.cmd(&list);
    let entries = facts::parse_unsquashfs_lls(&out.stdout_text()).ok();
    match entries.filter(|_| out.exit.ok()) {
        Some(entries) => Ok(entries
            .iter()
            .any(|e| e.kind == EntryKind::File && e.path == rule)),
        None => Err(CommandFailed {
            argv: list.argv(),
            exit: out.exit,
            raw: out.raw_tail(ERROR_RAW_LINES),
        }),
    }
}

/// Extract the module of `image` and load it.
fn load_from(ctx: &RunCtx, image: &Path) -> Result<(), Fault> {
    let kernel = ctx.state.plan.as_ref().map_or("", |p| p.kernel.as_str());
    let member = module_member(kernel);
    let dir = extract(ctx, image, &[&member]).map_err(|f| Fault::Image {
        raw: f.raw,
        command: Some((f.argv, f.exit)),
    })?;
    let module = dir.join(&member);
    if !ctx.host.lstat(&module).is_ok_and(|m| m.is_file()) {
        let raw = format!("the image has the rule but not {member}");
        return Err(Fault::Image { raw, command: None });
    }
    load(ctx, &module).map_err(Fault::Refused)
}

/// Before the unload for a local driver row: whether its image carries the rule. The listing
/// reads the file the write step copies, so a listing that fails is the write's failure,
/// reported before anything is unloaded.
pub fn before_local(ctx: &mut RunCtx, row_id: &str) -> Result<bool, BuildFailure> {
    let image = ctx.paths.row_dir(row_id).join("nvidia.raw");
    let carries = carries_rule(ctx, &image).map_err(|f| BuildFailure {
        code: ErrorCode::WriteFailed,
        params: json!({"write_reason": {"key": WRITE_SOURCE_UNREADABLE, "params": {}}}),
        raw: Some(f.raw),
        command: Some((f.argv, f.exit)),
    })?;
    if !carries {
        return Ok(false);
    }
    ctx.enter(Phase::UnlockPreload);
    load_from(ctx, &image).map_err(Fault::failure)?;
    Ok(true)
}

/// Before the original is put back: a refusal is only logged, the restore goes on.
pub fn before_restore(ctx: &mut RunCtx, image: &Path) {
    let raw = match carries_rule(ctx, image) {
        Ok(false) => return,
        Ok(true) => match load_from(ctx, image) {
            Ok(()) => return,
            Err(fault) => fault.raw().to_string(),
        },
        Err(failed) => failed.raw,
    };
    ctx.log(LogKey::UnlockPreloadFailed, json!({"raw": raw}));
}

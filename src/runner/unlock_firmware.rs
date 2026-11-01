//! The firmware stage of an update with the tensor unlock.
//!
//! The module needs 16 NVIDIA firmware files. The stage keeps them in a cache below the data
//! directory, and the build container mounts that cache read-only. A file only reaches the
//! cache after the host has checked every file against its pinned size and SHA-256, so the
//! stage trusts nothing that the generator wrote.
//!
//! The generator runs in process on the NVIDIA installer of [`FIRMWARE_VERSION`]
//! ([`crate::gpu_unlock::firmware`]). An update to that release, downloaded or uploaded, already
//! has it as the task's `driver.run`, and the pinned outputs are the check of that file; for
//! another supported release the stage downloads the pinned installer, once: later updates find
//! the cache.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use crate::codes::*;
use crate::gpu_unlock::{firmware, FixedFile, FIRMWARE_VERSION, INSTALLER, OUTPUTS};
use crate::host::{Create, Host};
use crate::runner::build::BuildFailure;
use crate::runner::download::{self, hash_fixed_input, CurlRedirects, FixedInput};
use crate::runner::RunCtx;

/// Below the data directory: the verified firmware.
const FIRMWARE_PATH: &str = "manager/gpu-unlock/firmware";

/// The verified firmware directory; the build container mounts it read-only.
pub fn firmware_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(FIRMWARE_PATH).join(FIRMWARE_VERSION)
}

fn staging_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join(FIRMWARE_PATH)
        .join(format!("{FIRMWARE_VERSION}.new"))
}

fn firmware_failure(what: &str, error: io::Error) -> BuildFailure {
    BuildFailure::new(
        ErrorCode::BuildFailed,
        GPU_UNLOCK_FIRMWARE_FAILED,
        json!({}),
        Some(format!("{what}: {error}")),
    )
}

/// Whether `dir` holds exactly the files of `expected`, each with its pinned byte count and
/// SHA-256. One check serves the cache and the staged output; any other content, a link or a
/// missing file is a mismatch. Only running out of time is an error.
pub fn dir_matches(
    ctx: &mut RunCtx,
    dir: &Path,
    expected: &[FixedFile],
    deadline: Instant,
) -> Result<bool, BuildFailure> {
    let Ok(names) = ctx.host.read_dir(dir) else {
        return Ok(false);
    };
    let mut have: Vec<String> = names
        .iter()
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    have.sort_unstable();
    let mut want: Vec<&str> = expected.iter().map(|f| f.name).collect();
    want.sort_unstable();
    if have != want {
        return Ok(false);
    }
    for file in expected {
        let path = dir.join(file.name);
        match ctx.host.lstat(&path) {
            Ok(meta) if meta.is_file() && meta.len() == file.bytes => {}
            _ => return Ok(false),
        }
        match hash_fixed_input(ctx, &path, deadline, file.name) {
            Ok(digest) if digest.size == file.bytes && digest.sha256 == file.sha256 => {}
            Ok(_) => return Ok(false),
            Err(failure) if failure.code == ErrorCode::BuildTimeout => return Err(failure),
            Err(_) => return Ok(false),
        }
    }
    Ok(true)
}

/// Whether an update to `version` with the unlock downloads the installer. Read when the plan is
/// made, from file names only (the cache is published whole); the stage checks every digest.
pub fn downloads(h: &dyn Host, data_dir: &Path, version: &str) -> bool {
    let cache = firmware_dir(data_dir);
    let cached = OUTPUTS
        .iter()
        .all(|f| h.lstat(&cache.join(f.name)).is_ok_and(|m| m.is_file()));
    version != FIRMWARE_VERSION && !cached
}

/// Make the verified firmware available, generating it when the cache is not complete.
pub fn run(ctx: &mut RunCtx, deadline: Instant) -> Result<(), BuildFailure> {
    stage(ctx, deadline, &OUTPUTS, |ctx, staged| {
        let installer = installer(ctx, deadline, &INSTALLER)?;
        generate(ctx, &installer, staged)
    })
}

/// The cache check and the landing around a generator. `generate` writes the files into the
/// empty staging directory it is given; the stage then checks them against `outputs` and only
/// a complete, matching set replaces the cache. A test gives the pinned values and the
/// generator of its own.
pub fn stage<G>(
    ctx: &mut RunCtx,
    deadline: Instant,
    outputs: &[FixedFile],
    generate: G,
) -> Result<(), BuildFailure>
where
    G: FnOnce(&mut RunCtx, &Path) -> Result<(), BuildFailure>,
{
    let data = ctx.data_dir();
    let cache = firmware_dir(&data);
    if dir_matches(ctx, &cache, outputs, deadline)? {
        ctx.log(LogKey::UnlockFirmwareReused, json!({}));
        return Ok(());
    }
    let staged = staging_dir(&data);
    // A leftover that could not be removed is caught by the check below: only the pinned set passes.
    let _ = ctx.host.remove_dir_all(&staged);
    ctx.host
        .create_dir(&staged, 0o755)
        .map_err(|e| firmware_failure("cannot create the staging directory", e))?;
    generate(ctx, &staged)?;
    if !dir_matches(ctx, &staged, outputs, deadline)? {
        return Err(BuildFailure::new(
            ErrorCode::BuildFailed,
            GPU_UNLOCK_INSTALLER_UNUSABLE,
            json!({}),
            None,
        ));
    }
    // A cache that cannot be removed makes the rename below fail, which is reported.
    let _ = ctx.host.remove_dir_all(&cache);
    ctx.host
        .rename(&staged, &cache)
        .and_then(|()| ctx.host.fsync_dir(cache.parent().unwrap_or(&data)))
        .map_err(|e| firmware_failure("cannot publish the firmware", e))?;
    ctx.log(LogKey::UnlockFirmwareGenerated, json!({}));
    Ok(())
}

/// The installer the firmware comes from: the task's own `driver.run` when the update installs
/// [`FIRMWARE_VERSION`], otherwise the download of `pin` into the task directory. A task without
/// a plan fails before it downloads anything.
pub fn installer(
    ctx: &mut RunCtx,
    deadline: Instant,
    pin: &FixedFile,
) -> Result<PathBuf, BuildFailure> {
    let Some(plan) = ctx.state.plan.as_ref() else {
        return Err(BuildFailure::new(
            ErrorCode::BuildFailed,
            GPU_UNLOCK_FIRMWARE_FAILED,
            json!({}),
            Some("the task has no plan".into()),
        ));
    };
    if plan.version == FIRMWARE_VERSION {
        return Ok(ctx.task_dir().join("in").join("driver.run"));
    }
    let dl = ctx.task_dir().join("dl");
    let _ = ctx.host.create_dir(&dl, 0o755);
    let spec = FixedInput {
        url: format!("{CATALOG_URL}{FIRMWARE_VERSION}/{}", pin.name),
        file_name: pin.name.into(),
        part: dl.join("gpu-unlock.run.part"),
        headers: dl.join("gpu-unlock.headers"),
        target: dl.join(pin.name),
        redirects: CurlRedirects::Stop,
        expected: Some((pin.bytes, pin.sha256.into())),
        progress: true,
    };
    let fetched = download::fetch(ctx, &spec, deadline);
    ctx.state.download = None;
    let _ = ctx.write_state();
    fetched.map(|_| spec.target)
}

/// Extract the firmware from `installer` into the empty `staged` directory. An installer that is
/// not one the firmware can come from fails as unusable, any other error as a write failure.
fn generate(ctx: &mut RunCtx, installer: &Path, staged: &Path) -> Result<(), BuildFailure> {
    let host = ctx.host.clone();
    let mut output = |name: &str, data: &mut dyn Read| -> io::Result<()> {
        let mut file = host.create(&staged.join(name), Create::New, 0o644)?;
        io::copy(data, &mut file)?;
        file.sync_all()
    };
    let written = host
        .open(installer)
        .and_then(|reader| firmware::generate(reader, &mut output))
        .and_then(|()| host.fsync_dir(staged));
    written.map_err(|e| match e.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof | io::ErrorKind::NotFound => {
            let raw = Some(format!("{}: {e}", installer.display()));
            BuildFailure::new(
                ErrorCode::BuildFailed,
                GPU_UNLOCK_INSTALLER_UNUSABLE,
                json!({}),
                raw,
            )
        }
        _ => firmware_failure("cannot write the firmware", e),
    })
}

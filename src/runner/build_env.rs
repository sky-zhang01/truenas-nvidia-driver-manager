//! Preparing the build environment on the host.
//!
//! Every operation that compiles (both the official and the upload source) runs this before
//! the build container starts: it makes sure the fingerprint-pinned official Debian image is
//! on the host (pulling it only when it is missing), stages the Debian packages of the
//! manifest (the offline folder first, then the cache below the data directory, then a
//! download from the snapshot that follows https redirects and is kept only when the byte
//! count and SHA256 match; a kept download is written into that cache), and writes the
//! embedded installer and package list into the task's `in` directory. Operations that do not
//! compile (switching to a local driver, restoring the original) never touch any of it.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use crate::build_kit::{self, KitPackage};
use crate::codes::*;
use crate::host::{At, Cmd, Exit};
use crate::runner::build::BuildFailure;
use crate::runner::download::{curl_cmd, curl_timed_out, http_status, remaining, CurlRedirects};
use crate::runner::RunCtx;
use crate::store;

/// The container entrypoint runs this first: it installs the manifest packages
/// and then runs the build script.
pub const INSTALL_KIT_SCRIPT: &str = include_str!("../../docker/install-build-kit.sh");
/// The program embeds the build script and stages it in the task's read-only input mount.
pub const BUILD_DRIVER_SCRIPT: &str = include_str!("../../docker/build-driver.sh");

/// The container paths of both scripts and the package list (all below the
/// read-only `/in` mount).
pub const INSTALL_KIT_PATH: &str = "/in/install-build-kit.sh";
pub const BUILD_SCRIPT_PATH: &str = "/in/build-driver.sh";
pub const KIT_LIST_PATH: &str = "/in/build-kit.list";

/// The offline package folder below the data directory: where an offline host
/// puts the packages beforehand.
pub const OFFLINE_KIT_PATH: &str = "manager/build-kit";
/// The read-only mount point of the offline folder inside the build container.
pub const OFFLINE_KIT_MOUNT: &str = "/build-kit";
/// Verified build packages kept below the data directory. The next build uses a
/// file here only when its byte count and SHA256 match the current manifest.
/// Removing the directory leaves every other record alone; the next build
/// downloads the packages again.
pub const CACHE_KIT_PATH: &str = "manager/build-kit-cache";
/// Packages this task downloads go to `in/build-kit/` of the task directory
/// (they enter the container through the existing read-only `/in` mount and are
/// cleaned up with the task directory like every other task input).
pub const TASK_KIT_PATH: &str = "in/build-kit";
/// The container path prefix of a downloaded package.
pub const TASK_KIT_MOUNT: &str = "/in/build-kit";

/// The build environment one task prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildEnv {
    /// The local image ID the build container really runs.
    pub image_id: String,
    /// An image this task pulled is removed by this task; one that was already
    /// local (or was loaded offline) is left alone.
    pub pulled_image: bool,
    /// The container path of every package, in manifest order.
    pub kit: Vec<String>,
}

impl BuildEnv {
    /// An already prepared build environment (the three values as given).
    pub fn new(image_id: String, pulled_image: bool, kit: Vec<String>) -> BuildEnv {
        BuildEnv {
            image_id,
            pulled_image,
            kit,
        }
    }
}

/// The offline package folder below the data directory.
pub fn offline_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(OFFLINE_KIT_PATH)
}

/// The build-package cache below the data directory.
pub fn cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_KIT_PATH)
}

/// Prepare the build environment: image, packages, installer and package list.
/// On failure an image this call pulled is removed here, so the caller has
/// nothing left to undo.
pub fn acquire(ctx: &mut RunCtx, deadline: Instant) -> Result<BuildEnv, BuildFailure> {
    let packages = build_kit::packages().map_err(|error| {
        BuildFailure::new(
            ErrorCode::BuildFailed,
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!(
                "the embedded build kit manifest is unusable: {error}"
            )),
        )
    })?;
    acquire_packages(ctx, deadline, packages)
}

/// The same as [`acquire`], with the list given by the caller: a test runs the
/// same logic with a small list of its own (the 49 embedded packages have no
/// real bytes inside a test).
pub fn acquire_packages(
    ctx: &mut RunCtx,
    deadline: Instant,
    packages: &[KitPackage],
) -> Result<BuildEnv, BuildFailure> {
    let (image_id, pulled_image) = ensure_image(ctx, deadline)?;
    // Record the verified image ID rather than an unverified image reference.
    if let Some(plan) = ctx.state.plan.as_mut() {
        plan.image_id = image_id.clone();
    }
    // "Pulled by this task" goes into the task state too: the image is removed
    // when the task ends and this field is cleared again. An image that was
    // already local (or was loaded offline) is never recorded.
    ctx.state.build_env_pulled_image = pulled_image.then(|| image_id.clone());
    let _ = ctx.write_state();
    let mut env = BuildEnv {
        image_id,
        pulled_image,
        kit: Vec::new(),
    };
    if let Err(failure) = stage_packages(ctx, packages, deadline, &mut env) {
        release(ctx, &env);
        return Err(failure);
    }
    if let Err(failure) = write_installer(ctx, &env) {
        release(ctx, &env);
        return Err(failure);
    }
    Ok(env)
}

/// The build is over (success, failure and timeout alike): the official image
/// is removed when this task pulled it.
pub fn release(ctx: &mut RunCtx, env: &BuildEnv) {
    if !env.pulled_image {
        return;
    }
    let cmd = Cmd::new(At::Host, "docker", HOST_CMD_TIMEOUT)
        .arg("image")
        .arg("rm")
        .arg(&env.image_id);
    // `cmd` goes into the task log, so a failed removal keeps its exit code and
    // argv on record.
    let out = ctx.cmd(&cmd);
    if out.exit.ok() {
        // The image really is gone, so the task state forgets it; a failed
        // removal keeps the record honest.
        ctx.state.build_env_pulled_image = None;
        let _ = ctx.write_state();
    }
}

/// Stage the embedded build script in the task's input directory.
pub fn write_build_script(ctx: &RunCtx) -> Result<(), BuildFailure> {
    let in_dir = ctx.task_dir().join("in");
    ctx.host.create_dir(&in_dir, 0o755).map_err(|error| {
        BuildFailure::new(
            ErrorCode::BuildFailed,
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!("cannot create {}: {error}", in_dir.display())),
        )
    })?;
    store::write_atomic(
        ctx.host.as_ref(),
        &in_dir.join("build-driver.sh"),
        BUILD_DRIVER_SCRIPT.as_bytes(),
        0o755,
    )
    .map_err(|error| {
        BuildFailure::new(
            ErrorCode::BuildFailed,
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!("cannot write the build script: {error}")),
        )
    })
}

// ---- Image preparation -----------------------------------------------------

/// Make sure the pinned image is on the host: look it up by the pinned
/// reference and then by the two known content IDs, use the copy that is
/// already there as it is (no pull, no removal), and pull only when none of the
/// three resolves.
fn ensure_image(ctx: &mut RunCtx, deadline: Instant) -> Result<(String, bool), BuildFailure> {
    match local_image(ctx, deadline)? {
        Some(id) => Ok((id, false)),
        None => {
            pull_image(ctx, deadline)?;
            // The image was just pulled by this task: every failing path from here removes
            // that copy first and re-raises the original failure. The removal never changes
            // the reason; its command and result become one line of `raw`, and the command
            // itself still goes into the task log. An image that was already local takes the
            // branch above and is never removed.
            match pulled_image_id(ctx, deadline) {
                Ok(id) => Ok((id, true)),
                Err(failure) => Err(after_failed_pull(ctx, failure)),
            }
        }
    }
}

/// The check after the pull: the image must be findable through the same lookup
/// and carry the pinned fingerprint.
fn pulled_image_id(ctx: &mut RunCtx, deadline: Instant) -> Result<String, BuildFailure> {
    match local_image(ctx, deadline)? {
        Some(id) => Ok(id),
        None => Err(BuildFailure::new(
            ErrorCode::BuildFailed,
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!(
                "the pulled image {} cannot be inspected",
                build_kit::BUILD_ENV_IMAGE_REF
            )),
        )),
    }
}

/// The check after the pull failed: best-effort removal of the copy this task
/// just pulled, then the failure that came in goes out unchanged (code, reason
/// and command stay put; `raw` gains one note line). The removal names the pinned
/// reference, and a failed removal does not replace the original reason either.
fn after_failed_pull(ctx: &mut RunCtx, failure: BuildFailure) -> BuildFailure {
    let cmd = Cmd::new(At::Host, "docker", HOST_CMD_TIMEOUT)
        .arg("image")
        .arg("rm")
        .arg(build_kit::BUILD_ENV_IMAGE_REF);
    // The command goes into the task log: a failed removal keeps its exit code
    // and argv on record.
    let out = ctx.cmd(&cmd);
    let note = format!(
        "docker image rm {} after the failed check on the pulled image: {}",
        build_kit::BUILD_ENV_IMAGE_REF,
        out.exit.to_json()
    );
    BuildFailure {
        raw: Some(match failure.raw {
            Some(raw) => format!("{raw}\n{note}"),
            None => note,
        }),
        ..failure
    }
}

/// The local image ID must be the pinned fingerprint: a classic image store
/// reports the per-platform image ID, a containerd-backed store reports the
/// index digest, and both name the same pinned amd64 content. Anything else is
/// refused.
fn accepted_image_id(id: &str) -> bool {
    id == build_kit::BUILD_ENV_IMAGE_ID || id == build_kit::BUILD_ENV_IMAGE_INDEX_ID
}

/// The image lookup: the pinned reference first, then the two known content IDs.
/// A `docker save` / `docker load` round trip often leaves the image local under
/// its ID only, with no reference that resolves, so the IDs are what finds it.
/// A lookup that resolves to anything but the pinned fingerprint is refused
/// here, exactly like the reference lookup, and never turns into a pull.
fn local_image(ctx: &mut RunCtx, deadline: Instant) -> Result<Option<String>, BuildFailure> {
    for reference in [
        build_kit::BUILD_ENV_IMAGE_REF,
        build_kit::BUILD_ENV_IMAGE_ID,
        build_kit::BUILD_ENV_IMAGE_INDEX_ID,
    ] {
        if let Some(id) = inspect_image(ctx, deadline, reference)? {
            return if accepted_image_id(&id) {
                Ok(Some(id))
            } else {
                Err(image_id_failure(&id))
            };
        }
    }
    Ok(None)
}

fn image_id_failure(actual: &str) -> BuildFailure {
    BuildFailure::new(
        ErrorCode::BuildFailed,
        BUILD_DOCKER_RUN_FAILED,
        json!({}),
        Some(format!(
            "the local image {} has ID {actual}, expected {}",
            build_kit::BUILD_ENV_IMAGE_REF,
            build_kit::BUILD_ENV_IMAGE_ID
        )),
    )
}

/// Ask once for the local image ID of `reference`; `None` means it is not there
/// (or the daemon refused the query). An image ID works as the reference here,
/// so a copy that is local under its ID only is found too.
fn inspect_image(
    ctx: &mut RunCtx,
    deadline: Instant,
    reference: &str,
) -> Result<Option<String>, BuildFailure> {
    let Some(left) = remaining(ctx.host.now(), deadline) else {
        return Err(deadline_failure(ctx));
    };
    let cmd = Cmd::new(At::Host, "docker", left)
        .arg("image")
        .arg("inspect")
        .arg("--format")
        .arg("{{.Id}}")
        .arg(reference);
    let out = ctx.cmd(&cmd);
    if out.exit == Exit::TimedOut {
        return Err(deadline_failure_with_output(ctx, &cmd, &out));
    }
    if !out.exit.ok() {
        return Ok(None);
    }
    let text = out.stdout_text();
    let id = text.trim();
    if build_kit::image_id_ok(id) {
        Ok(Some(id.to_string()))
    } else {
        Err(BuildFailure::new(
            ErrorCode::BuildFailed,
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!("docker image inspect printed {text:?}")),
        ))
    }
}

fn pull_image(ctx: &mut RunCtx, deadline: Instant) -> Result<(), BuildFailure> {
    let Some(left) = remaining(ctx.host.now(), deadline) else {
        return Err(deadline_failure(ctx));
    };
    let cmd = Cmd::new(At::Host, "docker", left)
        .arg("pull")
        .arg("--platform")
        .arg(build_kit::BUILD_ENV_PLATFORM)
        .arg(build_kit::BUILD_ENV_IMAGE_REF);
    let out = ctx.cmd(&cmd);
    if out.exit == Exit::TimedOut {
        return Err(deadline_failure_with_output(ctx, &cmd, &out));
    }
    if !out.exit.ok() {
        return Err(BuildFailure::new(
            ErrorCode::BuildFailed,
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(out.raw_tail(ERROR_RAW_LINES)),
        )
        .with_command(cmd.argv(), out.exit));
    }
    Ok(())
}

// ---- Package staging -------------------------------------------------------

/// Stage the packages one by one. The offline folder wins. A cache file is used
/// only when its byte count and SHA256 match the manifest; otherwise it is
/// downloaded from the snapshot into the task directory and the verified bytes
/// replace the cache file. A failure while reading, pruning, deleting, copying,
/// writing or renaming the cache is logged once and does not fail the build:
/// the package is downloaded instead. A cache path that is not a real
/// directory is left unused: it is not listed, cleaned, or written. Both
/// source kinds run through this.
fn stage_packages(
    ctx: &mut RunCtx,
    packages: &[KitPackage],
    deadline: Instant,
    env: &mut BuildEnv,
) -> Result<(), BuildFailure> {
    let offline = offline_dir(&ctx.data_dir());
    // The offline folder is where the administrator drops the packages: create
    // it first so the read-only mount never makes the daemon create a
    // root-owned host directory of its own.
    ctx.host.create_dir(&offline, 0o755).map_err(|error| {
        BuildFailure::new(
            ErrorCode::BuildFailed,
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!("cannot create {}: {error}", offline.display())),
        )
    })?;
    let cache = cache_dir(&ctx.data_dir());
    let gate = open_cache(ctx, &cache, packages);
    let task_kit = ctx.task_dir().join(TASK_KIT_PATH);
    let mut task_kit_ready = false;
    for package in packages {
        if package_matches(ctx, &offline.join(&package.filename), package) {
            env.kit
                .push(format!("{OFFLINE_KIT_MOUNT}/{}", package.filename));
            continue;
        }
        if !task_kit_ready {
            ctx.host.create_dir(&task_kit, 0o755).map_err(|error| {
                BuildFailure::new(
                    ErrorCode::BuildFailed,
                    BUILD_DOCKER_RUN_FAILED,
                    json!({}),
                    Some(format!("cannot create {}: {error}", task_kit.display())),
                )
            })?;
            task_kit_ready = true;
        }
        let target = task_kit.join(&package.filename);
        if gate.hits && stage_from_cache(ctx, &cache, &target, package) {
            env.kit
                .push(format!("{TASK_KIT_MOUNT}/{}", package.filename));
            continue;
        }
        download_package(ctx, package, deadline, &target)?;
        if gate.store {
            if let Err(error) = store_cache(ctx, &target, &cache, package) {
                cache_skipped(ctx, &error);
            }
        }
        env.kit
            .push(format!("{TASK_KIT_MOUNT}/{}", package.filename));
    }
    Ok(())
}

struct CacheGate {
    /// A listed real directory may satisfy a package. False when the cache
    /// path is not a real directory, or it cannot be listed or pruned.
    hits: bool,
    /// A real directory may receive a verified download. False when the cache
    /// path is not a real directory, or that directory could not be created.
    store: bool,
}

/// One task-log line when the cache cannot be used. The build continues.
/// The reason is the same `{key, params, raw}` object used by other host I/O.
fn cache_skipped(ctx: &RunCtx, error: &std::io::Error) {
    ctx.log(
        LogKey::BuildKitCache,
        json!({
            "reason": crate::log::reason_object(
                crate::log::io_reason_key(error),
                json!({}),
                Some(&error.to_string()),
            ),
        }),
    );
}

fn open_cache(ctx: &RunCtx, cache: &Path, packages: &[KitPackage]) -> CacheGate {
    let closed = CacheGate {
        hits: false,
        store: false,
    };
    // A symlink, a regular file, or any other node is not a cache. Leave it
    // in place: do not create through it, list it, delete through it, or
    // write through it.
    if !accept_cache_root(ctx, cache, true) {
        return closed;
    }
    if let Err(error) = ctx.host.create_dir(cache, 0o755) {
        cache_skipped(ctx, &error);
        return closed;
    }
    // `create_dir` accepts a symlink to an existing directory. Confirm the
    // root again before any listing, deletion, hit, or write.
    if !accept_cache_root(ctx, cache, false) {
        return closed;
    }
    // Drop names the current manifest does not list before any package is
    // staged, including a temporary name left by an interrupted write.
    if let Err(error) = retain_manifest_packages(ctx, cache, packages) {
        cache_skipped(ctx, &error);
        return CacheGate {
            hits: false,
            store: true,
        };
    }
    CacheGate {
        hits: true,
        store: true,
    }
}

/// The cache root may be used. A missing path is acceptable only before it is
/// created. Anything that is not a real directory is logged once and refused.
fn accept_cache_root(ctx: &RunCtx, cache: &Path, allow_missing: bool) -> bool {
    match ctx.host.lstat(cache) {
        Ok(meta) if real_directory(&meta) => true,
        Err(error) if allow_missing && error.kind() == ErrorKind::NotFound => true,
        Ok(_) => {
            cache_skipped(ctx, &cache_not_directory());
            false
        }
        Err(error) => {
            cache_skipped(ctx, &error);
            false
        }
    }
}

/// `lstat`: a symlink is not a directory, even when its target is one.
fn real_directory(meta: &std::fs::Metadata) -> bool {
    meta.is_dir() && !meta.file_type().is_symlink()
}

fn cache_not_directory() -> std::io::Error {
    std::io::Error::new(ErrorKind::InvalidInput, "the cache path is not a directory")
}

/// One direct child of `dir`. `.`, `..`, and a name that contains a separator
/// are not children, so a join cannot leave `dir`.
fn direct_child(dir: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return None;
    }
    let path = dir.join(name);
    if path.parent() != Some(dir) {
        return None;
    }
    Some(path)
}

/// Delete a direct child of the cache whose name is not a current manifest
/// file. Only a regular file, or a symbolic link itself, is removed. `lstat`
/// does not follow the link, and a directory or any other node stays.
/// A matching name is left in place even when its bytes are wrong; the caller
/// overwrites that name only after a verified download.
fn retain_manifest_packages(
    ctx: &RunCtx,
    cache: &Path,
    packages: &[KitPackage],
) -> std::io::Result<()> {
    let names = ctx.host.read_dir(cache)?;
    let keep: BTreeSet<&str> = packages
        .iter()
        .map(|package| package.filename.as_str())
        .collect();
    for name in names {
        let Some(text) = name.to_str() else {
            continue;
        };
        if keep.contains(text) {
            continue;
        }
        let Some(path) = direct_child(cache, text) else {
            continue;
        };
        match ctx.host.lstat(&path) {
            Ok(meta) if deletable_cache_entry(&meta) => ctx.host.remove_file(&path)?,
            Ok(_) => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// A regular file, or the symbolic link node itself. Not the directory or
/// file the link points at.
fn deletable_cache_entry(meta: &std::fs::Metadata) -> bool {
    let kind = meta.file_type();
    kind.is_file() || kind.is_symlink()
}

/// Copy a matching cache file into the task directory. A checksum that no
/// longer matches, or a copy that fails, is a miss: the caller downloads.
fn stage_from_cache(ctx: &RunCtx, cache: &Path, target: &Path, package: &KitPackage) -> bool {
    let Some(source) = direct_child(cache, &package.filename) else {
        return false;
    };
    if !package_matches(ctx, &source, package) {
        return false;
    }
    match store_verified_copy(ctx, &source, target, package) {
        Ok(()) => true,
        Err(CopyFail::Mismatch) => false,
        Err(CopyFail::Failed(error)) => {
            cache_skipped(ctx, &error);
            false
        }
    }
}

/// Keep the verified task file in the cache under its manifest name.
fn store_cache(
    ctx: &RunCtx,
    source: &Path,
    cache: &Path,
    package: &KitPackage,
) -> std::io::Result<()> {
    let dest = direct_child(cache, &package.filename).ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            "cache name is not inside the cache directory",
        )
    })?;
    match store_verified_copy(ctx, source, &dest, package) {
        Ok(()) => Ok(()),
        Err(CopyFail::Mismatch) => Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "cached copy does not match the manifest",
        )),
        Err(CopyFail::Failed(error)) => Err(error),
    }
}

enum CopyFail {
    Mismatch,
    Failed(std::io::Error),
}

/// Write `source` to a temporary name beside `dest`, keep it only when the
/// byte count and SHA256 match, then rename it onto `dest`. A partial file
/// keeps the temporary name and is not the manifest file.
fn store_verified_copy(
    ctx: &RunCtx,
    source: &Path,
    dest: &Path,
    package: &KitPackage,
) -> Result<(), CopyFail> {
    let Some(dir) = dest.parent() else {
        return Err(CopyFail::Failed(std::io::Error::new(
            ErrorKind::InvalidInput,
            "path has no parent",
        )));
    };
    let Some(part) = direct_child(dir, &format!("{}.part", package.filename)) else {
        return Err(CopyFail::Failed(std::io::Error::new(
            ErrorKind::InvalidInput,
            "name is not inside the destination directory",
        )));
    };
    let copied = store::copy_file_verified(ctx.host.as_ref(), source, &part, 0o644)
        .map_err(CopyFail::Failed)?;
    if copied.sha256 != package.sha256 || copied.size != package.bytes {
        let _ = ctx.host.remove_file(&part);
        return Err(CopyFail::Mismatch);
    }
    if let Err(error) = ctx.host.fsync_file(&part) {
        let _ = ctx.host.remove_file(&part);
        return Err(CopyFail::Failed(error));
    }
    if let Err(error) = ctx.host.rename(&part, dest) {
        let _ = ctx.host.remove_file(&part);
        return Err(CopyFail::Failed(error));
    }
    Ok(())
}

/// A regular file whose byte count and SHA256 match the manifest row.
/// Anything else counts as absent: not a regular file, a wrong size or a
/// wrong checksum.
fn package_matches(ctx: &RunCtx, path: &Path, package: &KitPackage) -> bool {
    let Ok(meta) = ctx.host.lstat(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() != package.bytes {
        return false;
    }
    matches!(
        store::hash_file(ctx.host.as_ref(), path),
        Ok(copied) if copied.sha256 == package.sha256 && copied.size == package.bytes
    )
}

/// Download one package to its final name in the task directory. This is the
/// only download that follows https redirects, at most five times. The final
/// response must be 200, and the byte count and SHA256 must match the manifest.
fn download_package(
    ctx: &mut RunCtx,
    package: &KitPackage,
    deadline: Instant,
    target: &Path,
) -> Result<(), BuildFailure> {
    let dl = ctx.task_dir().join("dl");
    ctx.host.create_dir(&dl, 0o755).map_err(|error| {
        BuildFailure::new(
            ErrorCode::BuildFailed,
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!("cannot create {}: {error}", dl.display())),
        )
    })?;
    let part = dl.join(format!("{}.part", package.filename));
    let headers = dl.join(format!("{}.headers", package.filename));
    let Some(left) = remaining(ctx.host.now(), deadline) else {
        return Err(deadline_failure(ctx));
    };
    let cmd = curl_cmd(&package.url, &part, &headers, left, CurlRedirects::Https);
    let out = ctx.cmd(&cmd);
    if curl_timed_out(out.exit) {
        return Err(deadline_failure_with_output(ctx, &cmd, &out));
    }
    if !out.exit.ok() {
        return Err(BuildFailure::new(
            ErrorCode::DownloadFailed,
            DOWNLOAD_REQUEST_FAILED,
            json!({"file_name": package.filename}),
            Some(out.raw_tail(ERROR_RAW_LINES)),
        )
        .with_command(cmd.argv(), out.exit));
    }
    if http_status(&out) != Some(200) {
        return Err(BuildFailure::new(
            ErrorCode::DownloadFailed,
            DOWNLOAD_HTTP_STATUS,
            json!({"file_name": package.filename, "status": http_status(&out)}),
            Some(out.stdout_text()),
        ));
    }
    let copied = match store::hash_file_within(ctx.host.as_ref(), &part, deadline) {
        Ok(copied) => copied,
        Err(error) if error.kind() == ErrorKind::TimedOut => {
            return Err(deadline_failure(ctx));
        }
        Err(error) => {
            return Err(request_failure(&package.filename, error.to_string()));
        }
    };
    if remaining(ctx.host.now(), deadline).is_none() {
        return Err(deadline_failure(ctx));
    }
    if copied.sha256 != package.sha256 || copied.size != package.bytes {
        return Err(checksum_failure(&package.filename));
    }
    // Flush the package before it counts as landed.
    if ctx.host.fsync_file(&part).is_err() {
        return Err(request_failure(&package.filename, "fsync failed".into()));
    }
    if ctx.host.rename(&part, target).is_err() {
        return Err(request_failure(&package.filename, "rename failed".into()));
    }
    ctx.log(
        LogKey::FileDownloaded,
        json!({"file_name": package.filename, "bytes": package.bytes}),
    );
    Ok(())
}

/// Write the embedded installer and the package list into the task's `in`
/// directory: the container entrypoint installs exactly what the list names.
fn write_installer(ctx: &RunCtx, env: &BuildEnv) -> Result<(), BuildFailure> {
    let in_dir = ctx.task_dir().join("in");
    ctx.host.create_dir(&in_dir, 0o755).map_err(|error| {
        BuildFailure::new(
            ErrorCode::BuildFailed,
            BUILD_DOCKER_RUN_FAILED,
            json!({}),
            Some(format!("cannot create {}: {error}", in_dir.display())),
        )
    })?;
    let list = format!("{}\n", env.kit.join("\n"));
    for (name, bytes, mode) in [
        ("install-build-kit.sh", INSTALL_KIT_SCRIPT.as_bytes(), 0o755),
        ("build-kit.list", list.as_bytes(), 0o644),
    ] {
        store::write_atomic(ctx.host.as_ref(), &in_dir.join(name), bytes, mode).map_err(
            |error| {
                BuildFailure::new(
                    ErrorCode::BuildFailed,
                    BUILD_DOCKER_RUN_FAILED,
                    json!({}),
                    Some(format!("cannot write {name}: {error}")),
                )
            },
        )?;
    }
    Ok(())
}

// ---- Failure reporting -----------------------------------------------------

/// The two download failure shapes match `runner/download.rs` (the same nested
/// `{key, params}` and the same raw-output tail).
fn request_failure(file_name: &str, raw: String) -> BuildFailure {
    BuildFailure::new(
        ErrorCode::DownloadFailed,
        DOWNLOAD_REQUEST_FAILED,
        json!({"file_name": file_name}),
        Some(raw),
    )
}

fn checksum_failure(file_name: &str) -> BuildFailure {
    BuildFailure::new(
        ErrorCode::DownloadFailed,
        DOWNLOAD_CHECKSUM_MISMATCH,
        json!({"file_name": file_name}),
        None,
    )
}

/// Report the timeout when no command can start within the remaining budget.
fn deadline_failure(ctx: &RunCtx) -> BuildFailure {
    ctx.log(LogKey::BuildTimeout, json!({}));
    BuildFailure {
        code: ErrorCode::BuildTimeout,
        params: json!({}),
        raw: None,
        command: None,
    }
}

/// An external command really ran and used the deadline up: the exit and the
/// raw output travel with the failure.
fn deadline_failure_with_output(
    ctx: &RunCtx,
    cmd: &Cmd,
    out: &crate::host::Output,
) -> BuildFailure {
    ctx.log(LogKey::BuildTimeout, json!({}));
    BuildFailure {
        code: ErrorCode::BuildTimeout,
        params: json!({}),
        raw: Some(out.raw_tail(ERROR_RAW_LINES)),
        command: Some((cmd.argv(), out.exit)),
    }
}

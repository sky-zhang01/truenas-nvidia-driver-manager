//! Paths, ids, atomic writes, JSON reading and streaming hashes of the data directory.

use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::codes::*;
use crate::host::{Create, Host};

/// Root of the data directory inside the container.
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Paths {
        Paths { root: root.into() }
    }

    pub fn manager(&self) -> PathBuf {
        self.root.join("manager")
    }

    pub fn state(&self, id: &str) -> PathBuf {
        self.manager().join("state").join(format!("{id}.json"))
    }

    pub fn state_unreadable(&self, id: &str) -> PathBuf {
        self.manager()
            .join("state")
            .join(format!("{id}.json.unreadable"))
    }

    pub fn state_dir(&self) -> PathBuf {
        self.manager().join("state")
    }

    pub fn state_log(&self, id: &str) -> PathBuf {
        self.manager().join("state").join(format!("{id}.log"))
    }

    pub fn task_dir(&self, id: &str) -> PathBuf {
        self.manager().join("tasks").join(id)
    }

    pub fn row_dir(&self, row: &str) -> PathBuf {
        self.drivers().join(row)
    }

    pub fn drivers(&self) -> PathBuf {
        self.root.join("drivers")
    }

    pub fn credentials(&self) -> PathBuf {
        self.manager().join("secrets").join("web-credentials.json")
    }

    /// Revoked session macs, beside the credentials file.
    pub fn logged_out_file(&self) -> PathBuf {
        self.manager().join("secrets").join("logged-out.json")
    }

    pub fn upload(&self) -> PathBuf {
        self.manager().join("upload")
    }

    pub fn upload_group(&self, group: &str) -> PathBuf {
        self.upload().join(group)
    }

    pub fn bin(&self) -> PathBuf {
        self.manager().join("bin")
    }

    pub fn runner_bin(&self) -> PathBuf {
        self.bin().join("truenas-nvidia-driver-manager")
    }

    pub fn trash(&self) -> PathBuf {
        self.manager().join("trash")
    }

    pub fn startup_check(&self) -> PathBuf {
        self.manager().join(".startup-check")
    }
}

/// Keep secrets private (0o700); other data directories use 0o755.
pub fn prepare_dirs(h: &dyn Host, p: &Paths) -> io::Result<()> {
    h.create_dir(&p.manager().join("secrets"), 0o700)?;
    h.create_dir(&p.manager().join("state"), 0o755)?;
    h.create_dir(&p.manager().join("bin"), 0o755)?;
    h.create_dir(&p.manager().join("upload"), 0o755)?;
    h.create_dir(&p.manager().join("tasks"), 0o755)?;
    h.create_dir(&p.manager().join("trash"), 0o755)?;
    h.create_dir(&p.drivers(), 0o755)?;
    Ok(())
}

fn is_tmp_name(name: &str) -> bool {
    // `.<file>.tmp-<16 hex>` and the upload marker `.incoming-<16 hex>`.
    if let Some(rest) = name.strip_prefix('.') {
        // A retired protected copy (`.tmp-superseded-<16 hex>`) is an ordinary
        // leftover once it left the recovery namespace; test it before the
        // plain `.tmp-` shape, whose suffix would otherwise reject it.
        if let Some(pos) = rest.rfind(".tmp-superseded-") {
            let suffix = &rest[pos + ".tmp-superseded-".len()..];
            return suffix.len() == TEMP_SUFFIX_HEX
                && suffix.bytes().all(|b| b.is_ascii_hexdigit());
        }
        if let Some(pos) = rest.rfind(".tmp-") {
            let suffix = &rest[pos + 5..];
            return suffix.len() == TEMP_SUFFIX_HEX
                && suffix.bytes().all(|b| b.is_ascii_hexdigit());
        }
        if let Some(suffix) = rest.strip_prefix("incoming-") {
            return suffix.len() == TEMP_SUFFIX_HEX
                && suffix.bytes().all(|b| b.is_ascii_hexdigit());
        }
    }
    false
}

/// The protected name of the material that keeps the previous bytes of a publish whose
/// rollback could not be confirmed: `.<file>.recovery-<16 hex>`. It is deliberately not one
/// of the `.<file>.tmp-<16 hex>` names that `clean_leftovers` deletes: until the bytes are
/// back in the target, this file may be their only copy.
fn recovery_name(file_name: &str) -> String {
    format!(".{file_name}.recovery-{}", random_hex(8))
}

/// The protected recovery path of one target file name. Writers that snapshot a target before
/// replacing it use this name from the moment the copy exists, so the ordinary leftover
/// cleanup can never delete the only copy of the previous bytes.
pub fn recovery_path(dir: &Path, file_name: &str) -> PathBuf {
    dir.join(recovery_name(file_name))
}

/// The target file name of a protected recovery name, or `None`.
fn recovery_target(name: &str) -> Option<&str> {
    let rest = name.strip_prefix('.')?;
    let pos = rest.rfind(".recovery-")?;
    let target = &rest[..pos];
    let suffix = &rest[pos + ".recovery-".len()..];
    if target.is_empty()
        || suffix.len() != TEMP_SUFFIX_HEX
        || !suffix.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    Some(target)
}

/// The hidden temporary name one publish uses: `.<target file name>.tmp-<16
/// hex>`. A copy whose destination is a protected recovery name takes the
/// recovery target's own file name, so no temporary name ever nests a second
/// name inside the recovery namespace.
fn temp_name_for(final_name: &str) -> String {
    let base = recovery_target(final_name).unwrap_or(final_name);
    format!(".{base}.tmp-{}", random_hex(8))
}

/// Copy `from` to `to` through a fresh temporary name in the same directory,
/// `fsync` the file, rename it and `fsync` the directory. Shared by the upload
/// group move-back and the recovery paths so every copy has the same failure
/// handling.
pub fn copy_file_verified(h: &dyn Host, from: &Path, to: &Path, mode: u32) -> io::Result<Copied> {
    let mut src = h.open(from)?;
    let name = to
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = to.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(temp_name_for(&name));
    let result = (|| -> io::Result<Copied> {
        let mut dst = h.create(&tmp, Create::New, mode)?;
        let copied = copy_hashed(&mut src, &mut *dst, |_| ControlFlow::Continue(()))?;
        dst.sync_all()?;
        drop(dst);
        h.rename(&tmp, to)?;
        h.fsync_dir(dir)?;
        Ok(copied)
    })();
    if result.is_err() {
        let _ = h.remove_file(&tmp);
    }
    result
}

/// Keep the bytes of `source` as the protected recovery material of
/// `file_name`. The direct rename is tried first, then a verified copy, then a
/// fresh write of `bytes` when the caller still holds them. Returns the kept
/// path only when the file really exists.
pub fn protect_recovery(
    h: &dyn Host,
    dir: &Path,
    file_name: &str,
    source: &Path,
    mode: u32,
    bytes: Option<&[u8]>,
) -> Option<PathBuf> {
    let protected = dir.join(recovery_name(file_name));
    // The cheapest complete copy: one atomic rename of the source.
    if h.rename(source, &protected).is_ok() && h.lstat(&protected).is_ok() {
        let _ = h.fsync_dir(dir);
        return Some(protected);
    }
    // A verified copy through its own temporary name.
    if copy_file_verified(h, source, &protected, mode).is_ok() && h.lstat(&protected).is_ok() {
        let _ = h.fsync_dir(dir);
        return Some(protected);
    }
    // A fresh write of the bytes the caller still holds, published through a temporary
    // name and read back first: a half-written attempt stays in the temporary name and never
    // becomes a candidate.
    if let Some(bytes) = bytes {
        if write_recovery_copy(h, dir, &protected, bytes, mode).is_ok() {
            return Some(protected);
        }
    }
    None
}

/// Write and verify protected recovery bytes without syncing the directory.
/// The following publish owns the directory sync and its failure handling.
fn write_recovery_copy(
    h: &dyn Host,
    dir: &Path,
    protected: &Path,
    bytes: &[u8],
    mode: u32,
) -> io::Result<()> {
    let name = protected
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(temp_name_for(&name));
    let result = (|| -> io::Result<()> {
        write_synced(h, &tmp, bytes, mode)?;
        if h.read(&tmp)? != bytes {
            return Err(io::Error::other(
                "the recovery copy does not match the previous bytes",
            ));
        }
        h.rename(&tmp, protected)?;
        // Leave directory sync to the publish so its failure and pause point stay unchanged.
        if h.read(protected)? != bytes {
            return Err(io::Error::other(
                "the published recovery copy does not match the previous bytes",
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = h.remove_file(&tmp);
    }
    result
}

fn same_bytes(h: &dyn Host, path: &Path, want: &Copied) -> bool {
    hash_file(h, path)
        .map(|have| have.sha256 == want.sha256 && have.size == want.size)
        .unwrap_or(false)
}

/// The protected recovery material of one target, if any: the previous bytes of a publish
/// whose rollback could not be confirmed. While it exists and the target is present, those
/// bytes are the value of record.
pub fn recovery_bytes(h: &dyn Host, dir: &Path, file_name: &str) -> io::Result<Option<Vec<u8>>> {
    let names = match h.read_dir(dir) {
        Ok(names) => names,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    for name in names {
        let name = name.to_string_lossy().into_owned();
        if recovery_target(&name) == Some(file_name) {
            return Ok(Some(h.read(&dir.join(&name))?));
        }
    }
    Ok(None)
}

/// Retire the protected recovery material of `file_name` after its value was committed, so a
/// later `flush_recovery` can never put the old bytes back. Unlink first, then one rename out
/// of the recovery namespace. When both are refused, the copy is overwritten in place with
/// the committed bytes: the material then denotes exactly the committed value, so a later
/// flush is a no-op instead of a silent rollback.
pub fn retire_recovery(
    h: &dyn Host,
    dir: &Path,
    file_name: &str,
    rec: &Path,
    committed: &Path,
) -> io::Result<()> {
    if h.remove_file(rec).is_ok() {
        let _ = h.fsync_dir(dir);
        return Ok(());
    }
    let retired = dir.join(format!(".{file_name}.tmp-superseded-{}", random_hex(8)));
    if h.rename(rec, &retired).is_ok() {
        let _ = h.remove_file(&retired);
        let _ = h.fsync_dir(dir);
        return Ok(());
    }
    // Disarm: stream the committed bytes into the protected name in place.
    let mut src = h.open(committed)?;
    let mode = recovery_mode(h, committed);
    let mut dst = h.create(rec, Create::Truncate, mode)?;
    copy_hashed(&mut src, &mut *dst, |_| ControlFlow::Continue(()))?;
    dst.sync_all()?;
    drop(dst);
    let same = hash_file(h, rec)
        .map(|a| {
            hash_file(h, committed)
                .map(|b| a.sha256 == b.sha256 && a.size == b.size)
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if !same {
        return Err(io::Error::other("the protected copy could not be retired"));
    }
    let _ = h.fsync_dir(dir);
    Ok(())
}

/// Write `bytes` into an existing file in place, without any rename: the last
/// resort when unlink and rename are refused but a write still succeeds.
pub fn write_in_place(h: &dyn Host, p: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut f = h.create(p, Create::Truncate, mode)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Put the kept bytes back into `to` without any rename. Used only after the
/// rename and copy restores were refused; the caller reads the target back.
pub fn restore_in_place(h: &dyn Host, from: &Path, to: &Path, mode: u32) -> io::Result<Copied> {
    let mut src = h.open(from)?;
    let _ = h.remove_file(to);
    let mut dst = h.create(to, Create::New, mode)?;
    let copied = copy_hashed(&mut src, &mut *dst, |_| ControlFlow::Continue(()))?;
    dst.sync_all()?;
    Ok(copied)
}

/// Put the previous bytes of one target back from its protected recovery
/// material. The material is removed only after the target was read back byte
/// for byte; a failure leaves it in place.
fn flush_recovery_file(h: &dyn Host, dir: &Path, name: &str, target_name: &str) -> io::Result<()> {
    let rec = dir.join(name);
    let target = dir.join(target_name);
    let want = hash_file(h, &rec)?;
    if same_bytes(h, &target, &want) {
        // The target holds the material's bytes; the copy is redundant. Both the removal and
        // the directory sync are part of the restore, so their errors propagate instead of
        // being discarded.
        h.remove_file(&rec)?;
        h.fsync_dir(dir)?;
        return Ok(());
    }
    // Restore through one atomic rename; if it is refused, write a fresh copy
    // and rename that. The material is not consumed by the copy path.
    let consumed = h.rename(&rec, &target).is_ok();
    if !consumed {
        copy_file_verified(h, &rec, &target, recovery_mode(h, &target))?;
    }
    if !same_bytes(h, &target, &want) {
        return Err(io::Error::other(
            "the restored file does not match the kept recovery material",
        ));
    }
    if !consumed {
        h.remove_file(&rec)?;
    }
    h.fsync_dir(dir)?;
    Ok(())
}

fn flush_recovery_dir(h: &dyn Host, dir: &Path) -> io::Result<()> {
    let names = match h.read_dir(dir) {
        Ok(names) => names,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        // A directory whose listing fails is not "nothing to restore": the error must reach
        // the caller.
        Err(e) => return Err(e),
    };
    for name in names {
        let name = name.to_string_lossy().into_owned();
        if let Some(target) = recovery_target(&name) {
            flush_recovery_file(h, dir, &name, target)?;
        }
    }
    Ok(())
}

/// Apply the protected recovery material of one target, if any. Every writer that snapshots
/// the target's previous bytes calls this first, so a new value never silently supersedes the
/// only copy of the old one.
pub fn flush_recovery(h: &dyn Host, dir: &Path, file_name: &str) -> io::Result<()> {
    let names = match h.read_dir(dir) {
        Ok(names) => names,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for name in names {
        let name = name.to_string_lossy().into_owned();
        if recovery_target(&name) == Some(file_name) {
            flush_recovery_file(h, dir, &name, file_name)?;
        }
    }
    Ok(())
}

/// Remove leftovers of a previous run.
///
/// A publish whose rollback could not be confirmed keeps its previous bytes under the
/// protected recovery name; those bytes are put back (and read back) before any temporary
/// file of the same directory is deleted, and the protected material itself is never deleted
/// by this function.
pub fn clean_leftovers(h: &dyn Host, p: &Paths) -> io::Result<()> {
    let mut first_error: Option<io::Error> = None;
    for dir in [p.manager().join("secrets"), p.bin(), p.state_dir()] {
        if let Err(e) = flush_recovery_dir(h, &dir) {
            first_error.get_or_insert(e);
        }
    }
    // Empty manager/trash/.
    if let Ok(names) = h.read_dir(&p.trash()) {
        for name in names {
            let path = p.trash().join(&name);
            let _ = h.remove_dir_all(&path);
            let _ = h.remove_file(&path);
        }
    }
    // Temporary files written only by the web process.
    for dir in [p.manager().join("secrets"), p.bin()] {
        remove_tmp_files(h, &dir)?;
    }
    for base in [p.upload(), p.drivers()] {
        let children = match h.read_dir(&base) {
            Ok(children) => children,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            // A failed listing must not be read as "no leftovers": it is kept as an error of
            // this cleanup.
            Err(e) => {
                first_error.get_or_insert(e);
                continue;
            }
        };
        for child in children {
            let dir = base.join(&child);
            // A stray entry that is not a directory (a file someone left in
            // `manager/upload/`) has no recovery material or temporaries to visit and must
            // not stop the cleanup; a real child directory whose listing fails still reaches
            // the caller.
            match h.lstat(&dir) {
                Ok(meta) if !meta.is_dir() => continue,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => {
                    first_error.get_or_insert(e);
                    continue;
                }
            }
            if let Err(e) = flush_recovery_dir(h, &dir) {
                first_error.get_or_insert(e);
            }
            remove_tmp_files(h, &dir)?;
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn remove_tmp_files(h: &dyn Host, dir: &Path) -> io::Result<()> {
    let Ok(names) = h.read_dir(dir) else {
        return Ok(());
    };
    for name in names {
        let name_str = name.to_string_lossy();
        if is_tmp_name(&name_str) {
            let _ = h.remove_file(&dir.join(&name));
        }
    }
    Ok(())
}

/// `^[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}$`
pub fn task_id_ok(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 8 + 1 + 6 + 1 + 1 + 8 {
        return false;
    }
    if b[8] != b'T' || b[15] != b'Z' || b[16] != b'-' {
        return false;
    }
    for (i, c) in b.iter().enumerate() {
        if i < 8 || (9..15).contains(&i) {
            if !c.is_ascii_digit() {
                return false;
            }
        } else if i >= 17 && !is_lower_hex(*c) {
            return false;
        }
    }
    true
}

/// `^[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}-(build|original)$`
pub fn row_id_ok(s: &str) -> bool {
    if let Some(base) = s.strip_suffix("-build") {
        task_id_ok(base)
    } else if let Some(base) = s.strip_suffix("-original") {
        task_id_ok(base)
    } else {
        false
    }
}

fn is_lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// `^[0-9a-f]{32}$`
pub fn group_id_ok(s: &str) -> bool {
    s.len() == GROUP_ID_HEX && s.bytes().all(is_lower_hex)
}

/// Convert a day count since 1970-01-01 to a Gregorian civil date using Howard Hinnant's
/// algorithm.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// `YYYYMMDDTHHMMSSZ-<8 hex digits>`.
pub fn new_task_id(wall_ms: u64) -> String {
    let days = (wall_ms / 86_400_000) as i64;
    let secs = (wall_ms / 1000) % 86_400;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (secs / 3600, (secs / 60) % 60, secs % 60);
    format!(
        "{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z-{}",
        random_hex(4)
    )
}

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Decode lowercase/uppercase hex.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let b = s.as_bytes();
    for chunk in b.chunks(2) {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// `bytes` random bytes as lowercase hex.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        // No safe fallback exists if the recovery name cannot be reserved.
        panic!("the system random source failed");
    }
    hex(&buf)
}

/// Result of hashing a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    pub sha256: String,
    pub size: u64,
}

/// Stream `src` into `dst` while hashing; `on_chunk` is called with the total
/// number of bytes written after every chunk and may stop the copy.
pub fn copy_hashed<W: Write + ?Sized>(
    src: &mut dyn Read,
    dst: &mut W,
    mut on_chunk: impl FnMut(u64) -> ControlFlow<()>,
) -> io::Result<Copied> {
    let mut hasher = Sha256::new();
    let mut size: u64 = 0;
    let mut buf = vec![0u8; COPY_CHUNK_BYTES];
    loop {
        let n = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        dst.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        size += n as u64;
        if let ControlFlow::Break(()) = on_chunk(size) {
            break;
        }
    }
    dst.flush()?;
    Ok(Copied {
        sha256: hex(&hasher.finalize()),
        size,
    })
}

/// Whole-file sha256 of a host path.
pub fn hash_file(h: &dyn Host, p: &Path) -> io::Result<Copied> {
    let mut f = h.open(p)?;
    copy_hashed(&mut f, &mut io::sink(), |_| ControlFlow::Continue(()))
}

/// [`hash_file`] under a deadline. The stream is abandoned (a timeout error) once the host's
/// monotonic clock reaches `deadline`, so a call site can give the whole read the
/// `READING_TIMEOUT`.
pub fn hash_file_within(h: &dyn Host, p: &Path, deadline: Instant) -> io::Result<Copied> {
    let mut f = h.open(p)?;
    let mut timed_out = h.now() >= deadline;
    let copied = copy_hashed(&mut f, &mut io::sink(), |_| {
        if h.now() >= deadline {
            timed_out = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })?;
    // An empty file reaches EOF without a single chunk, so the per-chunk check never runs;
    // the whole read is still judged by the deadline (a read that cannot finish inside its
    // budget is not a value).
    if !timed_out && h.now() >= deadline {
        timed_out = true;
    }
    if timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the read exceeded its deadline",
        ));
    }
    Ok(copied)
}

/// The result of a failed publish that had to put the previous file back.
///
/// `restored` says whether the target is byte-identical to the snapshot taken before the
/// publish, judged by actually reading it back. When the rollback cannot be verified,
/// `recovery` names the file that still holds the previous bytes, judged by `lstat`, so the
/// caller must not claim that the old file is back and must not name a file that no longer
/// exists. That file is moved under the protected recovery name, which the startup cleanup
/// never deletes and `flush_recovery` restores.
pub struct Rollback {
    pub error: io::Error,
    pub restored: bool,
    pub recovery: Option<PathBuf>,
}

/// The mode bits of the file that is about to be replaced. The recovery copy must keep the
/// target's permissions when it is restored; if the target cannot be inspected, the most
/// restrictive mode is used.
fn recovery_mode(h: &dyn Host, target: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = h.lstat(target) {
            return meta.permissions().mode() & 0o7777;
        }
    }
    let _ = (h, target);
    0o600
}

/// Write `bytes` to the fresh hidden name `path` and flush it.
fn write_synced(h: &dyn Host, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::io::Write;
    let mut f = h.create(path, Create::New, mode)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Publish `tmp` under `target`'s name while keeping the previous bytes recoverable across a
/// failed publish.
///
/// Readers of `target` only ever see a complete old or complete new file: the previous bytes
/// are copied to a hidden sibling name *before* the single rename that switches the target,
/// so the target name is never absent while the publish is in flight, and an interruption at
/// any point leaves the old or the new file in place (never neither).
///
/// If the directory sync after the rename fails, the old bytes are put back with atomic
/// renames: first a fresh copy of them, then — if that failed — the recovery copy itself, so
/// a single refused rename does not cost the old file. A rollback that cannot be confirmed is
/// reported as `restored: false`, with the recovery file named only when it still exists on
/// disk.
pub fn publish_file(
    h: &dyn Host,
    dir: &Path,
    target: &Path,
    tmp: &Path,
    file_name: &str,
    before: Option<&[u8]>,
) -> Result<(), Rollback> {
    // A publish must not silently supersede the recovery material of an
    // earlier failed publish: put those bytes back first.
    if let Err(e) = flush_recovery(h, dir, file_name) {
        return Err(Rollback {
            error: e,
            restored: true,
            recovery: None,
        });
    }
    // Prepare the recovery copy first. A failure here touches nothing: the
    // target still holds the old bytes and no new file has been published.
    let recovery = match before {
        Some(old) => {
            // The copy is created under the protected name, not a plain `.tmp-*` name: from
            // this moment the ordinary leftover cleanup can never delete the only copy of the
            // previous bytes.
            let rec = dir.join(recovery_name(file_name));
            if let Err(e) = write_recovery_copy(h, dir, &rec, old, recovery_mode(h, target)) {
                return Err(Rollback {
                    error: e,
                    restored: true,
                    recovery: None,
                });
            }
            Some(rec)
        }
        None => None,
    };
    if let Err(e) = h.rename(tmp, target) {
        // The rename did not happen: the target still holds the old bytes.
        if let Some(rec) = &recovery {
            let _ = h.remove_file(rec);
        }
        return Err(Rollback {
            error: e,
            restored: true,
            recovery: None,
        });
    }
    match h.fsync_dir(dir) {
        Ok(()) => {
            if let Some(rec) = &recovery {
                if let Err(e) = retire_recovery(h, dir, file_name, rec, target) {
                    // The published bytes are durable but the protected copy could not be
                    // retired. It must not stay able to roll the committed value back; only
                    // when even the in-place disarm was refused is the commit not
                    // protectable. Report the publish as failed and put the previous bytes
                    // back, so the caller never sees a success whose value a later flush
                    // could silently revert.
                    let old = before.expect("a recovery copy holds previous bytes");
                    let restored = {
                        let copy = dir.join(format!(".{file_name}.tmp-{}", random_hex(8)));
                        let via_copy = write_synced(h, &copy, old, recovery_mode(h, target))
                            .is_ok()
                            && h.rename(&copy, target).is_ok()
                            && h.read(target).map(|b| b == old).unwrap_or(false);
                        let _ = h.remove_file(&copy);
                        via_copy
                            || (h.rename(rec, target).is_ok()
                                && h.read(target).map(|b| b == old).unwrap_or(false))
                    };
                    return Err(Rollback {
                        error: e,
                        restored,
                        recovery: if restored { None } else { Some(rec.clone()) },
                    });
                }
                let _ = h.fsync_dir(dir);
            }
            Ok(())
        }
        Err(e) => {
            let Some(rec) = &recovery else {
                // There was no previous file: put the target name back to
                // absent. If unlinking is refused, move the published file out
                // of the way, and read the result back either way.
                if h.remove_file(target).is_err() {
                    let hidden = dir.join(format!(".{file_name}.tmp-{}", random_hex(8)));
                    let _ = h.rename(target, &hidden);
                }
                let absent = h.lstat(target).is_err();
                return Err(Rollback {
                    error: e,
                    restored: absent,
                    recovery: None,
                });
            };
            let old = before.expect("the recovery copy is written only with previous bytes");
            // First restore path: an atomic rename of a fresh copy of the old
            // bytes. The recovery file itself is not consumed, so the report
            // below can still name it truthfully.
            let copy = dir.join(format!(".{file_name}.tmp-{}", random_hex(8)));
            let path_one = write_synced(h, &copy, old, recovery_mode(h, target)).is_ok()
                && h.rename(&copy, target).is_ok()
                && h.read(target).map(|b| b == old).unwrap_or(false);
            if path_one {
                let _ = h.remove_file(rec);
                return Err(Rollback {
                    error: e,
                    restored: true,
                    recovery: None,
                });
            }
            let _ = h.remove_file(&copy);
            // Second restore path: rename the recovery copy itself back.
            if h.rename(rec, target).is_ok() && h.read(target).map(|b| b == old).unwrap_or(false) {
                return Err(Rollback {
                    error: e,
                    restored: true,
                    recovery: None,
                });
            }
            // Neither rollback was confirmed. The recovery copy already lives under the
            // protected name that `clean_leftovers` never deletes and `flush_recovery`
            // restores.
            let kept = if h.read(rec).map(|have| have == *old).unwrap_or(false) {
                Some(rec.clone())
            } else {
                protect_recovery(h, dir, file_name, rec, recovery_mode(h, target), Some(old))
            };
            Err(Rollback {
                error: e,
                restored: false,
                recovery: kept,
            })
        }
    }
}

/// Publish complete bytes through a same-directory temporary file.
/// A failed publish may leave new bytes visible; protected recovery material is retained.
pub fn write_atomic(h: &dyn Host, p: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = p
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let file_name = p
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_string_lossy()
        .into_owned();
    // Put the recovery material of an earlier failed publish back before the
    // previous bytes are snapshotted; a failure here stops the write instead of
    // superseding the only copy of them.
    flush_recovery(h, dir, &file_name)?;
    // Snapshot the previous bytes before publishing anything; a read error is
    // not "the file does not exist" and must stop the write here.
    let before = match h.read(p) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let tmp = dir.join(format!(".{file_name}.tmp-{}", random_hex(8)));
    let write_result = (|| -> io::Result<()> {
        let mut f = h.create(&tmp, Create::New, mode)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = h.remove_file(&tmp);
        return Err(e);
    }
    publish_file(h, dir, p, &tmp, &file_name, before.as_deref()).map_err(|r| {
        let _ = h.remove_file(&tmp);
        r.error
    })
}

/// Trait implemented by every type read from a JSON file.
pub trait Validate {
    fn validate(&self) -> Result<(), String>;
}

/// Reading result of a JSON file, following the table.
#[derive(Debug)]
pub enum JsonRead<T> {
    Missing,
    DirUnreadable(io::Error),
    FileUnreadable(io::Error),
    Incomplete,
    NotJson,
    Invalid,
    Ok(T),
}

impl<T> JsonRead<T> {
    pub fn is_ok(&self) -> bool {
        matches!(self, JsonRead::Ok(_))
    }

    pub fn ok(self) -> Option<T> {
        match self {
            JsonRead::Ok(v) => Some(v),
            _ => None,
        }
    }
}

/// Read and classify a JSON file.
pub fn read_json<T: serde::de::DeserializeOwned + Validate>(h: &dyn Host, p: &Path) -> JsonRead<T> {
    let dir = p.parent();
    let bytes = match h.read(p) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(dir) = dir {
                match h.read_dir(dir) {
                    Ok(_) => return JsonRead::Missing,
                    Err(de) if de.kind() == io::ErrorKind::NotFound => return JsonRead::Missing,
                    Err(de) => return JsonRead::DirUnreadable(de),
                }
            }
            return JsonRead::Missing;
        }
        Err(e) => {
            if let Some(dir) = dir {
                if let Err(de) = h.read_dir(dir) {
                    return JsonRead::DirUnreadable(de);
                }
            }
            return JsonRead::FileUnreadable(e);
        }
    };
    classify_json(&bytes)
}

/// [`read_json`] for a file whose protected recovery material takes precedence: a publish
/// whose rollback could not be confirmed leaves the previous bytes under the protected name,
/// and those bytes are the value of record while they exist.
///
/// The fixed target is read and classified first: a read error, truncated content or an
/// invalid field of the target is reported as that failure and is never hidden by a leftover
/// recovery copy. The recovery bytes are consulted only when the fixed file itself reads back
/// as valid, so a refused rollback still cannot expose the superseded value. A target that
/// does not exist is still Missing: a deleted file is not resurrected from leftovers.
pub fn read_json_recovering<T: serde::de::DeserializeOwned + Validate>(
    h: &dyn Host,
    p: &Path,
) -> JsonRead<T> {
    let target = read_json::<T>(h, p);
    if !target.is_ok() {
        return target;
    }
    if let (Some(dir), Some(file_name)) = (p.parent(), p.file_name()) {
        let file_name = file_name.to_string_lossy().into_owned();
        match recovery_bytes(h, dir, &file_name) {
            Ok(Some(bytes)) => return classify_json(&bytes),
            Ok(None) => {}
            Err(e) => {
                if let Err(de) = h.read_dir(dir) {
                    return JsonRead::DirUnreadable(de);
                }
                return JsonRead::FileUnreadable(e);
            }
        }
    }
    target
}

fn classify_json<T: serde::de::DeserializeOwned + Validate>(bytes: &[u8]) -> JsonRead<T> {
    let parsed: Result<T, serde_json::Error> = serde_json::from_slice(bytes);
    match parsed {
        Ok(v) => match v.validate() {
            Ok(()) => JsonRead::Ok(v),
            Err(_) => JsonRead::Invalid,
        },
        Err(e) => match e.classify() {
            serde_json::error::Category::Eof => JsonRead::Incomplete,
            serde_json::error::Category::Syntax => JsonRead::NotJson,
            serde_json::error::Category::Data => JsonRead::Invalid,
            serde_json::error::Category::Io => {
                JsonRead::FileUnreadable(io::Error::other(e.to_string()))
            }
        },
    }
}

/// The state-read-error reason key of a state-file read result.
///
/// The state-read-error closed set uses the plain keys `permission_denied` and `io_error`,
/// not the generic `reason.*` phrases of the reason fields that the other APIs carry. An
/// `EACCES`/`EPERM` read is `permission_denied`; every other errno is `io_error`.
pub fn read_reason_for_state(e: &io::Error) -> &'static str {
    match e.kind() {
        io::ErrorKind::PermissionDenied => "permission_denied",
        _ => "io_error",
    }
}

/// Extract the state-read-error key of a `JsonRead` result, whatever the type.
///
/// The value is always one of the five plain keys of `missing`, `incomplete`, `invalid`,
/// `permission_denied`, `io_error`.
pub fn state_read_error_key<T>(r: &JsonRead<T>) -> Option<&'static str> {
    match r {
        JsonRead::Ok(_) => None,
        JsonRead::Missing => Some("missing"),
        JsonRead::DirUnreadable(_) => Some("io_error"),
        JsonRead::FileUnreadable(e) => Some(read_reason_for_state(e)),
        JsonRead::Incomplete => Some("incomplete"),
        JsonRead::NotJson | JsonRead::Invalid => Some("invalid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RealHost;
    use crate::test_support::TestDir;

    #[test]
    fn id_formats() {
        assert!(task_id_ok("20260928T101530Z-3f9a0c12"));
        assert!(!task_id_ok("20260928T101530Z-3f9a0c1"));
        assert!(!task_id_ok("20260928T101530-3f9a0c12"));
        assert!(!task_id_ok("20260928T101530Z-3f9a0c1g"));
        assert!(row_id_ok("20260928T101530Z-3f9a0c12-build"));
        assert!(row_id_ok("20260928T101530Z-3f9a0c12-original"));
        assert!(!row_id_ok("20260928T101530Z-3f9a0c12-other"));
        assert!(group_id_ok(&"a".repeat(32)));
        assert!(!group_id_ok(&"a".repeat(31)));
        // ids are lowercase hex only.
        assert!(!task_id_ok("20260928T101530Z-3F9A0C12"));
        assert!(!group_id_ok(&"A".repeat(32)));
        assert!(!group_id_ok(&"a".repeat(31)));
        assert!(!row_id_ok("20260928T101530Z-3F9A0C12-build"));
    }

    #[test]
    fn task_id_is_utc_and_ordered() {
        // 2026-09-28T10:15:30Z
        let ms = 1790590530000u64;
        let id = new_task_id(ms);
        assert!(id.starts_with("20260928T101530Z-"), "{id}");
        assert!(task_id_ok(&id));
        // 1970-01-01T00:00:00Z
        assert!(new_task_id(0).starts_with("19700101T000000Z-"));
        // 2026-03-01T00:00:00Z
        assert!(new_task_id(1772323200000).starts_with("20260301T000000Z-"));
    }

    #[test]
    fn hex_roundtrip() {
        let b = [0u8, 1, 15, 16, 255];
        assert_eq!(hex(&b), "00010f10ff");
        assert_eq!(unhex("00010f10ff").unwrap(), b.to_vec());
        assert!(unhex("abc").is_none());
        assert_eq!(random_hex(8).len(), 16);
    }

    #[test]
    fn copy_hashed_computes_hash_and_size_and_can_stop() {
        let mut src: &[u8] = b"hello world";
        let mut dst = Vec::new();
        let copied = copy_hashed(&mut src, &mut dst, |_| ControlFlow::Continue(())).unwrap();
        assert_eq!(copied.size, 11);
        assert_eq!(
            copied.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        // The callback fires once per chunk; a break stops before later chunks.
        let big = vec![0u8; COPY_CHUNK_BYTES * 2 + 7];
        let mut src: &[u8] = &big;
        let mut dst = Vec::new();
        let copied = copy_hashed(&mut src, &mut dst, |_n| ControlFlow::Break(())).unwrap();
        assert_eq!(copied.size, COPY_CHUNK_BYTES as u64);
    }

    #[test]
    fn write_atomic_creates_and_replaces() {
        let tmp = TestDir::new("store-test");
        let h = RealHost::on_host();
        let p = tmp.join("file.json");
        write_atomic(&h, &p, b"one", 0o644).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        write_atomic(&h, &p, b"two", 0o644).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        // No temporary file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&tmp)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[derive(serde::Deserialize)]
    struct Sample {
        a: u32,
    }

    impl Validate for Sample {
        fn validate(&self) -> Result<(), String> {
            if self.a == 0 {
                Err("a must not be zero".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn read_json_classifies_results() {
        let tmp = TestDir::new("store-read");
        let h = RealHost::on_host();
        let p = tmp.join("f.json");
        assert!(matches!(read_json::<Sample>(&h, &p), JsonRead::Missing));
        std::fs::write(&p, b"{\"a\": 1}").unwrap();
        assert!(matches!(read_json::<Sample>(&h, &p), JsonRead::Ok(_)));
        std::fs::write(&p, b"{\"a\": 0}").unwrap();
        assert!(matches!(read_json::<Sample>(&h, &p), JsonRead::Invalid));
        std::fs::write(&p, b"{\"a\":").unwrap();
        assert!(matches!(read_json::<Sample>(&h, &p), JsonRead::Incomplete));
        std::fs::write(&p, b"nope").unwrap();
        assert!(matches!(read_json::<Sample>(&h, &p), JsonRead::NotJson));
        std::fs::write(&p, b"{\"b\": 1}").unwrap();
        assert!(matches!(read_json::<Sample>(&h, &p), JsonRead::Invalid));
        let _ = h;
    }

    #[test]
    fn state_read_errors_use_plain_closed_set() {
        // the state-read-error value is one of five plain keys and never one of the generic
        // `reason.*` phrases.
        let ok = JsonRead::Ok(Sample { a: 1 });
        assert_eq!(state_read_error_key(&ok), None);
        assert_eq!(
            state_read_error_key(&JsonRead::<Sample>::Missing),
            Some("missing")
        );
        assert_eq!(
            state_read_error_key(&JsonRead::<Sample>::Incomplete),
            Some("incomplete")
        );
        assert_eq!(
            state_read_error_key(&JsonRead::<Sample>::NotJson),
            Some("invalid")
        );
        assert_eq!(
            state_read_error_key(&JsonRead::<Sample>::Invalid),
            Some("invalid")
        );
        assert_eq!(
            state_read_error_key(&JsonRead::<Sample>::FileUnreadable(io::Error::from(
                io::ErrorKind::PermissionDenied
            ))),
            Some("permission_denied")
        );
        assert_eq!(
            state_read_error_key(&JsonRead::<Sample>::FileUnreadable(io::Error::from(
                io::ErrorKind::Other
            ))),
            Some("io_error")
        );
        // A directory listing that failed is still a read error, not the
        // permission key.
        assert_eq!(
            state_read_error_key(&JsonRead::<Sample>::DirUnreadable(io::Error::from(
                io::ErrorKind::PermissionDenied
            ))),
            Some("io_error")
        );
        // The generic reason interface keeps its `reason.*` value; the state-read-error
        // boundary must not borrow it.
        assert_eq!(REASON_PERMISSION_DENIED, "reason.permission_denied");
        assert_eq!(REASON_IO_ERROR, "reason.io_error");
    }
}

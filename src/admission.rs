//! The two host-global locks of the program: the short control lock and the
//! long execution lock.
//!
//! * the control lock serializes the disk-changing part of reconciliation
//!   across processes, and every writer that launches a task takes it as well;
//! * the execution lock is held by a runner for its whole execution and
//!   cleanup, so a second executor of the same host is refused, and it is
//!   attempted by reconciliation only after that host is confirmed idle;
//! * both live in a fixed host-global directory, and the lock files are never
//!   renamed, removed or written: every process that follows the protocol
//!   competes for the same inode through `flock`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::codes::*;
use crate::host::{Host, HostLock};

/// How long a caller waits for the host-global locks by default.
pub const LOCK_WAIT: Duration = Duration::from_secs(20);
/// Poll interval of the non-blocking lock wait.
const LOCK_POLL: Duration = Duration::from_millis(20);
/// Group/other write bits a lock directory must not carry.
const DIR_WRITE_BITS: u32 = 0o022;

/// A refusal with a machine-readable reason from the closed set.
#[derive(Debug, Clone)]
pub struct Blocked {
    pub key: &'static str,
    pub params: Value,
    pub raw: Option<String>,
}

impl Blocked {
    pub fn new(key: &'static str) -> Blocked {
        Blocked {
            key,
            params: json!({}),
            raw: None,
        }
    }

    pub fn with_raw(key: &'static str, raw: impl ToString) -> Blocked {
        Blocked {
            key,
            params: json!({}),
            raw: Some(raw.to_string()),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut obj = json!({"key": self.key, "params": self.params});
        if let Some(raw) = &self.raw {
            if let Some(map) = obj.as_object_mut() {
                map.insert("raw".into(), json!(raw));
            }
        }
        obj
    }
}

// The host-global lock directory. Tests may redirect it so the real `/run` is
// never touched (the production path is a compile-time constant).
#[cfg(test)]
thread_local! {
    static LOCK_DIR_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

fn lock_dir() -> PathBuf {
    #[cfg(test)]
    {
        if let Some(dir) = LOCK_DIR_OVERRIDE.with(|d| d.borrow().clone()) {
            return dir;
        }
    }
    PathBuf::from(LOCK_DIR)
}

#[cfg(test)]
fn set_lock_dir(dir: &Path) {
    LOCK_DIR_OVERRIDE.with(|d| *d.borrow_mut() = Some(dir.to_path_buf()));
}

/// A host-global path as this process must touch it through the ordinary file
/// interface. `Host::view` maps it into the host root for a container process,
/// exactly like `Host::open_lock` does for the lock file itself: without it the
/// web process would create and read the lock directory in its own container
/// while the kernel lock is taken on the host. Data-root paths (`Paths::*`) are
/// **not** viewed: they are already paths of this process.
fn host_path(h: &dyn Host, host_global: &Path) -> PathBuf {
    h.view(host_global)
}

/// Create and validate the host-global lock directory. This runs **before** any
/// lock is opened: a missing parent must never be mistaken for a busy lock.
pub fn lock_dir_bootstrap(h: &dyn Host) -> Result<(), Blocked> {
    let dir = host_path(h, &lock_dir());
    let dir = dir.as_path();
    if let Err(e) = h.create_dir(dir, LOCK_DIR_MODE) {
        return Err(Blocked::with_raw(ADMISSION_LOCK_DIR_INVALID, e));
    }
    let meta = h
        .lstat(dir)
        .map_err(|e| Blocked::with_raw(ADMISSION_LOCK_DIR_INVALID, e))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Blocked::new(ADMISSION_LOCK_DIR_INVALID));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & DIR_WRITE_BITS != 0 {
            return Err(Blocked::new(ADMISSION_LOCK_DIR_INVALID));
        }
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(Blocked::new(ADMISSION_LOCK_DIR_INVALID));
        }
    }
    Ok(())
}

fn lock_with_deadline(
    h: &dyn Host,
    host_path: &Path,
    timeout: Duration,
) -> Result<Box<dyn HostLock + Send>, Blocked> {
    let mut lock = h
        .open_lock(host_path, PRIVATE_FILE_MODE)
        .map_err(|e| Blocked::with_raw(ADMISSION_LOCK_UNSUPPORTED, e))?;
    let deadline = h.now() + timeout;
    loop {
        match lock.try_acquire() {
            Ok(true) => return Ok(lock),
            Ok(false) => {}
            // The `flock` call itself failed: nobody proved a holder, so this
            // is an unanswerable query (`lock_unsupported`), never the
            // known-busy class a real holder produces.
            Err(e) => return Err(Blocked::with_raw(ADMISSION_LOCK_UNSUPPORTED, e)),
        }
        if h.now() >= deadline {
            return Err(Blocked::new(ADMISSION_LOCK_TIMEOUT));
        }
        std::thread::sleep(LOCK_POLL);
    }
}

/// The short control lock. It serializes the disk-changing part of
/// reconciliation and every writer that launches a task across processes.
pub fn control_lock(h: &dyn Host, timeout: Duration) -> Result<Box<dyn HostLock + Send>, Blocked> {
    lock_dir_bootstrap(h)?;
    lock_with_deadline(h, &lock_dir().join(CONTROL_LOCK_NAME), timeout)
}

/// The long execution lock. A runner holds it for its whole execution and
/// cleanup; the reconciliation attempts it only after the host is confirmed
/// idle.
pub fn execution_lock(
    h: &dyn Host,
    timeout: Duration,
) -> Result<Box<dyn HostLock + Send>, Blocked> {
    lock_dir_bootstrap(h)?;
    lock_with_deadline(h, &lock_dir().join(EXECUTION_LOCK_NAME), timeout)
}

/// One non-blocking attempt at the execution lock.
pub fn try_execution_lock(h: &dyn Host) -> Result<Option<Box<dyn HostLock + Send>>, Blocked> {
    lock_dir_bootstrap(h)?;
    let mut lock = h
        .open_lock(&lock_dir().join(EXECUTION_LOCK_NAME), PRIVATE_FILE_MODE)
        .map_err(|e| Blocked::with_raw(ADMISSION_LOCK_UNSUPPORTED, e))?;
    match lock.try_acquire() {
        Ok(true) => Ok(Some(lock)),
        Ok(false) => Ok(None),
        // Same rule for the one non-blocking attempt: a failed `flock` is an
        // unanswerable probe, not a proven holder.
        Err(e) => Err(Blocked::with_raw(ADMISSION_LOCK_UNSUPPORTED, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RealHost;
    use crate::store;
    use crate::store::Paths;
    use crate::test_support::TestDir;

    fn prepare(h: &RealHost, p: &Paths) {
        store::prepare_dirs(h, p).unwrap();
    }

    #[test]
    fn the_execution_lock_is_exclusive_within_one_process() {
        let dir = TestDir::new("locks-execution");
        set_lock_dir(&dir.join("run"));
        let h = RealHost::on_host();
        prepare(&h, &Paths::new(dir.join("data")));
        let mut first = execution_lock(&h, Duration::from_secs(1)).unwrap();
        assert!(try_execution_lock(&h).unwrap().is_none());
        first.release().unwrap();
        let mut second = try_execution_lock(&h).unwrap().expect("free after release");
        second.release().unwrap();
    }

    #[test]
    fn the_control_lock_is_exclusive_until_it_is_released() {
        let dir = TestDir::new("locks-control");
        set_lock_dir(&dir.join("run"));
        let h = RealHost::on_host();
        prepare(&h, &Paths::new(dir.join("data")));
        let mut first = control_lock(&h, Duration::from_millis(50)).unwrap();
        assert!(control_lock(&h, Duration::ZERO).is_err());
        first.release().unwrap();
        let mut second = control_lock(&h, Duration::from_millis(50)).unwrap();
        second.release().unwrap();
    }
}

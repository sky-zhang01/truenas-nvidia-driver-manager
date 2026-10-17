//! Atomic publication keeps complete bytes at the target and recoverable bytes on disk after
//! interruption. A child pauses inside `rename` or `fsync_dir`; the parent kills it and checks the
//! files.

mod common;

use common::{Decorate, Wrap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use truenas_nvidia_driver_manager::host::{Host, RealHost};
use truenas_nvidia_driver_manager::store;

const CHILD_ENV: &str = "NVM_STORE_ATOMIC_PROBE_CHILD";
const DIR_ENV: &str = "NVM_STORE_ATOMIC_PROBE_DIR";
const TEST_NAME: &str = "interrupted_publish_keeps_the_saved_file";

const OLD: &[u8] = b"{\"generation\":1,\"padding\":\"old-bytes-old-bytes-old-bytes\"}";
const NEW: &[u8] = b"{\"generation\":2,\"padding\":\"new-bytes-new-bytes-new-bytes\"}";

fn probe_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nvm-atomic-{label}-{}", store::random_hex(8)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn hidden_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with('.') && path.is_file() {
            found.push((path.clone(), std::fs::read(&path).unwrap()));
        }
    }
    found
}

/// Concurrent readers must only ever observe complete old or new bytes, never a missing or torn
/// target.
#[test]
fn concurrent_readers_only_see_complete_old_or_new_files() {
    let dir = probe_dir("concurrent");
    let target = dir.join("state.json");
    store::write_atomic(&RealHost::on_host(), &target, OLD, 0o644).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::new();
    for _ in 0..4 {
        let stop = stop.clone();
        let target = target.clone();
        readers.push(std::thread::spawn(move || {
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                match std::fs::read(&target) {
                    Ok(bytes) => {
                        assert!(
                            bytes == OLD || bytes == NEW,
                            "a reader saw a torn or unknown file of {} bytes",
                            bytes.len()
                        );
                    }
                    Err(e) => panic!("a reader saw no file at all: {e}"),
                }
                reads += 1;
            }
            reads
        }));
    }
    for i in 0..150 {
        let bytes = if i % 2 == 0 { NEW } else { OLD };
        store::write_atomic(&RealHost::on_host(), &target, bytes, 0o644).unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let mut total = 0u64;
    for reader in readers {
        total += reader.join().unwrap();
    }
    assert!(total > 0, "the readers never read the target");
    let leftovers: Vec<_> = hidden_files(&dir).into_iter().map(|(p, _)| p).collect();
    assert!(
        leftovers.is_empty(),
        "leftovers after success: {leftovers:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

struct PauseHost {
    inner: RealHost,
    mode: &'static str,
    marker: PathBuf,
    paused: AtomicBool,
}

impl PauseHost {
    fn pause(&self) {
        if self.paused.swap(true, Ordering::SeqCst) {
            return;
        }
        std::fs::write(&self.marker, b"paused").unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(5));
        }
    }
}

impl Decorate for PauseHost {
    fn inner(&self) -> &dyn Host {
        &self.inner
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.mode == "before-rename" {
            self.pause();
        }
        self.inner.rename(from, to)
    }

    fn fsync_dir(&self, p: &Path) -> io::Result<()> {
        if self.mode == "before-dir-sync" {
            self.pause();
        }
        self.inner.fsync_dir(p)
    }
}

#[cfg(unix)]
#[test]
fn interrupted_publish_keeps_the_saved_file() {
    // Child role: block inside the requested step until the parent kills us.
    if let Ok(mode) = std::env::var(CHILD_ENV) {
        let dir = PathBuf::from(std::env::var(DIR_ENV).unwrap());
        let target = dir.join("state.json");
        let host = Wrap(PauseHost {
            inner: RealHost::on_host(),
            mode: if mode == "before-rename" {
                "before-rename"
            } else {
                "before-dir-sync"
            },
            marker: dir.join("paused"),
            paused: AtomicBool::new(false),
        });
        let _ = store::write_atomic(&host, &target, NEW, 0o644);
        std::process::exit(0);
    }

    for mode in ["before-rename", "before-dir-sync"] {
        let dir = probe_dir(mode);
        let target = dir.join("state.json");
        std::fs::write(&target, OLD).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([TEST_NAME, "--exact", "--nocapture"])
            .env(CHILD_ENV, mode)
            .env(DIR_ENV, &dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let marker = dir.join("paused");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "{mode}: the child never reached the pause point"
            );
            if let Some(status) = child.try_wait().unwrap() {
                panic!("{mode}: the child exited before pausing: {status}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(100));
        // A reader in another process must see a complete file while the publish is paused: the old
        // bytes before the rename, the new bytes afterwards. Never a missing target.
        let during = std::fs::read(&target)
            .unwrap_or_else(|e| panic!("{mode}: no target while the publish is in progress: {e}"));
        if mode == "before-rename" {
            assert!(during == OLD, "{mode}: a reader did not see the old bytes");
        } else {
            assert!(during == NEW, "{mode}: a reader did not see the new bytes");
        }
        child.kill().unwrap();
        child.wait().unwrap();

        let after = std::fs::read(&target).expect("the target must still exist after SIGKILL");
        let hidden = hidden_files(&dir);
        match mode {
            "before-rename" => {
                assert!(after == OLD, "{mode}: the target changed before the rename");
                assert!(
                    hidden.iter().any(|(_, bytes)| bytes == OLD),
                    "{mode}: no hidden file kept the old bytes"
                );
                assert!(
                    hidden.iter().any(|(_, bytes)| bytes == NEW),
                    "{mode}: the prepared new file was lost"
                );
            }
            _ => {
                assert!(
                    after == NEW,
                    "{mode}: the switch did not happen before the sync"
                );
                assert!(
                    hidden.iter().any(|(_, bytes)| bytes == OLD),
                    "{mode}: the old bytes are gone after SIGKILL"
                );
            }
        }
        for (path, _) in hidden {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_file(&marker);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

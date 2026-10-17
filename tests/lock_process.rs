//! Cross-process lock tests use real kernel `flock` on a fixed host path. They require Linux and
//! writable `/run`, so they are ignored on workstations. Run serially because they share
//! `/run/nvm-admission`: `cargo test --locked --test lock_process -- --ignored --nocapture
//! --test-threads=1`. Fake-host cases run in the ordinary suite and do not prove OS locking
//! semantics.

mod common;

use common::{
    app, default_host, get, healthy_startup, login_changed, post, prime_canonical, settings,
    sha256_hex, KERNEL,
};
use serde_json::json;
use truenas_nvidia_driver_manager::host::Host;

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use truenas_nvidia_driver_manager::admission;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::host::RealHost;

/// Set in the child process; the value names the role it plays.
const CHILD_ENV: &str = "NVM_LOCK_PROBE_CHILD";
/// The exact name of the child entry below.
const TEST_NAME: &str = "lock_probe_child";

fn control_lock_path() -> PathBuf {
    Path::new(LOCK_DIR).join(CONTROL_LOCK_NAME)
}

/// The child role of every test below. A parent starts it through `current_exe`; without the
/// environment variable it does nothing, so the ordinary suite never runs it.
#[test]
#[ignore = "subprocess entry: started by the real OS lock tests"]
fn lock_probe_child() {
    let Ok(role) = std::env::var(CHILD_ENV) else {
        return;
    };
    let host = RealHost::on_host();
    match role.as_str() {
        // One acquire attempt with a short deadline.
        "contend" => match admission::control_lock(&host, Duration::from_millis(400)) {
            Ok(mut lock) => {
                let _ = lock.release();
                println!("PROBE acquired");
            }
            Err(blocked) => println!("PROBE refused {}", blocked.key),
        },
        // Hold the lock until the parent kills this process.
        "hold" => {
            let lock = admission::control_lock(&host, Duration::from_secs(5))
                .expect("the child must acquire the free lock");
            let id = lock.identity().expect("the held lock has an identity");
            println!("PROBE holding {} {}", id.dev, id.ino);
            // Keep the descriptor open; the parent kills this process.
            std::thread::sleep(Duration::from_secs(60));
        }
        other => println!("PROBE unknown-role {other}"),
    }
}

struct Drain {
    text: std::sync::Arc<std::sync::Mutex<String>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn drain(mut pipe: impl Read + Send + 'static) -> Drain {
    let text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = text.clone();
    let thread = std::thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(n) = pipe.read(&mut bytes) {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&bytes[..n]));
        }
    });
    Drain {
        text,
        thread: Some(thread),
    }
}

impl Drain {
    fn snapshot(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    fn finish(&mut self) -> String {
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
        self.snapshot()
    }
}

struct Probe {
    child: Child,
    stdout: Drain,
    stderr: Drain,
}

impl Probe {
    fn spawn(role: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                TEST_NAME,
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, role)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the probe child starts");
        // Both pipes stay open and are drained until the child exits.
        let stdout = drain(child.stdout.take().unwrap());
        let stderr = drain(child.stderr.take().unwrap());
        Self {
            child,
            stdout,
            stderr,
        }
    }

    fn wait_for_probe(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let text = self.stdout.snapshot();
            if text
                .split_inclusive('\n')
                .any(|line| line.contains("PROBE ") && line.ends_with('\n'))
            {
                return text;
            }
            if self.child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                self.stop();
                let final_text = self.stdout.snapshot();
                if final_text
                    .split_inclusive('\n')
                    .any(|line| line.contains("PROBE ") && line.ends_with('\n'))
                {
                    return final_text;
                }
                panic!(
                    "no complete PROBE line\nstdout: {}\nstderr: {}",
                    self.stdout.snapshot(),
                    self.stderr.snapshot()
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stdout.finish();
        self.stderr.finish();
    }

    fn finish(mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                let stdout = self.stdout.finish();
                let stderr = self.stderr.finish();
                assert!(
                    status.success(),
                    "probe failed: {status}\nstdout: {stdout}\nstderr: {stderr}"
                );
                return stdout;
            }
            if Instant::now() >= deadline {
                self.stop();
                panic!(
                    "probe did not exit\nstdout: {}\nstderr: {}",
                    self.stdout.snapshot(),
                    self.stderr.snapshot()
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_child(role: &str) -> String {
    let mut probe = Probe::spawn(role);
    probe.wait_for_probe();
    probe.finish()
}

/// Create a fixture directory with the mode the product really validates. `create_dir_all` alone
/// would inherit the process umask (0755 on the CI runner), and production refuses a lock directory
/// that grants group or other write access.
fn make_dir(path: &Path, mode: u32) {
    std::fs::create_dir_all(path).unwrap_or_else(|e| panic!("the fixture directory {path:?}: {e}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .unwrap_or_else(|e| panic!("the fixture directory mode {path:?}: {e}"));
    }
    #[cfg(not(unix))]
    let _ = mode;
}

/// Clear the one host-global lock namespace before a case claims it: every ignored case starts from
/// its own namespace and never from another case's leftover lock file.
fn reset_global_namespace() {
    let dir = Path::new(LOCK_DIR);
    let _ = std::fs::remove_file(dir.join(CONTROL_LOCK_NAME));
    let _ = std::fs::remove_file(dir.join(EXECUTION_LOCK_NAME));
}

fn mode_of_dir(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .expect("the directory exists")
        .mode()
        & 0o777
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("the lock file exists")
        .mode()
        & 0o777
}

#[test]
#[ignore = "needs a writable /run and real flock: run on Linux"]
fn two_real_processes_contend_for_the_control_lock() {
    reset_global_namespace();
    let host = RealHost::on_host();
    // Make sure the directory and the file exist before the identity is read.
    let mut warmup = admission::control_lock(&host, Duration::from_secs(5)).expect("free lock");
    warmup.release().unwrap();
    let before = std::fs::metadata(control_lock_path()).expect("the lock file exists");

    let mut parent = admission::control_lock(&host, Duration::from_secs(5)).expect("free lock");
    let held = parent.identity().expect("the held lock has an identity");
    assert_eq!(held.ino, before.ino(), "the same inode is locked");

    // Another process must refuse, and the child must not have inherited the descriptor: an
    // inherited one would share this lock and succeed.
    let refused = run_child("contend");
    assert!(
        refused.contains(ADMISSION_LOCK_TIMEOUT),
        "the child must time out on the held lock: {refused}"
    );
    parent.release().unwrap();

    let acquired = run_child("contend");
    assert!(
        acquired.contains("PROBE acquired"),
        "the released lock must be free for another process: {acquired}"
    );

    let after = std::fs::metadata(control_lock_path()).expect("the lock file exists");
    assert_eq!(after.ino(), before.ino(), "the lock file was replaced");
    assert_eq!(after.len(), 0, "the lock file is never written");
    assert_eq!(mode_of(&control_lock_path()), 0o600, "private mode");
}

#[test]
#[ignore = "needs a writable /run and real flock: run on Linux"]
fn a_killed_holder_releases_the_lock_without_replacing_the_file() {
    reset_global_namespace();
    let host = RealHost::on_host();
    let mut warmup = admission::control_lock(&host, Duration::from_secs(5)).expect("free lock");
    warmup.release().unwrap();
    let before = std::fs::metadata(control_lock_path()).expect("the lock file exists");

    let mut probe = Probe::spawn("hold");
    let holding = probe.wait_for_probe();
    assert!(holding.contains("PROBE holding"), "{holding}");

    probe.stop();

    let mut after = admission::control_lock(&host, Duration::from_secs(5))
        .expect("a killed holder releases the lock immediately");
    let id = after.identity().expect("the lock has an identity");
    after.release().unwrap();
    assert_eq!(id.ino, before.ino(), "the lock file was replaced");
    let meta = std::fs::metadata(control_lock_path()).expect("the lock file exists");
    assert_eq!(meta.len(), 0);
    assert_eq!(mode_of(&control_lock_path()), 0o600);
}

#[test]
#[ignore = "needs a writable /run and real flock: run on Linux"]
fn a_symlink_at_the_lock_path_is_refused() {
    reset_global_namespace();
    let host = RealHost::on_host();
    let path = control_lock_path();
    // Independent of any other test: the directory exists here, whatever ran before, with the
    // private mode.
    make_dir(Path::new(LOCK_DIR), 0o755);
    let _ = std::fs::remove_file(&path);
    std::os::unix::fs::symlink("/etc/hostname", &path).expect("the symlink is installed");
    let Err(blocked) = admission::control_lock(&host, Duration::from_millis(200)) else {
        panic!("a symlink must never be followed");
    };
    assert_eq!(blocked.key, ADMISSION_LOCK_UNSUPPORTED, "{blocked:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore = "needs a writable /run and real flock: run on Linux"]
fn a_lock_file_that_group_or_others_can_reach_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    reset_global_namespace();
    let host = RealHost::on_host();
    let mut warmup = admission::control_lock(&host, Duration::from_secs(5)).expect("free lock");
    warmup.release().unwrap();
    let path = control_lock_path();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("the mode is changed");
    let Err(blocked) = admission::control_lock(&host, Duration::from_millis(200)) else {
        panic!("a world-readable lock file must be refused");
    };
    assert_eq!(blocked.key, ADMISSION_LOCK_UNSUPPORTED, "{blocked:?}");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("the mode is restored");
}

/// The directory-level checks of the fixed host namespace, on the real filesystem: the global
/// `/run/nvm-admission` keeps the directory rule (only write bits are refused) and never follows a
/// symlink.
#[test]
#[ignore = "needs a writable /run and real flock: run on Linux"]
fn the_lock_namespace_directory_keeps_its_contract() {
    use std::os::unix::fs::PermissionsExt;
    reset_global_namespace();
    let host = RealHost::on_host();
    let lock_dir = Path::new(LOCK_DIR);
    let data = std::env::temp_dir().join(format!("nvm-dir-checks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).expect("the data root exists");

    // 1) The global directory: 0755 (the real remote host) and 0750 pass — no group/other *write*
    // bit; 0775 and 0777 are refused.
    std::fs::create_dir_all(lock_dir).expect("the lock directory exists");
    for accepted in [0o755, 0o750] {
        std::fs::set_permissions(lock_dir, std::fs::Permissions::from_mode(accepted))
            .expect("mode");
        assert_eq!(mode_of_dir(lock_dir), accepted);
        admission::try_execution_lock(&host).unwrap_or_else(|e| {
            panic!("{accepted:o} has no group/other write bit and must pass: {e:?}")
        });
    }
    for refused in [0o775, 0o777] {
        std::fs::set_permissions(lock_dir, std::fs::Permissions::from_mode(refused)).expect("mode");
        let Err(blocked) = admission::try_execution_lock(&host) else {
            panic!("{refused:o} is group/other writable and must be refused");
        };
        assert_eq!(blocked.key, ADMISSION_LOCK_DIR_INVALID, "{blocked:?}");
    }
    std::fs::set_permissions(lock_dir, std::fs::Permissions::from_mode(0o755)).expect("mode");

    // 2) A symlinked lock directory is refused, never followed.
    let real = data.join("real-lock-dir");
    std::fs::create_dir_all(&real).expect("the target exists");
    std::fs::remove_dir_all(lock_dir).expect("the lock directory is moved aside");
    std::os::unix::fs::symlink(&real, lock_dir).expect("the symlink is installed");
    let Err(blocked) = admission::try_execution_lock(&host) else {
        panic!("a symlinked namespace must be refused");
    };
    assert_eq!(blocked.key, ADMISSION_LOCK_DIR_INVALID, "{blocked:?}");
    std::fs::remove_file(lock_dir).expect("the symlink is removed");
    std::fs::create_dir_all(lock_dir).expect("the lock directory is restored");
    std::fs::set_permissions(lock_dir, std::fs::Permissions::from_mode(0o755)).expect("mode");

    let _ = std::fs::remove_dir_all(&data);
}

/// A container process reaches the host-global lock directory through `Host::view`; its own `/run`
/// must stay untouched. The fixture maps `view` into `/proc/1/root` and lets `open_lock` reach
/// through it, exactly like `RealHost::in_container`.
#[test]
fn the_host_global_lock_paths_are_reached_through_the_view() {
    let host = default_host();
    host.remove("/run/nvm-admission");
    host.set_view_prefix(Some("/proc/1/root"));

    admission::lock_dir_bootstrap(&host).unwrap();
    assert!(
        host.exists("/proc/1/root/run/nvm-admission"),
        "the lock directory is created on the host"
    );
    assert!(
        !host.exists("/run/nvm-admission"),
        "the container's own /run must stay untouched"
    );

    let mut lock = admission::control_lock(&host, Duration::from_secs(1)).unwrap();
    lock.release().unwrap();
    assert!(
        host.exists("/proc/1/root/run/nvm-admission/control.lock"),
        "the lock file lives on the host"
    );
    assert!(!host.exists("/run/nvm-admission/control.lock"));
}

/// Driver deletion must honour the cross-process control ordering: while another writer holds the
/// control lock, the deletion does not happen.
#[tokio::test]
async fn driver_deletion_honours_the_global_control_lock() {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "longpass99").await;

    let id = "20260928T101530Z-3f9a0c12-build";
    let bytes = b"payload".to_vec();
    let canonical = sha256_hex(b"driver image");
    app.host
        .put_text(&format!("/var/lib/nvm/drivers/{id}/nvidia.raw"), "payload");
    app.host.put_text(
        &format!("/var/lib/nvm/drivers/{id}/record.json"),
        &json!({
            "kind": "official",
            "version": "580.178.04",
            "sha256": canonical,
            "size": bytes.len(),
            "target_kernel": KERNEL,
            "created_at": 1000u64,
            "fingerprint": null,
            "original_of": null,
            "keep": null,
        })
        .to_string(),
    );

    // Another writer holds the global control lock.
    let mut foreign = app
        .host
        .open_lock(
            std::path::Path::new("/run/nvm-admission/control.lock"),
            0o600,
        )
        .unwrap();
    assert!(foreign.try_acquire().unwrap());

    let request = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": [id]}),
        Some(&cookie),
    );
    tokio::pin!(request);
    let response = loop {
        tokio::select! {
            result = &mut request => break result,
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                // The lock wait is bounded by the fake clock; push it forward so the request
                // answers instead of stalling the suite.
                app.host.advance(Duration::from_secs(6));
            }
        }
    };
    let (status, body) = response;
    assert_eq!(status, axum::http::StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "delete.task_state_unknown", "{body}");
    // The row is still there: the deletion waited for the global lock.
    assert!(app
        .host
        .exists(&format!("/var/lib/nvm/drivers/{id}/record.json")));
    foreign.release().unwrap();
}

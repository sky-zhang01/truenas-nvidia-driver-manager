//! Process interruption must never leave an incomplete initial credential file.
mod common;

use common::{Decorate, Wrap};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use truenas_nvidia_driver_manager::auth::{AuthState, CredentialState};
use truenas_nvidia_driver_manager::host::{Create, Host, RealHost, SyncWrite};
use truenas_nvidia_driver_manager::store::{self, Paths};

const CHILD_MODE: &str = "NVM_AUTH_ATOMIC_CHILD";
const CHILD_DIR: &str = "NVM_AUTH_ATOMIC_DIR";
const PASSWORD: &str = "a sentence for atomic setup";

struct PausingWriter {
    inner: Box<dyn SyncWrite + Send>,
    mode: String,
    marker: PathBuf,
}

fn pause(marker: &Path) -> ! {
    std::fs::write(marker, b"paused").unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

impl Write for PausingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.mode == "during-write" {
            self.inner.write_all(&bytes[..bytes.len().min(7)])?;
            pause(&self.marker);
        }
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl SyncWrite for PausingWriter {
    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()?;
        if self.mode == "before-link" {
            pause(&self.marker);
        }
        Ok(())
    }
}

struct PauseHost {
    inner: RealHost,
    mode: String,
    marker: PathBuf,
}

impl Decorate for PauseHost {
    fn inner(&self) -> &dyn Host {
        &self.inner
    }
    fn create(&self, path: &Path, how: Create, mode: u32) -> io::Result<Box<dyn SyncWrite + Send>> {
        Ok(Box::new(PausingWriter {
            inner: self.inner.create(path, how, mode)?,
            mode: self.mode.clone(),
            marker: self.marker.clone(),
        }))
    }
    fn fsync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.fsync_dir(path)?;
        if self.mode == "after-link" {
            pause(&self.marker);
        }
        Ok(())
    }
}

#[tokio::test]
async fn interrupted_initial_credentials_remain_missing_or_complete() {
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        let root = PathBuf::from(std::env::var(CHILD_DIR).unwrap());
        let host: Arc<dyn Host> = Arc::new(Wrap(PauseHost {
            inner: RealHost::on_host(),
            mode,
            marker: root.join("paused"),
        }));
        let paths = Paths::new(&root);
        let auth = AuthState::new();
        let settings = common::settings();
        let code = auth
            .ensure_setup_code(&*host, &paths, &settings, "start")
            .unwrap()
            .code;
        auth.setup(host, paths, settings, code, PASSWORD.into())
            .await
            .unwrap();
        panic!("the child did not pause");
    }

    for mode in ["before-link", "during-write", "after-link"] {
        let root = std::env::temp_dir().join(format!("nvm-auth-atomic-{}", store::random_hex(8)));
        std::fs::create_dir_all(&root).unwrap();
        let paths = Paths::new(&root);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "interrupted_initial_credentials_remain_missing_or_complete",
                "--exact",
                "--nocapture",
            ])
            .env(CHILD_MODE, mode)
            .env(CHILD_DIR, &root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !root.join("paused").exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("{mode}: child exited before pausing: {status}");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{mode}: child did not reach its pause point");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        let host = RealHost::on_host();
        store::clean_leftovers(&host, &paths).unwrap();
        let restarted = AuthState::new();
        let state = restarted.load_state(&host, &paths, &common::settings());
        if mode == "after-link" {
            assert_eq!(state, CredentialState::Ok, "{mode}");
            assert!(restarted
                .ensure_setup_code(&host, &paths, &common::settings(), "start")
                .is_none());
        } else {
            assert_eq!(state, CredentialState::SetupRequired, "{mode}");
            assert!(restarted
                .ensure_setup_code(&host, &paths, &common::settings(), "start")
                .is_some());
            assert!(!paths.credentials().exists());
        }
        assert!(!std::fs::read_dir(paths.credentials().parent().unwrap())
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.')));
        std::fs::remove_dir_all(root).unwrap();
    }
}

struct RacingHost {
    inner: RealHost,
    barrier: std::sync::Barrier,
}

impl Decorate for RacingHost {
    fn inner(&self) -> &dyn Host {
        &self.inner
    }
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.barrier.wait();
        self.inner.hard_link(from, to)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_initial_creators_cannot_overwrite_each_other() {
    let root = std::env::temp_dir().join(format!("nvm-auth-race-{}", store::random_hex(8)));
    let paths = Paths::new(&root);
    let host: Arc<dyn Host> = Arc::new(Wrap(RacingHost {
        inner: RealHost::on_host(),
        barrier: std::sync::Barrier::new(2),
    }));
    let mut tasks = Vec::new();
    for password in [PASSWORD, "a different atomic setup password"] {
        let auth = AuthState::new();
        let code = auth
            .ensure_setup_code(&*host, &paths, &common::settings(), "start")
            .unwrap()
            .code;
        let host = host.clone();
        let paths = paths.clone();
        tasks.push(tokio::spawn(async move {
            (
                password,
                auth.setup(host, paths, common::settings(), code, password.into())
                    .await,
            )
        }));
    }
    let mut winner = None;
    let mut rejected = 0;
    for task in tasks {
        let (password, result) = task.await.unwrap();
        match result {
            Ok(_) => {
                assert!(winner.replace(password).is_none());
            }
            Err(truenas_nvidia_driver_manager::auth::SetupError::NotAvailable) => rejected += 1,
            Err(error) => panic!("unexpected creation failure: {error:?}"),
        }
    }
    assert_eq!(rejected, 1);
    let credentials = AuthState::new()
        .snapshot_credentials(&*host, &paths)
        .unwrap();
    assert!(truenas_nvidia_driver_manager::auth::verify_password(
        &credentials.password_hash,
        winner.unwrap()
    ));
    let secrets = paths.credentials().parent().unwrap().to_path_buf();
    assert!(!std::fs::read_dir(&secrets).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".web-credentials.json.tmp-")
    }));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_existing_empty_credential_file_is_not_a_missing_account() {
    let host = RealHost::on_host();
    let root = std::env::temp_dir().join(format!("nvm-auth-empty-{}", store::random_hex(8)));
    let paths = Paths::new(&root);
    store::prepare_dirs(&host, &paths).unwrap();
    std::fs::write(paths.credentials(), b"").unwrap();
    let auth = AuthState::new();
    assert_eq!(
        auth.load_state(&host, &paths, &common::settings()),
        CredentialState::Unreadable(truenas_nvidia_driver_manager::auth::UnreadableReason::NotJson)
    );
    assert!(auth
        .ensure_setup_code(&host, &paths, &common::settings(), "start")
        .is_none());
    std::fs::remove_dir_all(root).unwrap();
}

struct FailingWriter {
    inner: Box<dyn SyncWrite + Send>,
    point: &'static str,
}

impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.point == "write" {
            self.inner.write_all(&bytes[..bytes.len().min(7)])?;
            return Err(io::Error::other("interrupted credential write"));
        }
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl SyncWrite for FailingWriter {
    fn sync_all(&mut self) -> io::Result<()> {
        if self.point == "file-sync" {
            return Err(io::Error::other("credential file sync refused"));
        }
        self.inner.sync_all()
    }
}

struct FailingHost {
    inner: RealHost,
    point: &'static str,
    syncs: std::sync::atomic::AtomicUsize,
}

impl Decorate for FailingHost {
    fn inner(&self) -> &dyn Host {
        &self.inner
    }
    fn create(&self, path: &Path, how: Create, mode: u32) -> io::Result<Box<dyn SyncWrite + Send>> {
        if self.point == "create" {
            return Err(io::Error::other("credential temporary create refused"));
        }
        Ok(Box::new(FailingWriter {
            inner: self.inner.create(path, how, mode)?,
            point: self.point,
        }))
    }
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.point == "link" {
            return Err(io::Error::other("credential link refused"));
        }
        self.inner.hard_link(from, to)
    }
    fn fsync_dir(&self, path: &Path) -> io::Result<()> {
        let count = self.syncs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if (self.point == "publish-sync" && count == 0)
            || (self.point == "cleanup-sync" && count == 1)
        {
            return Err(io::Error::other("credential directory sync refused"));
        }
        self.inner.fsync_dir(path)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        if self.point == "unlink" {
            return Err(io::Error::other("credential temporary unlink refused"));
        }
        self.inner.remove_file(path)
    }
}

#[tokio::test]
async fn publication_failures_preserve_the_correct_account_state_without_a_session() {
    for point in [
        "create",
        "write",
        "file-sync",
        "link",
        "publish-sync",
        "unlink",
        "cleanup-sync",
    ] {
        let root = std::env::temp_dir().join(format!("nvm-auth-failure-{}", store::random_hex(8)));
        let paths = Paths::new(&root);
        let host: Arc<dyn Host> = Arc::new(Wrap(FailingHost {
            inner: RealHost::on_host(),
            point,
            syncs: std::sync::atomic::AtomicUsize::new(0),
        }));
        let auth = AuthState::new();
        let code = auth
            .ensure_setup_code(&*host, &paths, &common::settings(), "start")
            .unwrap()
            .code;
        let outcome = auth
            .setup(
                host,
                paths.clone(),
                common::settings(),
                code,
                PASSWORD.into(),
            )
            .await;
        assert!(
            matches!(
                outcome,
                Err(truenas_nvidia_driver_manager::auth::SetupError::Unsaved(_))
            ),
            "{point}: {outcome:?}"
        );
        let real = RealHost::on_host();
        store::clean_leftovers(&real, &paths).unwrap();
        let restarted = AuthState::new();
        let state = restarted.load_state(&real, &paths, &common::settings());
        if ["publish-sync", "unlink", "cleanup-sync"].contains(&point) {
            assert_eq!(state, CredentialState::Ok, "{point}");
            let credentials = restarted.snapshot_credentials(&real, &paths).unwrap();
            assert!(truenas_nvidia_driver_manager::auth::verify_password(
                &credentials.password_hash,
                PASSWORD
            ));
            assert!(restarted
                .ensure_setup_code(&real, &paths, &common::settings(), "start")
                .is_none());
        } else {
            assert_eq!(state, CredentialState::SetupRequired, "{point}");
            assert!(!paths.credentials().exists());
            assert!(restarted
                .ensure_setup_code(&real, &paths, &common::settings(), "start")
                .is_some());
        }
        assert!(!std::fs::read_dir(paths.credentials().parent().unwrap())
            .unwrap()
            .any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".web-credentials.json.tmp-")
            }));
        std::fs::remove_dir_all(root).unwrap();
    }
}

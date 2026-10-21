//! Regression tests for recovery and service-validation boundaries. Each case checks the observable
//! state or error at a shared boundary.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::auth::AuthState;
use truenas_nvidia_driver_manager::codes::{
    CleanupStep, CANONICAL_PATH, CATALOG_URL, CONTAINER_DATA_DIR, REASON_TIMED_OUT, RW_MOUNT,
    RW_TMP_PATH,
};
use truenas_nvidia_driver_manager::host::{Cmd, Host, Output};
use truenas_nvidia_driver_manager::plan::{Ctx, PlanSlot};
use truenas_nvidia_driver_manager::runner::download::{pick_toolkit, PickError};
use truenas_nvidia_driver_manager::startup::Startup;
use truenas_nvidia_driver_manager::store::{self, Paths, Validate};
use truenas_nvidia_driver_manager::upload::UploadState;
use truenas_nvidia_driver_manager::web::{router, StaticFiles};

const ID: &str = "20261003T101530Z-abcdef12";
const RUN_BODY: &[u8] =
    b"#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\nrest";

fn paths() -> Paths {
    Paths::new(CONTAINER_DATA_DIR)
}

fn names_with(host: &FakeHost, dir: &str, needle: &str) -> Vec<String> {
    host.children(dir)
        .into_iter()
        .filter(|n| n.contains(needle))
        .collect()
}

/// The web process on an arbitrary host implementation (so the tests can wrap the fake host with
/// delays or clock advances).
fn app_with(host: Arc<dyn Host>, fake: FakeHost, static_files: StaticFiles) -> TestApp {
    let ctx = Arc::new(Ctx {
        host,
        paths: paths(),
        settings: settings(),
        startup: Startup {
            problems: Vec::new(),
            pid_host_ok: true,
            host_data_dir: Some(PathBuf::from(HOST_DATA_DIR)),
            image_id: Some(format!("sha256:{}", "a".repeat(64))),
        },
        process_lock: tokio::sync::Mutex::new(()),
        gpu_users_cache: Default::default(),
        plan: std::sync::Mutex::new(PlanSlot::new()),
        upload: Arc::new(UploadState::new()),
        auth: Arc::new(AuthState::new()),
    });
    TestApp {
        host: fake,
        router: router(ctx.clone(), static_files),
        setup_code: Arc::new(std::sync::Mutex::new(
            ctx.auth
                .ensure_setup_code(&*ctx.host, &ctx.paths, &ctx.settings, "start")
                .map(|issued| issued.code),
        )),
        ctx,
    }
}

/// The early-exit branch of the upload landing: when the incoming rename is refused and the
/// rename-based slot-restore paths are refused too, the accepted bytes are still put back at the
/// fixed slot in place: a refused rename is not a refused write, so the slot is never left absent.
#[tokio::test]
async fn upload_early_exit_keeps_the_accepted_slot() {
    let host = default_host();
    let p = paths();
    let st = Arc::new(UploadState::new());
    let group = truenas_nvidia_driver_manager::upload::open_group(&host, &p, &st).unwrap();
    receive_run(&host, &group, &st).await.unwrap();
    let dir = format!("/var/lib/nvm/manager/upload/{group}");

    // Refuse every replacement and restore rename onto the fixed name; the accepted bytes are
    // restored in place without any rename.
    host.fail_rename_to_suffix_after("driver.run", 0);
    let failed = receive_run(&host, &group, &st).await;
    assert!(failed.unwrap_err().contains("write_failed"));
    assert_eq!(
        host.file(&format!("{dir}/driver.run")).unwrap(),
        RUN_BODY,
        "the accepted slot must not be left absent"
    );
    assert!(
        names_with(&host, &dir, ".driver.run.recovery-").is_empty(),
        "a restored slot keeps no protected material"
    );

    store::clean_leftovers(&host, &p).unwrap();
    assert_eq!(host.file(&format!("{dir}/driver.run")).unwrap(), RUN_BODY);
}

async fn receive_run(host: &FakeHost, group: &str, st: &Arc<UploadState>) -> Result<(), String> {
    let lock = tokio::sync::Mutex::new(());
    truenas_nvidia_driver_manager::upload::receive(
        Arc::new(host.clone()),
        paths(),
        Some(HOST_DATA_DIR.into()),
        group.to_string(),
        "NVIDIA-Linux-x86_64-580.178.04.run".to_string(),
        None,
        Body::from(RUN_BODY),
        st.clone(),
        &lock,
        || true,
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("{e:?}"))
}

/// A recovery write publishes only a complete, verified copy: the protected name never holds a
/// half-written attempt, and a leftover temporary of a failed attempt is never treated as material.
#[test]
fn recovery_publishing_is_complete_and_verified() {
    let host = default_host();
    let dir = paths().upload();
    host.mkdir(dir.to_str().unwrap());
    let target = dir.join("driver.run");
    host.put_text(target.to_str().unwrap(), "original");
    let missing_source = dir.join("not-there");
    let kept = store::protect_recovery(
        &host,
        &dir,
        "driver.run",
        &missing_source,
        0o600,
        Some(b"old-complete"),
    )
    .expect("the bytes the caller holds are published");
    assert_eq!(host.file(kept.to_str().unwrap()).unwrap(), b"old-complete");
    assert!(names_with(&host, dir.to_str().unwrap(), ".tmp-").is_empty());

    // A half-written attempt lives under a temporary name, never under the protected name, and the
    // cleanup deletes it without touching the target.
    let group = dir.join("group1");
    host.mkdir(group.to_str().unwrap());
    let half = group.join(format!(".driver.run.tmp-{}", "b".repeat(16)));
    host.put_file(half.to_str().unwrap(), b"uniq".to_vec());
    store::clean_leftovers(&host, &paths()).unwrap();
    assert_eq!(host.file(target.to_str().unwrap()).unwrap(), b"original");
    assert!(!host.exists(half.to_str().unwrap()));
    assert_eq!(host.file(kept.to_str().unwrap()).unwrap(), b"old-complete");
}

/// A recovery copy is published through a temporary name that carries the target's own file name.
/// Nesting the recovery name inside the temporary name produces material the state directory cannot
/// explain, which turns the whole collection unreadable and stops every later update, rollback and
/// deletion.
#[test]
fn recovery_temporaries_never_nest_the_recovery_name() {
    let host = default_host();
    let p = paths();
    let dir = p.state_dir();
    host.mkdir(dir.to_str().unwrap());
    let file_name = format!("{ID}.json");
    let missing_source = dir.join("not-there");
    let kept = store::protect_recovery(
        &host,
        &dir,
        &file_name,
        &missing_source,
        0o600,
        Some(b"previous bytes"),
    )
    .expect("the bytes the caller holds are published");
    assert_eq!(
        host.file(kept.to_str().unwrap()).unwrap(),
        b"previous bytes"
    );

    let temps: Vec<String> = host
        .ops()
        .into_iter()
        .filter(|op| op.kind.starts_with("create") && op.path.contains(".tmp-"))
        .map(|op| op.path)
        .collect();
    assert_eq!(
        temps.len(),
        1,
        "one publish writes one temporary: {temps:?}"
    );
    let name = temps[0].rsplit('/').next().unwrap().to_string();
    assert!(
        !name.contains(".recovery-"),
        "the temporary name nests a recovery name: {name}"
    );
    assert_eq!(
        name,
        format!(".{file_name}.tmp-{}", &name[name.len() - 16..])
    );
}

/// The read-only restore is judged by the post-state reading, never by the exit code of `zfs set`
/// alone.
#[test]
fn readonly_restore_is_judged_by_the_readback() {
    let host = default_host();
    let p = paths();
    let rootfs = truenas_nvidia_driver_manager::task_state::Rootfs {
        dataset: "tank/ROOT/env/usr".to_string(),
        readonly_before: "on".to_string(),
    };
    host.preset(
        "zfs",
        &["set", "readonly=on", "tank/ROOT/env/usr"],
        FakeOutput::stdout(""),
    );
    // The set exits 0 but the property reads back as `off`: a failed step.
    host.preset(
        "zfs",
        &["get", "-H", "-o", "value", "readonly", "tank/ROOT/env/usr"],
        FakeOutput::stdout("off\n"),
    );
    let results = truenas_nvidia_driver_manager::tasks::cleanup(&host, &p, ID, Some(&rootfs));
    let step = results
        .iter()
        .find(|r| r.step == truenas_nvidia_driver_manager::codes::CleanupStep::RestoreReadonly)
        .expect("the readonly step ran");
    assert!(!step.ok, "a mismatching readback must fail the step");
    assert_eq!(step.reading.as_deref(), Some("off"));
    assert!(step.reason.as_deref().unwrap().contains("off"));
}

/// A directory-sync failure on the restore path propagates out of `clean_leftovers` and the
/// material's meaning is preserved.
#[test]
fn recovery_sync_failures_propagate() {
    let host = default_host();
    let dir = "/var/lib/nvm/manager/state";
    let target = format!("{dir}/{ID}.json");
    let recovery = format!("{dir}/.{ID}.json.recovery-{}", "c".repeat(16));
    host.put_text(&target, "same-bytes");
    host.put_text(&recovery, "same-bytes");
    host.fail_fsync_dir(dir);
    assert!(
        store::clean_leftovers(&host, &paths()).is_err(),
        "the startup cleanup must propagate the sync error"
    );
    host.put_text(&target, "same-bytes");
    host.put_text(&recovery, "same-bytes");
    assert!(
        store::flush_recovery(&host, std::path::Path::new(dir), &format!("{ID}.json")).is_err(),
        "the sync error must not be discarded"
    );
}

/// A second unreadable state file never overwrites the first permanent `.json.unreadable` original;
/// the current file stays UNKNOWN.
#[test]
fn a_second_corrupt_state_keeps_the_first_original() {
    let host = default_host();
    let p = paths();
    let state = p.state(ID).to_string_lossy().into_owned();
    let sidecar = p.state_unreadable(ID).to_string_lossy().into_owned();
    host.put_text(&sidecar, "first-original");
    host.put_text(&state, "second-corrupt");
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &p);
    assert_eq!(host.file_text(&sidecar).as_deref(), Some("first-original"));
    assert_eq!(host.file_text(&state).as_deref(), Some("second-corrupt"));
    let task = reconciled.state_of(ID).expect("the task is reported");
    assert!(task.status.is_none(), "the current state stays UNKNOWN");
    assert!(
        task.state_note.is_none(),
        "no new record may claim the state"
    );
}

/// The credentials validator enforces the 16-byte salt.
#[test]
fn credentials_require_a_sixteen_byte_salt() {
    let valid = truenas_nvidia_driver_manager::auth::hash_password("hunter12").unwrap();
    let mut credentials = truenas_nvidia_driver_manager::auth::Credentials {
        username: "admin".into(),
        password_hash: valid.clone(),
        session_key: "a".repeat(64),
        ssh_keys: Vec::new(),
    };
    credentials.validate().expect("the generated hash is valid");
    // Only the salt changes: eight bytes must be rejected with the same algorithm and parameters.
    let mut parts: Vec<String> = valid.split('$').map(str::to_string).collect();
    parts[4] = "YWFhYWFhYQ".to_string(); // 8 bytes, base64 without padding
    credentials.password_hash = parts.join("$");
    assert!(
        credentials.validate().is_err(),
        "an unapproved salt length must be rejected"
    );
}

/// Duplicate `(Package, Version)` paragraphs make the index unreadable; a version outside the rule
/// never takes part in the selection.
#[test]
fn toolkit_index_duplicates_and_invalid_versions() {
    use truenas_nvidia_driver_manager::codes::TOOLKIT_PACKAGES;
    let mut index = String::new();
    for package in TOOLKIT_PACKAGES {
        index.push_str(&format!(
            "Package: {package}\nArchitecture: amd64\nVersion: 1.20.1-1\nFilename: ./{package}_1.20.1-1_amd64.deb\nSize: 10\nSHA256: aa\n\n"
        ));
    }
    let picked = pick_toolkit(&index).expect("the complete four-package index is valid");
    assert_eq!(picked.version, "1.20.1-1");

    // Appending a non-conforming version does not block the valid intersection.
    let with_bad = format!(
        "{index}Package: nvidia-container-toolkit\nArchitecture: amd64\nVersion: 2.0~rc1\nFilename: ./nvidia-container-toolkit_2.0~rc1_amd64.deb\nSize: 9\nSHA256: bb\n\n"
    );
    assert_eq!(pick_toolkit(&with_bad).unwrap().version, "1.20.1-1");

    // A duplicated paragraph of a complete set is unreadable.
    let duplicated = format!("{index}Package: nvidia-container-toolkit\nArchitecture: amd64\nVersion: 1.20.1-1\nFilename: ./nvidia-container-toolkit_1.20.1-1_amd64.deb\nSize: 10\nSHA256: aa\n\n");
    assert_eq!(pick_toolkit(&duplicated), Err(PickError::IndexUnreadable));
}

/// Versions runs the four readings concurrently.
#[tokio::test]
async fn version_readings_run_concurrently() {
    let host = default_host();
    prime_canonical(&host, "580.178.04");
    host.add_gpu("0000:01:00.0", 0x1b80, Some("nvidia"));
    prime_kernel_headers(&host, KERNEL);
    host.set_curl(
        CATALOG_URL,
        200,
        b"<a href=\"580.178.04/\">580.178.04</a>\n",
    );
    let fake = host.clone();
    let gate = Arc::new(ReadGate::default());
    let release = ReleaseGate(gate.clone());
    let slow = Arc::new(SlowHost::blocking_on(host, "overlap", gate.clone()));
    let app = app_with(slow.clone(), fake, StaticFiles::default());
    let cookie = setup_app(&app, "longpass99").await;
    gate.armed.store(true, Ordering::SeqCst);
    let response = tokio::spawn(async move { get(&app, "/api/versions", Some(&cookie)).await });
    wait_until(|| gate.entered.load(Ordering::SeqCst) >= 2).await;
    drop(release);
    let (status, _) = response.await.unwrap();
    assert_eq!(status, StatusCode::OK);
}

/// A GPU listing whose clock passes READING_TIMEOUT is an unreadable GPU reading, not a partial
/// success.
#[tokio::test]
async fn gpu_reading_over_the_deadline_is_unreadable() {
    let host = default_host();
    prime_canonical(&host, "580.178.04");
    host.add_gpu("0000:01:00.0", 0x1b80, Some("nvidia"));
    host.set_curl(
        CATALOG_URL,
        200,
        b"<a href=\"580.178.04/\">580.178.04</a>\n",
    );
    let fake = host.clone();
    let slow = Arc::new(SlowHost::advancing_pci(host, Duration::from_secs(16)));
    let app = app_with(slow.clone(), fake, StaticFiles::default());
    let cookie = setup_app(&app, "longpass99").await;
    let (status, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let unreadable = body["unreadable"].as_array().unwrap();
    assert!(
        unreadable
            .iter()
            .any(|u| u["object"] == "gpus" && u["reason"]["key"] == "reason.timed_out"),
        "{body}"
    );
    assert_eq!(body["gpus"], json!([]));
}

/// The permanent-path checksum read has its own deadline; a read that passes it fails closed:
/// driver list reports `canonical_unreadable` and driver deletion refuses with 409.
#[tokio::test]
async fn canonical_hash_deadline_fails_closed() {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    host.put_text(
        &format!("{HOST_DATA_DIR}/drivers/{ID}-build/nvidia.raw"),
        "driver image",
    );
    let sha = common::sha256_hex(b"driver image");
    host.put_text(
        &format!("{HOST_DATA_DIR}/drivers/{ID}-build/record.json"),
        &format!(
            r#"{{"kind":"official","version":"580.173.02","sha256":"{sha}","size":12,"target_kernel":"{KERNEL}","created_at":1000,"fingerprint":null,"original_of":null,"keep":null}}"#
        ),
    );
    let fake = host.clone();
    let slow = Arc::new(SlowHost::advancing_canonical(host, Duration::from_secs(16)));
    let app = app_with(slow.clone(), fake, StaticFiles::default());
    let cookie = setup_app(&app, "longpass99").await;

    let (status, body) = get(&app, "/api/drivers", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["delete_unavailable"]["key"], "canonical_unreadable",
        "{body}"
    );

    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": [format!("{ID}-build")]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "delete.canonical_unreadable");
    assert!(app
        .host
        .exists(&format!("{HOST_DATA_DIR}/drivers/{ID}-build/nvidia.raw")));
}

/// Invalid UTF-8 and malformed query values are format errors answered with the JSON key, not the
/// framework's plain text.
#[tokio::test]
async fn invalid_query_and_path_are_json_format_errors() {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let app = app(host.clone(), settings(), healthy_startup(&host));
    let cookie = setup_app(&app, "longpass99").await;
    // A signed-in request avoids the session layer's 401 for this contract.
    let (status, body) = get(&app, &format!("/api/tasks/{ID}?after=%FF"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "request.invalid");
    let (status, body) = get(&app, &format!("/api/tasks/{ID}?after=%2B1"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "request.invalid");
    // A path segment that is not valid UTF-8: JSON, not axum's text/plain.
    let (status, body, headers) = request(
        &app,
        axum::http::Method::GET,
        "/api/tasks/%FF",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "application/json"
    );
    assert_eq!(body["error"], "request.invalid");
}

// ---------------------------------------------------------------- helpers --

/// A releasable blocking boundary. The owner always releases it on unwind.
#[derive(Default)]
struct ReadGate {
    armed: AtomicBool,
    entered: AtomicUsize,
    released: Mutex<bool>,
    ready: Condvar,
}

impl ReadGate {
    fn block(&self) {
        if !self.armed.load(Ordering::SeqCst) {
            return;
        }
        self.entered.fetch_add(1, Ordering::SeqCst);
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.ready.wait(released).unwrap();
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.ready.notify_all();
    }
}

struct ReleaseGate(Arc<ReadGate>);

impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}

async fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "the blocking worker did not make progress"
        );
        tokio::task::yield_now().await;
    }
}

/// Stay runnable while advancing, so blocked workers cannot trigger automatic clock jumps to
/// unrelated readings' deadlines.
async fn advance_reading_clock(seconds: u64) {
    for _ in 0..seconds * 10 {
        tokio::time::advance(Duration::from_millis(100)).await;
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
    }
}

/// Decorates only the calls that exercise overlap or deadline boundaries.
struct SlowHost {
    inner: FakeHost,
    advance_on: Option<(String, Duration)>,
    block_on: Option<(String, Arc<ReadGate>)>,
    clock: Option<(Instant, tokio::time::Instant)>,
}

impl SlowHost {
    fn with_advance(inner: FakeHost, needle: &str, advance: Duration) -> Wrap<Self> {
        Wrap(Self {
            inner,
            advance_on: Some((needle.to_string(), advance)),
            block_on: None,
            clock: None,
        })
    }

    fn blocking_on(inner: FakeHost, needle: &str, gate: Arc<ReadGate>) -> Wrap<Self> {
        Wrap(Self {
            inner,
            advance_on: None,
            block_on: Some((needle.to_string(), gate)),
            clock: Some((Instant::now(), tokio::time::Instant::now())),
        })
    }

    fn advancing_pci(inner: FakeHost, advance: Duration) -> Wrap<Self> {
        Self::with_advance(inner, "/class", advance)
    }

    fn advancing_canonical(inner: FakeHost, advance: Duration) -> Wrap<Self> {
        Self::with_advance(inner, CANONICAL_PATH, advance)
    }

    fn boundary(&self, name: &str) {
        if let Some((needle, gate)) = &self.block_on {
            let matches = if needle == "overlap" {
                name.contains("/include/") || name == "/proc/modules" || name.ends_with("/class")
            } else {
                name.contains(needle.as_str())
            };
            if matches {
                gate.block();
            }
        }
        if let Some((needle, advance)) = &self.advance_on {
            if name.contains(needle.as_str()) {
                self.inner.advance(*advance);
            }
        }
    }
}

impl Decorate for SlowHost {
    fn inner(&self) -> &dyn Host {
        &self.inner
    }

    fn run(&self, cmd: &Cmd) -> Output {
        self.boundary(cmd.program);
        self.inner.run(cmd)
    }

    fn read(&self, p: &std::path::Path) -> std::io::Result<Vec<u8>> {
        self.boundary(&p.to_string_lossy());
        self.inner.read(p)
    }

    fn open(&self, p: &std::path::Path) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        self.boundary(&p.to_string_lossy());
        self.inner.open(p)
    }

    fn lstat(&self, p: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        if self.advance_on.is_some() {
            self.boundary(&p.to_string_lossy());
        }
        self.inner.lstat(p)
    }

    fn read_link(&self, p: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        self.boundary(&p.to_string_lossy());
        self.inner.read_link(p)
    }

    fn now(&self) -> Instant {
        match self.clock {
            Some((host, clock)) => host + tokio::time::Instant::now().duration_since(clock),
            None => self.inner.now(),
        }
    }
}

/// `zfs set` may return non-zero although the property was applied; the restore is judged by the
/// readback.
#[test]
fn a_nonzero_zfs_set_with_the_matching_readback_is_not_a_failure() {
    let host = default_host();
    let p = paths();
    let rootfs = truenas_nvidia_driver_manager::task_state::Rootfs {
        dataset: "tank/ROOT/env/usr".to_string(),
        readonly_before: "on".to_string(),
    };
    // The fake `zfs set` applies the value and exits 255, modeling a dataset covered by an overlay.
    let results = truenas_nvidia_driver_manager::tasks::cleanup(&host, &p, ID, Some(&rootfs));
    let step = results
        .iter()
        .find(|r| r.step == truenas_nvidia_driver_manager::codes::CleanupStep::RestoreReadonly)
        .expect("the readonly step ran");
    assert!(step.ok, "the matching readback must pass: {step:?}");
    assert_eq!(step.reading.as_deref(), Some("on"));
    assert_eq!(host.readonly(), "on");
}

/// Versions answers each of its four readings at that reading's own budget. A reading whose I/O
/// blocks past `READING_TIMEOUT` is classified as that reading's timeout and never holds the
/// endpoint past the budget.
#[tokio::test]
async fn versions_answers_a_blocking_reading_at_its_own_budget() {
    let host = default_host();
    prime_canonical(&host, "580.178.04");
    host.add_gpu("0000:01:00.0", 0x1b80, Some("nvidia"));
    host.set_curl(
        CATALOG_URL,
        200,
        b"<a href=\"580.178.04/\">580.178.04</a>\n",
    );
    let gate = Arc::new(ReadGate::default());
    let _release = ReleaseGate(gate.clone());
    let slow = Arc::new(SlowHost::blocking_on(host.clone(), "/class", gate.clone()));
    let app = app_with(slow, host, StaticFiles::default());
    let cookie = setup_app(&app, "longpass99").await;
    tokio::time::pause();
    gate.armed.store(true, Ordering::SeqCst);
    let response = tokio::spawn(async move { get(&app, "/api/versions", Some(&cookie)).await });
    wait_until(|| gate.entered.load(Ordering::SeqCst) > 0).await;
    advance_reading_clock(14).await;
    assert!(
        !response.is_finished(),
        "the GPU reading lost its 15 s budget"
    );
    advance_reading_clock(2).await;
    wait_until(|| response.is_finished()).await;
    let (status, body) = response.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let unreadable = body["unreadable"].as_array().unwrap();
    assert!(
        unreadable
            .iter()
            .any(|u| u["object"] == "gpus" && u["reason"]["key"] == "reason.timed_out"),
        "{body}"
    );
    // The blocked reading is the only timeout: the catalog query and the header reads (missing
    // files here) are unaffected by the GPU read.
    assert!(
        !unreadable
            .iter()
            .any(|u| u["object"] == "catalog" && u["reason"]["key"] == "reason.timed_out"),
        "{body}"
    );
    assert!(
        unreadable
            .iter()
            .any(|u| u["object"] == "kernel_headers"
                && u["reason"]["key"] == "reason.file_missing"),
        "{body}"
    );
    assert_eq!(body["items"], json!([]));
    assert_eq!(body["gpus"], json!([]));
}

/// An empty canonical file whose read only completes after the deadline is not a value; driver list
/// reports `canonical_unreadable` and driver deletion refuses to delete (an empty EOF must be
/// deadline-checked too).
#[tokio::test]
async fn an_empty_canonical_read_past_the_deadline_blocks_deletion() {
    let host = default_host();
    host.put_text(CANONICAL_PATH, "");
    host.put_text(
        &format!("{HOST_DATA_DIR}/drivers/{ID}-build/nvidia.raw"),
        "driver image",
    );
    let sha = sha256_hex(b"driver image");
    host.put_text(
        &format!("{HOST_DATA_DIR}/drivers/{ID}-build/record.json"),
        &format!(
            r#"{{"kind":"official","version":"580.173.02","sha256":"{sha}","size":12,"target_kernel":"{KERNEL}","created_at":1000,"fingerprint":null,"original_of":null,"keep":null}}"#
        ),
    );
    let fake = host.clone();
    let slow = Arc::new(SlowHost::advancing_canonical(host, Duration::from_secs(16)));
    let app = app_with(slow, fake, StaticFiles::default());
    let cookie = setup_app(&app, "longpass99").await;

    let (status, body) = get(&app, "/api/drivers", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["delete_unavailable"]["key"], "canonical_unreadable",
        "{body}"
    );
    let (status, body) = post(
        &app,
        "/api/drivers/delete",
        json!({"ids": [format!("{ID}-build")]}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "delete.canonical_unreadable");
    assert!(app
        .host
        .exists(&format!("{HOST_DATA_DIR}/drivers/{ID}-build/nvidia.raw")));
}

/// Loaded-version, TrueNAS-version, extension-link, and module readings are UNKNOWN when their
/// reads complete after `READING_TIMEOUT`.
#[test]
fn shared_readings_completing_after_the_deadline_are_unknown() {
    use truenas_nvidia_driver_manager::codes::REASON_TIMED_OUT;
    use truenas_nvidia_driver_manager::facts::{self, LinkReading, LoadedVersion, Reading};

    let unknown = |r: Reading<String>| match r {
        Reading::Unknown(reason) => assert_eq!(reason, REASON_TIMED_OUT),
        Reading::Value(v) => panic!("expected UNKNOWN, got {v}"),
    };

    let host = default_host();
    host.put_text(
        "/proc/driver/nvidia/version",
        "NVRM version: NVIDIA UNIX x86_64 Kernel Module  580.178.04  Thu Sep  4 00:00:00 UTC 2025\n",
    );
    let slow = SlowHost::with_advance(host, "/proc/driver/nvidia/version", Duration::from_secs(16));
    assert_eq!(
        facts::loaded_version(&slow, None),
        LoadedVersion::Unknown(REASON_TIMED_OUT)
    );

    let host = default_host();
    host.put_text("/data/manifest.json", "{\"version\":\"25.10.7\"}");
    let slow = SlowHost::with_advance(host, "/data/manifest.json", Duration::from_secs(16));
    unknown(facts::truenas_version(&slow));

    let host = default_host();
    host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH);
    let slow = SlowHost::with_advance(host, "/run/extensions/nvidia.raw", Duration::from_secs(16));
    assert!(matches!(
        facts::link(&slow),
        LinkReading::Unknown(REASON_TIMED_OUT)
    ));

    let host = default_host();
    host.put_text("/proc/modules", "nvidia 1 0 - Live 0x0\n");
    let slow = SlowHost::with_advance(host, "/proc/modules", Duration::from_secs(16));
    assert!(matches!(
        facts::read_modules(&slow),
        Reading::Unknown(REASON_TIMED_OUT)
    ));
}

/// The home response marks a shared fact UNKNOWN with the timeout reason when its read exceeds the
/// deadline.
#[tokio::test]
async fn home_shows_an_over_budget_shared_fact_as_unknown() {
    let host = default_host();
    prime_canonical(&host, "580.178.04");
    host.put_text(
        "/proc/driver/nvidia/version",
        "NVRM version: NVIDIA UNIX x86_64 Kernel Module  580.178.04  Thu Sep  4 00:00:00 UTC 2025\n",
    );
    let fake = host.clone();
    let slow = Arc::new(SlowHost::with_advance(
        host,
        "/proc/driver/nvidia/version",
        Duration::from_secs(16),
    ));
    let app = app_with(slow, fake, StaticFiles::default());
    let cookie = setup_app(&app, "longpass99").await;
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["loaded_version"]["unknown"]["key"], "reason.timed_out",
        "{body}"
    );
}

/// The metadata of an unselected version never spoils the selection; the selected version's own
/// entry still has to be complete.
#[test]
fn pick_toolkit_ignores_the_metadata_of_unselected_versions() {
    use truenas_nvidia_driver_manager::codes::TOOLKIT_PACKAGES;
    let entry = |pkg: &str, ver: &str, extra: &str| {
        format!("Package: {pkg}\nArchitecture: amd64\nVersion: {ver}\n{extra}\n")
    };
    let mut index = String::new();
    for package in TOOLKIT_PACKAGES {
        index.push_str(&entry(
            package,
            "1.20.1-1",
            &format!("Filename: ./{package}_1.20.1-1_amd64.deb\nSize: 10\nSHA256: aa\n"),
        ));
        // An unselected release with missing or foreign metadata.
        index.push_str(&entry(
            package,
            "1.19.0-1",
            "Filename: ./whatever_1.19.0-1_amd64.deb\n",
        ));
    }
    let picked = pick_toolkit(&index).expect("the unselected version must not block the index");
    assert_eq!(picked.version, "1.20.1-1");
    assert!(picked.files.iter().all(|f| f.filename.contains("1.20.1-1")));
    assert!(picked
        .files
        .iter()
        .all(|f| f.sha256 == "aa" && f.size == 10));

    let broken = index.replace(
        "./libnvidia-container1_1.20.1-1_amd64.deb",
        "./other_1.20.1-1_amd64.deb",
    );
    assert!(matches!(
        pick_toolkit(&broken),
        Err(PickError::EntryUnexpected(_))
    ));
}

/// Catalog queries keep their 20 s command budget. A valid result after the other readings' 15 s
/// limit must still return its items.
#[tokio::test]
async fn a_catalog_query_inside_the_catalog_command_budget_is_not_a_reading_timeout() {
    let host = default_host();
    prime_canonical(&host, "580.178.04");
    host.add_gpu("0000:01:00.0", 0x1b80, Some("nvidia"));
    prime_kernel_headers(&host, KERNEL);
    host.set_curl(
        CATALOG_URL,
        200,
        b"<a href=\"580.178.04/\">580.178.04</a>\n",
    );
    let gate = Arc::new(ReadGate::default());
    let release = ReleaseGate(gate.clone());
    let slow = Arc::new(SlowHost::blocking_on(host.clone(), "curl", gate.clone()));
    let app = app_with(slow, host, StaticFiles::default());
    let cookie = setup_app(&app, "longpass99").await;
    tokio::time::pause();
    gate.armed.store(true, Ordering::SeqCst);
    let response = tokio::spawn(async move { get(&app, "/api/versions", Some(&cookie)).await });
    wait_until(|| gate.entered.load(Ordering::SeqCst) > 0).await;
    advance_reading_clock(16).await;
    assert!(
        !response.is_finished(),
        "the catalog must retain its 20 s budget"
    );
    drop(release);
    wait_until(|| response.is_finished()).await;
    let (status, body) = response.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["unreadable"].as_array().unwrap().is_empty(),
        "catalog command inside its own budget was reported as a timeout: {body}"
    );
    assert_eq!(body["items"], json!([{"version":"580.178.04"}]));
}

/// A catalog query exceeding 20 s reports its own deadline in the versions response, rather than
/// the other readings' 15 s budget.
#[tokio::test]
async fn a_catalog_query_past_the_catalog_command_budget_reports_20_seconds() {
    let host = default_host();
    prime_canonical(&host, "580.178.04");
    host.set_curl(
        CATALOG_URL,
        200,
        b"<a href=\"580.178.04/\">580.178.04</a>\n",
    );
    // Keep the command blocked while the outer reading reaches its deadline.
    let gate = Arc::new(ReadGate::default());
    let _release = ReleaseGate(gate.clone());
    let slow = Arc::new(SlowHost::blocking_on(host.clone(), "curl", gate.clone()));
    let app = app_with(slow, host, StaticFiles::default());
    let cookie = setup_app(&app, "longpass99").await;
    tokio::time::pause();
    gate.armed.store(true, Ordering::SeqCst);
    let response = tokio::spawn(async move { get(&app, "/api/versions", Some(&cookie)).await });
    wait_until(|| gate.entered.load(Ordering::SeqCst) > 0).await;
    advance_reading_clock(19).await;
    assert!(!response.is_finished(), "the catalog timed out before 20 s");
    advance_reading_clock(2).await;
    wait_until(|| response.is_finished()).await;
    let (status, body) = response.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"], json!([]));
    assert!(
        body["unreadable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["object"] == "catalog"
                && u["reason"] == json!({"key": REASON_TIMED_OUT, "params": {"seconds": 20}})),
        "{body}"
    );
}

/// A refused temporary-file removal is a failed cleanup step, never reported as success.
#[test]
fn a_refused_temp_removal_is_a_failed_cleanup_step() {
    let host = FakeHost::new();
    let p = paths();
    host.add_mount(DATASET, RW_MOUNT, "zfs", "rw");
    host.put_text(RW_TMP_PATH, "half-written");
    host.fail_remove(RW_TMP_PATH);
    let results = truenas_nvidia_driver_manager::tasks::cleanup(&host, &p, ID, None);
    let step = results
        .iter()
        .find(|r| r.step == CleanupStep::DeleteTempFile)
        .expect("the temp-file step ran");
    assert!(host.any_op_for(RW_TMP_PATH, "remove_file"));
    assert!(
        !step.ok,
        "a rejected removal was reported successful: {step:?}"
    );
    assert!(
        step.reason.is_some(),
        "the refusal reason must be recorded: {step:?}"
    );
}

/// A refused state-temp removal is a failed cleanup step, and the bytes stay.
#[test]
fn a_refused_state_temp_removal_is_a_failed_cleanup_step() {
    let host = FakeHost::new();
    let p = paths();
    let temp = p
        .state_dir()
        .join(format!(".{ID}.json.tmp-aaaaaaaaaaaaaaaa"));
    host.put_text(temp.to_str().unwrap(), "half-written");
    host.fail_remove(temp.to_str().unwrap());
    let results = truenas_nvidia_driver_manager::tasks::cleanup(&host, &p, ID, None);
    let step = results
        .iter()
        .find(|r| r.step == CleanupStep::DeleteStateTemps)
        .expect("the state-temp step ran");
    assert!(host.file(temp.to_str().unwrap()).is_some());
    assert!(
        !step.ok,
        "a rejected removal was reported successful: {step:?}"
    );
}

/// The positive control: a successful cleanup reports both steps as done and really removes the
/// files.
#[test]
fn a_successful_cleanup_removes_and_reports_the_temp_files() {
    let host = FakeHost::new();
    let p = paths();
    host.add_mount(DATASET, RW_MOUNT, "zfs", "rw");
    host.put_text(RW_TMP_PATH, "half-written");
    let temp = p
        .state_dir()
        .join(format!(".{ID}.json.tmp-aaaaaaaaaaaaaaaa"));
    host.put_text(temp.to_str().unwrap(), "half-written");
    let results = truenas_nvidia_driver_manager::tasks::cleanup(&host, &p, ID, None);
    let temp_step = results
        .iter()
        .find(|r| r.step == CleanupStep::DeleteTempFile)
        .expect("the temp-file step ran");
    let state_step = results
        .iter()
        .find(|r| r.step == CleanupStep::DeleteStateTemps)
        .expect("the state-temp step ran");
    assert!(temp_step.ok, "{temp_step:?}");
    assert!(state_step.ok, "{state_step:?}");
    assert!(!host.exists(RW_TMP_PATH));
    assert!(!host.exists(temp.to_str().unwrap()));
}

/// A local driver row whose `nvidia.raw` matches its record.
fn put_checksum_deadline_row(h: &FakeHost) -> String {
    let id = "20260928T101530Z-3f9a0c12-build";
    let bytes = b"local row bytes";
    h.put_file(
        &format!("/var/lib/nvm/drivers/{id}/nvidia.raw"),
        bytes.to_vec(),
    );
    h.put_text(
        &format!("/var/lib/nvm/drivers/{id}/record.json"),
        &json!({
            "kind": "official",
            "version": "580.178.04",
            "sha256": sha256_hex(bytes),
            "size": bytes.len(),
            "target_kernel": KERNEL,
            "created_at": 7,
            "fingerprint": null,
            "original_of": null,
            "keep": null,
        })
        .to_string(),
    );
    id.to_string()
}

fn prime_checksum_deadline_host(h: &FakeHost) {
    prime_canonical(h, "580.178.04");
    h.add_gpu("0000:01:00.0", 0x1b80, Some("nvidia"));
    prime_kernel_headers(h, KERNEL);
    h.set_curl(
        CATALOG_URL,
        200,
        b"<a href=\"580.178.04/\">580.178.04</a>\n",
    );
}

async fn plan_checksum_deadline_row(
    slow: Arc<Wrap<SlowHost>>,
    fake: FakeHost,
    id: &str,
) -> (axum::http::StatusCode, serde_json::Value) {
    let app = app_with(slow, fake, StaticFiles::default());
    let cookie = setup_app(&app, "longpass99").await;
    post_plan(&app, &cookie, json!({"kind": "local", "row_id": id})).await
}

/// The local row's whole-file sha256 has the 15 s deadline on the host clock: a dialed or
/// really-blocked read past it is a `local_checksum` timeout, never a ready plan.
#[tokio::test]
async fn a_local_checksum_read_past_its_deadline_blocks_the_plan() {
    // Dialed host clock during the row-file read.
    let host = default_host();
    prime_checksum_deadline_host(&host);
    let id = put_checksum_deadline_row(&host);
    let needle = format!("drivers/{id}/nvidia.raw");
    let slow = Arc::new(SlowHost::with_advance(
        host.clone(),
        &needle,
        Duration::from_secs(16),
    ));
    let (status, body) = plan_checksum_deadline_row(slow, host, &id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "blocked", "{body}");
    assert!(
        body["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["object"] == "local_checksum" && f["reason"]["key"] == REASON_TIMED_OUT),
        "{body}"
    );

    // The synchronous hash reader uses the host clock, including time spent blocked in open. Check
    // both sides of the budget before releasing it.
    tokio::time::pause();
    for seconds in [14, 16] {
        let host = default_host();
        prime_checksum_deadline_host(&host);
        let id = put_checksum_deadline_row(&host);
        let needle = format!("drivers/{id}/nvidia.raw");
        let gate = Arc::new(ReadGate::default());
        let release = ReleaseGate(gate.clone());
        let slow = Arc::new(SlowHost::blocking_on(host.clone(), &needle, gate.clone()));
        let app = app_with(slow, host, StaticFiles::default());
        let cookie = setup_app(&app, "longpass99").await;
        gate.armed.store(true, Ordering::SeqCst);
        let response = tokio::spawn(async move {
            post_plan(&app, &cookie, json!({"kind": "local", "row_id": id})).await
        });
        wait_until(|| gate.entered.load(Ordering::SeqCst) > 0).await;
        advance_reading_clock(seconds).await;
        assert!(
            !response.is_finished(),
            "the hash worker must still be gated"
        );
        drop(release);
        wait_until(|| response.is_finished()).await;
        let (status, body) = response.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        if seconds == 14 {
            assert_eq!(body["result"], "ready", "{body}");
        } else {
            assert_eq!(body["result"], "blocked", "{body}");
            assert!(
                body["failures"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f["object"] == "local_checksum"
                        && f["reason"]["key"] == REASON_TIMED_OUT),
                "{body}"
            );
        }
    }
}

/// Counts the bytes each thread allocates. `#[tokio::test]` runs the static-file test below on a
/// current-thread runtime, so the router's work for its requests is counted on that thread alone
/// and the other tests of the binary, running in parallel on their own threads, do not add to it.
struct CountingAlloc;

thread_local! {
    static THREAD_ALLOCATED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn count_allocation(size: usize) {
    // No allocation happens here: the counter is a const-initialised Cell.
    let _ = THREAD_ALLOCATED.try_with(|c| c.set(c.get().wrapping_add(size)));
}

fn thread_allocated() -> usize {
    THREAD_ALLOCATED.with(|c| c.get())
}

#[global_allocator]
static COUNTING: CountingAlloc = CountingAlloc;

unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        count_allocation(layout.size());
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        count_allocation(layout.size());
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        count_allocation(new_size);
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

/// Serving static files must not copy the table per request: the state every request clones shares
/// the one table, so a table of any size costs no per-request allocation beyond the response
/// bookkeeping.
#[tokio::test]
async fn static_requests_do_not_copy_the_file_table() {
    use tower::ServiceExt;

    let table_bytes = 64 * 1024 * 1024;
    let host = default_host();
    let mut files = StaticFiles::default();
    files.files.insert(
        "index.html".to_string(),
        (vec![b'x'; table_bytes].into(), "text/html; charset=utf-8"),
    );
    let app = app_with(Arc::new(host.clone()) as Arc<dyn Host>, host, files);
    let make_request = || {
        axum::http::Request::builder()
            .method("GET")
            .uri("/")
            .header(axum::http::header::HOST, "nas.example:8199")
            .body(Body::empty())
            .unwrap()
    };

    let before = thread_allocated();
    for _ in 0..3 {
        let response = app.router.clone().oneshot(make_request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
    }
    let delta = thread_allocated() - before;
    assert!(
        delta < table_bytes / 2,
        "one table copy is {table_bytes} bytes; three requests allocated {delta}"
    );
}

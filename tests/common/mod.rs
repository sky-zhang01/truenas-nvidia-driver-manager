#![allow(dead_code)]
//! Shared helpers for the fake-host integration tests.

pub mod fake_host;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use serde_json::Value;
use tower::ServiceExt;

use truenas_nvidia_driver_manager::codes::{CANONICAL_PATH, CONTAINER_DATA_DIR};
use truenas_nvidia_driver_manager::facts::MountLine;
use truenas_nvidia_driver_manager::host::{Cmd, Create, Host, HostLock, Output, SyncWrite, Watch};
use truenas_nvidia_driver_manager::plan::{Ctx, PlanSlot};
use truenas_nvidia_driver_manager::settings::Settings;
use truenas_nvidia_driver_manager::startup::{Startup, StartupProblem};
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::upload::UploadState;
use truenas_nvidia_driver_manager::web::{router, StaticFiles};

#[allow(unused_imports)]
pub use fake_host::{
    default_host, default_pci_dir, prime_default_scene, FakeHost, FakeOutput,
    CANONICAL_PATH_FOR_TESTS, DATASET, HOST_DATA_DIR, KERNEL, TRUENAS_VERSION,
};

/// A complete web process wired to a fake host.
#[derive(Clone)]
pub struct TestApp {
    pub host: FakeHost,
    pub ctx: Arc<Ctx>,
    pub router: Router,
}

/// Build a web process. `startup` problems can be injected by the caller.
pub fn app(host: FakeHost, settings: Settings, startup: Startup) -> TestApp {
    let paths = Paths::new(CONTAINER_DATA_DIR);
    let ctx = Arc::new(Ctx {
        host: Arc::new(host.clone()),
        paths,
        settings,
        startup,
        process_lock: tokio::sync::Mutex::new(()),
        plan: std::sync::Mutex::new(PlanSlot::new()),
        upload: Arc::new(UploadState::new()),
        auth: Arc::new(truenas_nvidia_driver_manager::auth::AuthState::new()),
    });
    let router = router(ctx.clone(), StaticFiles::default());
    TestApp { host, ctx, router }
}

/// Build a web process whose `Host` is a wrapper around the fake host (for delays or clock
/// advances); `fake` stays the test's handle to the same sandbox.
pub fn app_with_host(host: Arc<dyn Host>, fake: FakeHost) -> TestApp {
    let paths = Paths::new(CONTAINER_DATA_DIR);
    let ctx = Arc::new(Ctx {
        host,
        paths,
        settings: settings("hunter12"),
        startup: healthy_startup(&fake),
        process_lock: tokio::sync::Mutex::new(()),
        plan: std::sync::Mutex::new(PlanSlot::new()),
        upload: Arc::new(UploadState::new()),
        auth: Arc::new(truenas_nvidia_driver_manager::auth::AuthState::new()),
    });
    let router = router(ctx.clone(), StaticFiles::default());
    TestApp {
        host: fake,
        ctx,
        router,
    }
}

/// The startup state of a healthy host.
pub fn healthy_startup(_host: &FakeHost) -> Startup {
    Startup {
        problems: Vec::<StartupProblem>::new(),
        pid_host_ok: true,
        host_data_dir: Some(std::path::PathBuf::from(HOST_DATA_DIR)),
        image_id: Some(format!("sha256:{}", "a".repeat(64))),
    }
}

/// Settings with an initial password and the default username.
pub fn settings(password: &str) -> Settings {
    Settings {
        username: Some("admin".into()),
        password: truenas_nvidia_driver_manager::settings::InitialPassword::Usable(
            truenas_nvidia_driver_manager::settings::Secret::new(password.to_string()),
        ),
        port: 8199,
        build_timeout_minutes: Some(30),
        problems: Vec::new(),
    }
}

/// A canonical image listing with the toolkit programs and one version.
pub fn canonical_listing(kernel: &str, version: &str) -> String {
    format!(
        "drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr\n\
         drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr/lib/modules/{kernel}\n\
         -rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.{version}\n\
         -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-ctk\n\
         -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime\n\
         -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime-hook\n"
    )
}

/// Put the standard files of a healthy host into the sandbox.
pub fn prime_canonical(host: &FakeHost, version: &str) {
    let bytes = b"driver image".to_vec();
    host.put_file(CANONICAL_PATH, bytes);
    host.set_listing(CANONICAL_PATH, &canonical_listing(KERNEL, version));
    host.put_text("/data/manifest.json", r#"{"version":"25.10.7"}"#);
    host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH);
}

static SESSION_TOKENS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Remember the request token that belongs to a session cookie.
pub fn remember_session(cookie: &str, token: &str) {
    SESSION_TOKENS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(cookie.to_string(), token.to_string());
}

/// The request token issued with this cookie, when this process saw it.
pub fn token_for(cookie: &str) -> Option<String> {
    SESSION_TOKENS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(cookie)
        .cloned()
}

fn remember_issued(body: &Value, headers: &HeaderMap) {
    let Some(token) = body.get("token").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(raw) = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
    else {
        return;
    };
    let Some(cookie) = raw
        .split(';')
        .next()
        .and_then(|part| part.trim().strip_prefix("nvm_session="))
    else {
        return;
    };
    if !cookie.is_empty() {
        remember_session(cookie, token);
    }
}

fn attach_token(
    builder: axum::http::request::Builder,
    cookie: &str,
) -> axum::http::request::Builder {
    match token_for(cookie) {
        Some(token) => builder.header("x-nvm-token", token),
        None => builder,
    }
}

/// One in-process API request.
pub async fn request(
    app: &TestApp,
    method: Method,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value, HeaderMap) {
    let is_mutation = !matches!(method, Method::GET | Method::HEAD);
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header(header::HOST, "nas.example:8199")
        .header(header::ORIGIN, "http://nas.example:8199");
    if is_mutation {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("nvm_session={cookie}"));
        builder = attach_token(builder, cookie);
    }
    let body = match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    };
    let request = builder.body(body).unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    remember_issued(&value, &headers);
    (status, value, headers)
}

pub async fn get(app: &TestApp, uri: &str, cookie: Option<&str>) -> (StatusCode, Value) {
    let (s, v, _) = request(app, Method::GET, uri, None, cookie).await;
    (s, v)
}

pub async fn post(
    app: &TestApp,
    uri: &str,
    body: Value,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let (s, v, _) = request(app, Method::POST, uri, Some(body), cookie).await;
    (s, v)
}

pub async fn put(
    app: &TestApp,
    uri: &str,
    body: Value,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let (s, v, _) = request(app, Method::PUT, uri, Some(body), cookie).await;
    (s, v)
}

/// One in-process request with a raw byte body and an explicit query string.
pub async fn request_raw(
    app: &TestApp,
    method: Method,
    uri: &str,
    bytes: Vec<u8>,
    content_type: &str,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header(header::HOST, "nas.example:8199")
        .header(header::ORIGIN, "http://nas.example:8199")
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, bytes.len().to_string());
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("nvm_session={cookie}"));
        builder = attach_token(builder, cookie);
    }
    let request = builder.body(Body::from(bytes)).unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    remember_issued(&value, &headers);
    (status, value)
}

/// Sign in and return the session cookie value.
pub async fn login(app: &TestApp, user: &str, password: &str) -> String {
    let (status, body, headers) = request(
        app,
        Method::POST,
        "/api/login",
        Some(serde_json::json!({"username": user, "password": password})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login failed: {body}");
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("no Set-Cookie");
    let value = set_cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("nvm_session=")
        .to_string();
    value
}

/// Sha256 of bytes as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    truenas_nvidia_driver_manager::store::hex(&hasher.finalize())
}

/// A `docker inspect` answer for the startup check.
pub fn inspect_json(runtime: &str, mounts: &[(&str, &str)]) -> String {
    let mounts_json: Vec<String> = mounts
        .iter()
        .map(|(src, dst)| format!(r#"{{"Source":"{src}","Destination":"{dst}"}}"#))
        .collect();
    format!(
        r#"[{{"Image":"sha256:{}","HostConfig":{{"Runtime":"{runtime}"}},"Mounts":[{}]}}]"#,
        "a".repeat(64),
        mounts_json.join(",")
    )
}

/// A minimal valid plan for a state file.
pub fn sample_plan(
    source: truenas_nvidia_driver_manager::task_state::PlanSource,
) -> truenas_nvidia_driver_manager::task_state::Plan {
    use truenas_nvidia_driver_manager::task_state::*;
    Plan {
        source,
        version: "580.178.04".into(),
        kernel: KERNEL.into(),
        truenas_version: TRUENAS_VERSION.into(),
        original: OriginalChoice::Skip,
        image_id: format!("sha256:{}", "a".repeat(64)),
        build_timeout_minutes: 30,
        gpus: Vec::new(),
        kernel_module_type: None,
    }
}

/// Write a state file with the given status and phase.
pub fn write_state(
    host: &FakeHost,
    id: &str,
    status: truenas_nvidia_driver_manager::task_state::Status,
    phase: Option<truenas_nvidia_driver_manager::task_state::Phase>,
) -> truenas_nvidia_driver_manager::task_state::TaskState {
    use truenas_nvidia_driver_manager::task_state::*;
    let paths = Paths::new(CONTAINER_DATA_DIR);
    let mut state = new_state(
        id,
        1,
        sample_plan(PlanSource::Official),
        phase.unwrap_or(Phase::ReadLoadedVersion),
    );
    state.status = status;
    state.phase = phase;
    truenas_nvidia_driver_manager::task_state::write(host, &paths, &state).unwrap();
    state
}

/// Sign in, change the initial password, and return the new session cookie.
pub async fn login_changed(app: &TestApp, old: &str, new: &str) -> String {
    let cookie = login(app, "admin", old).await;
    let (status, body, headers) = request(
        app,
        Method::POST,
        "/api/password",
        Some(serde_json::json!({"new_password": new})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "password change failed: {body}");
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("no Set-Cookie after the password change");
    set_cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("nvm_session=")
        .to_string()
}

/// Build a healthy process and complete the initial password change.
pub async fn signed_in_app(host: FakeHost) -> (TestApp, String) {
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    let (status, _) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = login_changed(&app, "hunter12", "longpass99").await;
    (app, cookie)
}

/// Send a plan request with the standard envelope.
pub async fn post_plan(app: &TestApp, cookie: &str, source: Value) -> (StatusCode, Value) {
    post(
        app,
        "/api/plans",
        serde_json::json!({"source": source}),
        Some(cookie),
    )
    .await
}

/// Send a task request with the selected plan and button.
pub async fn post_task(
    app: &TestApp,
    cookie: &str,
    plan_id: impl serde::Serialize,
    button: &str,
) -> (StatusCode, Value) {
    post(
        app,
        "/api/tasks",
        serde_json::json!({"plan_id": plan_id, "button": button}),
        Some(cookie),
    )
    .await
}

/// A cloneable in-memory writer for structured log capture.
#[derive(Clone, Default)]
pub struct SharedBuf(pub Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuf {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Put the kernel-dependent header declarations into the sandbox.
pub fn prime_kernel_headers(host: &FakeHost, kernel: &str) {
    let base = "/usr/src/linux-headers-truenas-production-amd64/include";
    if kernel.starts_with("6.9") {
        host.put_text(
            &format!("{base}/linux/mm.h"),
            "int follow_pfn(struct vm_area_struct *vma, unsigned long addr, unsigned long *pfn);\n",
        );
        host.put_text(
            &format!("{base}/drm/drm_mode_config.h"),
            "struct drm_mode_config_funcs { void (*output_poll_changed)(struct drm_device *dev); };\n",
        );
    } else {
        host.put_text(
            &format!("{base}/linux/mm.h"),
            "static inline int follow_pfnmap_start(struct follow_pfnmap_args *args);\n",
        );
        host.put_text(
            &format!("{base}/drm/drm_mode_config.h"),
            "struct drm_mode_config_funcs { void (*atomic_state_free)(void); };\n",
        );
    }
    if kernel.starts_with("6.12.105") || kernel.starts_with("6.12.97") {
        host.put_text(
            &format!("{base}/linux/pci.h"),
            "int pci_resize_resource(struct pci_dev *dev, int resno, int size,\n                        u32 exclude_bars);\n",
        );
    } else {
        host.put_text(
            &format!("{base}/linux/pci.h"),
            "int pci_resize_resource(struct pci_dev *dev, int resno, int size);\n",
        );
    }
}

/// A complete build output that passes delivery checks.
pub fn put_build_output(host: &FakeHost, id: &str, version: &str, artifact: &[u8]) {
    let out = format!("{HOST_DATA_DIR}/manager/tasks/{id}/out");
    host.put_file(&format!("{out}/nvidia.raw"), artifact.to_vec());
    host.put_text(
        &format!("{out}/result.txt"),
        &format!(
            "ARTIFACT_SHA256={}\nARTIFACT_BYTES={}\n",
            sha256_hex(artifact),
            artifact.len()
        ),
    );
    let ko = format!("usr/lib/modules/{KERNEL}/kernel/drivers/video/nvidia.ko");
    let listing = format!(
        "drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr\n\
         drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr/lib/modules/{KERNEL}\n\
         -rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/{ko}\n\
         -rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.{version}\n\
         -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-ctk\n\
         -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime\n\
         -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime-hook\n"
    );
    host.set_listing(&format!("{out}/nvidia.raw"), &listing);
    host.set_modinfo(
        &format!("{HOST_DATA_DIR}/manager/tasks/{id}/inspect/{ko}"),
        version,
    );
}

/// A toolkit package index and its download responses.
pub fn toolkit_index() -> (String, Vec<(String, Vec<u8>)>) {
    toolkit_index_at("1.20.1-1")
}

pub fn toolkit_index_at(version: &str) -> (String, Vec<(String, Vec<u8>)>) {
    let files: Vec<(&str, &[u8])> = vec![
        ("libnvidia-container1", b"one"),
        ("libnvidia-container-tools", b"two"),
        ("nvidia-container-toolkit-base", b"three"),
        ("nvidia-container-toolkit", b"four"),
    ];
    let mut index = String::new();
    let mut responses = Vec::new();
    for (name, body) in &files {
        index.push_str(&format!(
            "Package: {name}\nArchitecture: amd64\nVersion: {version}\nFilename: ./{name}_{version}_amd64.deb\nSize: {}\nSHA256: {}\n\n",
            body.len(),
            sha256_hex(body)
        ));
        responses.push((
            format!("https://nvidia.github.io/libnvidia-container/stable/deb/amd64/{name}_{version}_amd64.deb"),
            body.to_vec(),
        ));
    }
    (index, responses)
}

/// Register the toolkit and official driver download responses.
pub fn official_downloads(host: &FakeHost, version: &str) {
    let (index, responses) = toolkit_index();
    host.set_curl(
        truenas_nvidia_driver_manager::codes::TOOLKIT_INDEX_URL,
        200,
        index.as_bytes(),
    );
    for (url, body) in responses {
        host.set_curl(&url, 200, &body);
    }
    host.set_curl(
        &format!(
            "{}{}/NVIDIA-Linux-x86_64-{version}-no-compat32.run",
            truenas_nvidia_driver_manager::codes::CATALOG_URL,
            version
        ),
        200,
        b"installer",
    );
}

/// Override selected host operations while forwarding every other operation.
pub trait Decorate: Send + Sync {
    fn inner(&self) -> &dyn Host;
    fn run(&self, cmd: &Cmd) -> Output {
        self.inner().run(cmd)
    }
    fn run_watch(&self, cmd: &Cmd, every: Duration, w: &mut dyn Watch) -> Output {
        self.inner().run_watch(cmd, every, w)
    }
    fn view(&self, host_path: &Path) -> PathBuf {
        self.inner().view(host_path)
    }
    fn mount_table(&self) -> io::Result<Vec<MountLine>> {
        self.inner().mount_table()
    }
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.inner().read(p)
    }
    fn open(&self, p: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.inner().open(p)
    }
    fn create(&self, p: &Path, how: Create, mode: u32) -> io::Result<Box<dyn SyncWrite + Send>> {
        self.inner().create(p, how, mode)
    }
    fn append(&self, p: &Path, mode: u32) -> io::Result<Box<dyn Write + Send>> {
        self.inner().append(p, mode)
    }
    fn lstat(&self, p: &Path) -> io::Result<std::fs::Metadata> {
        self.inner().lstat(p)
    }
    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        self.inner().read_link(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<Vec<OsString>> {
        self.inner().read_dir(p)
    }
    fn create_dir(&self, p: &Path, mode: u32) -> io::Result<()> {
        self.inner().create_dir(p, mode)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner().rename(from, to)
    }
    fn remove_file(&self, p: &Path) -> io::Result<()> {
        self.inner().remove_file(p)
    }
    fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
        self.inner().remove_dir_all(p)
    }
    fn fsync_dir(&self, p: &Path) -> io::Result<()> {
        self.inner().fsync_dir(p)
    }
    fn fsync_file(&self, p: &Path) -> io::Result<()> {
        self.inner().fsync_file(p)
    }
    fn uname_release(&self) -> io::Result<String> {
        self.inner().uname_release()
    }
    fn now(&self) -> Instant {
        self.inner().now()
    }
    fn wall_ms(&self) -> u64 {
        self.inner().wall_ms()
    }
    fn open_lock(&self, host_path: &Path, mode: u32) -> io::Result<Box<dyn HostLock + Send>> {
        self.inner().open_lock(host_path, mode)
    }
}

/// A host backed by a selective decorator.
pub struct Wrap<D>(pub D);

impl<D: Decorate> Host for Wrap<D> {
    fn run(&self, cmd: &Cmd) -> Output {
        self.0.run(cmd)
    }
    fn run_watch(&self, cmd: &Cmd, every: Duration, w: &mut dyn Watch) -> Output {
        self.0.run_watch(cmd, every, w)
    }
    fn view(&self, host_path: &Path) -> PathBuf {
        self.0.view(host_path)
    }
    fn mount_table(&self) -> io::Result<Vec<MountLine>> {
        self.0.mount_table()
    }
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.0.read(p)
    }
    fn open(&self, p: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.0.open(p)
    }
    fn create(&self, p: &Path, how: Create, mode: u32) -> io::Result<Box<dyn SyncWrite + Send>> {
        self.0.create(p, how, mode)
    }
    fn append(&self, p: &Path, mode: u32) -> io::Result<Box<dyn Write + Send>> {
        self.0.append(p, mode)
    }
    fn lstat(&self, p: &Path) -> io::Result<std::fs::Metadata> {
        self.0.lstat(p)
    }
    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        self.0.read_link(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<Vec<OsString>> {
        self.0.read_dir(p)
    }
    fn create_dir(&self, p: &Path, mode: u32) -> io::Result<()> {
        self.0.create_dir(p, mode)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.0.rename(from, to)
    }
    fn remove_file(&self, p: &Path) -> io::Result<()> {
        self.0.remove_file(p)
    }
    fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
        self.0.remove_dir_all(p)
    }
    fn fsync_dir(&self, p: &Path) -> io::Result<()> {
        self.0.fsync_dir(p)
    }
    fn fsync_file(&self, p: &Path) -> io::Result<()> {
        self.0.fsync_file(p)
    }
    fn uname_release(&self) -> io::Result<String> {
        self.0.uname_release()
    }
    fn now(&self) -> Instant {
        self.0.now()
    }
    fn wall_ms(&self) -> u64 {
        self.0.wall_ms()
    }
    fn open_lock(&self, host_path: &Path, mode: u32) -> io::Result<Box<dyn HostLock + Send>> {
        self.0.open_lock(host_path, mode)
    }
}

/// Release a blocked fake command when a test leaves its scope.
pub struct ReleaseGate(pub Arc<fake_host::Gate>);

impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}

pub const RELEASE_X: &str = "20261005T101530Z-abcdef12";
pub const RELEASE_L: &str = "20261006T101530Z-abcdef34";
pub const RELEASE_DATASET: &str = "tank/ROOT/env/usr";
pub const RELEASE_VERSION: &str = "580.178.04";

/// A write interrupted before its check, followed by a fully verified replacement.
pub fn release_scene(host: &FakeHost) {
    use truenas_nvidia_driver_manager::task_state::*;
    let paths = Paths::new(CONTAINER_DATA_DIR);
    let mut x = write_state(host, RELEASE_X, Status::Interrupted, Some(Phase::Write));
    x.rootfs = Some(Rootfs {
        dataset: RELEASE_DATASET.into(),
        readonly_before: "on".into(),
    });
    x.replace.push(ReplaceRecord {
        pass: Pass::Update,
        unloaded: vec!["nvidia_uvm".into()],
        reloaded: vec![],
        refresh_exit: None,
        ldconfig_exit: None,
        readback_ok: None,
        readonly_after: None,
    });
    truenas_nvidia_driver_manager::task_state::write(host, &paths, &x).unwrap();
    host.mkdir(paths.task_dir(RELEASE_X).to_str().unwrap());
    host.put_text(
        paths.state_log(RELEASE_X).to_str().unwrap(),
        "kept task log\n",
    );
    prime_canonical(host, RELEASE_VERSION);
    host.set_driver_version(RELEASE_VERSION);
    let bytes = b"driver image";
    let row = paths.row_dir(&format!("{RELEASE_L}-build"));
    host.put_file(row.join("nvidia.raw").to_str().unwrap(), bytes.to_vec());
    let record = truenas_nvidia_driver_manager::drivers::Record {
        kind: RowKind::Official,
        version: RELEASE_VERSION.into(),
        sha256: sha256_hex(bytes),
        size: bytes.len() as u64,
        target_kernel: KERNEL.into(),
        created_at: 2,
        fingerprint: None,
        original_of: None,
        keep: None,
    };
    host.put_text(
        row.join("record.json").to_str().unwrap(),
        &serde_json::to_string(&record).unwrap(),
    );
    let mut l = write_state(host, RELEASE_L, Status::Succeeded, Some(Phase::Check));
    l.created_at = 2;
    l.finished_at = Some(3);
    l.reload_targets = Some(ReloadTargetsJson {
        basis: "unloaded_in_update".into(),
        modules: vec!["nvidia".into(), "nvidia_uvm".into()],
    });
    l.replace.push(ReplaceRecord {
        pass: Pass::Update,
        unloaded: vec!["nvidia_uvm".into()],
        reloaded: vec![ReloadedRecord {
            module: "nvidia_uvm".into(),
            ok: true,
            reason: None,
        }],
        refresh_exit: Some(serde_json::json!({"code":0})),
        ldconfig_exit: Some(serde_json::json!({"code":0})),
        readback_ok: Some(true),
        readonly_after: Some(ReadingJson::value("on".into())),
    });
    l.checks.push(CheckRecord {
        pass: Pass::Update,
        since_before: ReadingJson::value(host.sysext_since() - 1),
        since_after: ReadingJson::value(host.sysext_since()),
        refresh_ok: true,
        nvidia_smi: ReadingJson::value(RELEASE_VERSION.into()),
        proc_version: ReadingJson::value(RELEASE_VERSION.into()),
        expected: RELEASE_VERSION.into(),
        version_ok: true,
        passed: true,
    });
    truenas_nvidia_driver_manager::task_state::write(host, &paths, &l).unwrap();
}

pub fn release_state(
    host: &FakeHost,
    id: &str,
) -> truenas_nvidia_driver_manager::task_state::TaskState {
    match truenas_nvidia_driver_manager::task_state::read(host, &Paths::new(CONTAINER_DATA_DIR), id)
    {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!("read release fixture: {other:?}"),
    }
}

/// Make the later task's effective verified pass restore its saved original.
pub fn release_restore_scene(host: &FakeHost) {
    use truenas_nvidia_driver_manager::task_state::*;
    release_scene(host);
    let paths = Paths::new(CONTAINER_DATA_DIR);
    let row_id = format!("{RELEASE_L}-original");
    let row = paths.row_dir(&row_id);
    host.put_file(
        row.join("nvidia.raw").to_str().unwrap(),
        b"driver image".to_vec(),
    );
    let record = truenas_nvidia_driver_manager::drivers::Record {
        kind: RowKind::Original,
        version: RELEASE_VERSION.into(),
        sha256: sha256_hex(b"driver image"),
        size: 12,
        target_kernel: KERNEL.into(),
        created_at: 2,
        fingerprint: None,
        original_of: Some(truenas_nvidia_driver_manager::drivers::OriginalOf {
            kernel: KERNEL.into(),
            truenas_version: TRUENAS_VERSION.into(),
        }),
        keep: Some(true),
    };
    host.put_text(
        row.join("record.json").to_str().unwrap(),
        &serde_json::to_string(&record).unwrap(),
    );
    let mut l = release_state(host, RELEASE_L);
    l.status = Status::StockRestored;
    l.replace[0].pass = Pass::Restore;
    let mut restored_check = l.checks[0].clone();
    restored_check.pass = Pass::Restore;
    l.checks.push(restored_check);
    l.restore = Some(Restore {
        decision: RestoreDecision::Restore,
        reason: None,
        read_reason: None,
        original: Some(RestoreOriginal {
            row_id,
            version: RELEASE_VERSION.into(),
            saved_at: 2,
        }),
    });
    truenas_nvidia_driver_manager::task_state::write(host, &paths, &l).unwrap();
}

/// Inject a task-directory removal failure without changing file removals.
pub struct ReleaseCleanupFailure {
    pub host: FakeHost,
    pub fail: Arc<std::sync::atomic::AtomicBool>,
}
impl Decorate for ReleaseCleanupFailure {
    fn inner(&self) -> &dyn Host {
        &self.host
    }
    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        if path.ends_with(RELEASE_X) && self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(io::Error::other("task directory removal refused"));
        }
        self.host.remove_dir_all(path)
    }
}

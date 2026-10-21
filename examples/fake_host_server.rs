//! Test service for the Playwright browser tests.
//!
//! It runs the real web routes on a [`FakeHost`](common::FakeHost) and adds the `/__fake/`
//! control endpoints used by the specs:
//!
//! - `POST /__fake/scenario {"name": "..."}` selects a preset scene.
//! - `GET /__fake/state` returns the recorded commands and units.
//! - `POST /__fake/clock {"advance_ms": n}` advances the fake clock.
//!
//! Usage: `cargo run --example fake_host_server -- --port 18109 --data-dir <tmp>`

#[path = "../tests/common/mod.rs"]
mod common;

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response as AxumResponse;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{
    canonical_listing, default_host, default_pci_dir, healthy_startup, prime_canonical, sha256_hex,
    FakeHost, HOST_DATA_DIR, KERNEL, TRUENAS_VERSION,
};
use truenas_nvidia_driver_manager::codes::{CANONICAL_PATH, CATALOG_URL};
use truenas_nvidia_driver_manager::host::Host;
use truenas_nvidia_driver_manager::plan::{Ctx, PlanSlot};
use truenas_nvidia_driver_manager::settings::Settings;
use truenas_nvidia_driver_manager::startup::Startup;
use truenas_nvidia_driver_manager::store::{Paths, Validate};
use truenas_nvidia_driver_manager::upload::UploadState;
use truenas_nvidia_driver_manager::web::{load_static_files, router, StaticFiles};

const INDEX: &str = r#"<a href="615.71.09/">615.71.09</a>
<a href="610.57.04/">610.57.04</a>
<a href="595.104.02/">595.104.02</a>
<a href="590.48.01/">590.48.01</a>
<a href="580.178.04/">580.178.04</a>
<a href="575.64.05/">575.64.05</a>
<a href="570.211.01/">570.211.01</a>
<a href="565.77/">565.77</a>
<a href="550.163.01/">550.163.01</a>
<a href="470.256.02/">470.256.02</a>
"#;

struct FakeLog(common::SharedBuf);

impl Write for FakeLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(bytes)?;
        std::io::stdout().lock().write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().lock().flush()
    }
}

struct ServerState {
    host: FakeHost,
    scenario: Mutex<String>,
    /// The live web process. Rebuilt with a fresh `AuthState` on every scenario switch so the
    /// login limiter, the logged-out table and the SSH challenge table never leak across
    /// scenes.
    inner: Mutex<Inner>,
    logs: common::SharedBuf,
}

struct Inner {
    ctx: Arc<Ctx>,
    normal: Router,
    setup_problem: Router,
    /// Incremented on every rebuild; the specs assert it to prove the isolation actually
    /// happened, not just that the name changed.
    auth_epoch: u64,
    /// The loaded static assets, shared by every rebuild.
    web_root: StaticFiles,
    /// The compose-like settings, reused by every rebuild.
    settings: Settings,
}

/// Build one web process (both routers) on a fresh `AuthState`.
fn build_routers(
    host: &FakeHost,
    settings: Settings,
    web_root: StaticFiles,
) -> (Arc<Ctx>, Router, Router) {
    let healthy = healthy_startup(host);
    let mut broken = Startup {
        problems: Vec::new(),
        pid_host_ok: false,
        host_data_dir: None,
        image_id: Some(format!("sha256:{}", "a".repeat(64))),
    };
    broken
        .problems
        .push(truenas_nvidia_driver_manager::startup::StartupProblem::PidHost);
    let auth = Arc::new(truenas_nvidia_driver_manager::auth::AuthState::new());
    let make = |startup: Startup| -> Arc<Ctx> {
        Arc::new(Ctx {
            host: Arc::new(host.clone()),
            paths: Paths::new("/var/lib/nvm"),
            settings: settings.clone(),
            startup,
            process_lock: tokio::sync::Mutex::new(()),
            gpu_users_cache: Default::default(),
            plan: std::sync::Mutex::new(PlanSlot::new()),
            upload: Arc::new(UploadState::new()),
            auth: auth.clone(),
        })
    };
    let ctx_normal = make(healthy);
    let ctx_setup = make(broken);
    if let Some(issued) = auth.ensure_setup_code(host, &ctx_normal.paths, &settings, "start") {
        host.forbid_in_argv(&issued.code);
        issued.log();
    }
    let normal = router(ctx_normal.clone(), web_root.clone());
    let setup_problem = router(ctx_setup, web_root);
    (ctx_normal, normal, setup_problem)
}

fn prime_common(host: &FakeHost) {
    default_pci_dir(host);
    prime_canonical(host, "580.173.02");
    host.set_curl(CATALOG_URL, 200, INDEX.as_bytes());
    for (name, section) in [
        (
            "mm.h",
            "static inline int follow_pfnmap_start(struct follow_pfnmap_args *args);\n",
        ),
        (
            "drm_mode_config.h",
            "struct drm_mode_config_funcs { void (*atomic_state_free)(void); };\n",
        ),
    ] {
        let dir = if name == "mm.h" { "linux" } else { "drm" };
        host.put_text(
            &format!("/usr/src/linux-headers-truenas-production-amd64/include/{dir}/{name}"),
            section,
        );
    }
    host.put_text(
        "/usr/src/linux-headers-truenas-production-amd64/include/linux/pci.h",
        "int pci_resize_resource(struct pci_dev *dev, int resno, int size,\n                        u32 exclude_bars);\n",
    );
}

fn prepare(host: &FakeHost, scenario: &str) {
    host.set_dpkg_fixture_content(matches!(scenario, "upload_files" | "upload_rejected"));
    host.remove("/var/lib/nvm/manager/secrets/web-credentials.json");
    // apply_reset wipes /run; every scene except no_link gets the link back.
    if scenario != "no_link" {
        host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH);
    }
    common::fake_host::prime_kernel_scene(
        host,
        KERNEL,
        TRUENAS_VERSION,
        "gcc (Debian 12.2.0-14+deb12u1) 12.2.0",
    );
    match scenario {
        "gpu_wait" => {
            prepare_local_replacement(host, KERNEL, TRUENAS_VERSION);
            host.load_module("nvidia", 1, &[]);
            let active = "a".repeat(64);
            let idle = "b".repeat(64);
            host.set_docker_ps(&[&active, &idle]);
            host.set_docker_inspect(&json!([
                {"Id":active,"Name":"/gpu-worker","Config":{"Env":["NVIDIA_VISIBLE_DEVICES=all"]}},
                {"Id":idle,"Name":"/media-idle","Config":{"Env":["NVIDIA_VISIBLE_DEVICES=all"]}}
            ]).to_string());
            host.preset(
                "docker",
                &["top", &active, "-eo", "pid"],
                common::FakeOutput::stdout("PID\n101\n"),
            );
            host.preset(
                "docker",
                &["top", &idle, "-eo", "pid"],
                common::FakeOutput::stdout("PID\n202\n"),
            );
            host.put_link("/proc/101/fd/3", "/dev/nvidia0");
            host.put_link("/proc/202/fd/0", "/dev/null");
        }
        "truenas27" | "truenas27_replacement" => {
            let kernel = "6.18.52-production+truenas";
            common::fake_host::prime_kernel_scene(
                host,
                kernel,
                "27.0.0-RC.1",
                "gcc (Debian 14.2.0-19) 14.2.0",
            );
            host.set_listing(CANONICAL_PATH, &canonical_listing(kernel, "580.173.02"));
            if scenario == "truenas27_replacement" {
                prepare_local_replacement(host, kernel, "27.0.0-RC.1");
            }
        }
        "upload_files" | "upload_rejected" => {
            // These scenarios must start idle even after the GPU-wait walkthrough.
            prime_canonical(host, "580.173.02");
            host.model_sysext_merge();
            for module in ["nvidia_drm", "nvidia_modeset", "nvidia_uvm"] {
                host.unload_module(module);
            }
            host.load_module("nvidia", 0, &[]);
            host.set_driver_version("580.173.02");
            host.set_docker_ps(&[]);
            host.set_docker_inspect("[]");
        }
        "fresh" | "no_link" | "upload_stage" | "drivers_rows" | "setup_problem" => {}
        "readme_update" => host.add_gpu("0000:01:00.0", 0x2684, Some("nvidia")),
        "pascal" => host.add_gpu("0000:01:00.0", 0x1b80, Some("nvidia")),
        "kepler" => host.add_gpu("0000:01:00.0", 0x1180, Some("nvidia")),
        "mixed_1b80_2b85" => {
            host.add_gpu("0000:01:00.0", 0x1b80, Some("nvidia"));
            host.add_gpu("0000:02:00.0", 0x2b85, None);
        }
        "mixed_1b07_2684" => {
            host.add_gpu("0000:01:00.0", 0x1b07, Some("nvidia"));
            host.add_gpu("0000:02:00.0", 0x2684, Some("nvidia"));
        }
        "vfio_1b80_2b85" => {
            host.add_gpu("0000:01:00.0", 0x1b80, Some("vfio-pci"));
            host.add_gpu("0000:02:00.0", 0x2b85, Some("nvidia"));
        }
        "blackwell_612_96" => {
            host.set_uname("6.12.96-production+truenas");
            host.add_gpu("0000:01:00.0", 0x2b85, Some("nvidia"));
            host.put_text(
                "/usr/src/linux-headers-truenas-production-amd64/include/linux/pci.h",
                "int pci_resize_resource(struct pci_dev *dev, int resno, int size);\n",
            );
        }
        _ => {}
    }
    match scenario {
        "local_replacement" => prepare_local_replacement(host, KERNEL, TRUENAS_VERSION),
        "readme_landing" => {
            use truenas_nvidia_driver_manager::task_state::{
                self, CheckRecord, ObservedGpu, OriginalChoice, Pass, Phase, Plan, PlanSource,
                ReadingJson, Status,
            };
            prepare_local_replacement(host, KERNEL, TRUENAS_VERSION);
            host.add_gpu("0000:01:00.0", 0x2684, Some("nvidia"));
            host.set_wall_ms(1_790_000_060_000);
            for row in ["original", "build"] {
                let path =
                    format!("{HOST_DATA_DIR}/drivers/20260928T101530Z-3f9a0c12-{row}/record.json");
                let mut record: Value =
                    serde_json::from_slice(&host.read(std::path::Path::new(&path)).unwrap())
                        .unwrap();
                record["created_at"] = json!(1_789_999_000_000_u64);
                host.put_text(&path, &record.to_string());
            }
            let version = "580.178.04";
            host.put_file(CANONICAL_PATH, b"agent requested driver image".to_vec());
            host.set_listing(CANONICAL_PATH, &canonical_listing(KERNEL, version));
            host.set_driver_version(version);
            let mut task = task_state::new_state(
                "20260921T141320Z-3f9a0c12",
                1_790_000_000_000,
                Plan {
                    source: PlanSource::Official,
                    version: version.into(),
                    kernel: KERNEL.into(),
                    truenas_version: TRUENAS_VERSION.into(),
                    original: OriginalChoice::Save,
                    image_id: format!("sha256:{}", "a".repeat(64)),
                    build_timeout_minutes: 30,
                    gpus: Vec::new(),
                    kernel_module_type: None,
                },
                Phase::Check,
            );
            task.status = Status::Succeeded;
            task.finished_at = Some(1_790_000_060_000);
            task.loaded_version_start = Some(ReadingJson::value("580.173.02".into()));
            task.loaded_version_end = Some(ReadingJson::value(version.into()));
            task.nvidia_smi_gpus = Some(ReadingJson::value(vec![ObservedGpu {
                name: "NVIDIA GeForce RTX 4090".into(),
                pci_address: "00000000:01:00.0".into(),
            }]));
            task.checks.push(CheckRecord {
                pass: Pass::Update,
                since_before: ReadingJson::value(1_790_000_000_000_000),
                since_after: ReadingJson::value(1_790_000_010_000_000),
                refresh_ok: true,
                nvidia_smi: ReadingJson::value(version.into()),
                proc_version: ReadingJson::value(version.into()),
                expected: version.into(),
                version_ok: true,
                passed: true,
            });
            task.validate().expect("valid README success fixture");
            let paths = Paths::new(HOST_DATA_DIR);
            task_state::write(host, &paths, &task).expect("write README success fixture");
            task_state::log(
                host,
                &paths,
                &task.task_id,
                truenas_nvidia_driver_manager::codes::LogKey::Finished,
                json!({"status": "SUCCEEDED"}),
            );
        }
        "module_race_update" | "module_race_restore" => {
            prepare_local_replacement(host, KERNEL, TRUENAS_VERSION);
            let restore = scenario == "module_race_restore";
            host.autoload_on_write(restore);
            if restore {
                host.preset_once("nvidia-smi", &[], common::FakeOutput::code(18));
            }
        }
        "no_link" => host.remove("/run/extensions/nvidia.raw"),
        "running_task" | "readme_running" => {
            if scenario == "readme_running" {
                host.add_gpu("0000:01:00.0", 0x2684, Some("nvidia"));
                prime_canonical(host, "580.173.02");
                host.set_driver_version("580.173.02");
                host.set_wall_ms(1_790_000_000_000);
            }
            let _ = host.set_gate("curl", &["--silent"]);
            let id = "20260928T101530Z-3f9a0c12";
            let state = truenas_nvidia_driver_manager::task_state::new_state(
                id,
                if scenario == "readme_running" {
                    1_790_000_000_000
                } else {
                    1
                },
                truenas_nvidia_driver_manager::task_state::Plan {
                    source: truenas_nvidia_driver_manager::task_state::PlanSource::Official,
                    version: "580.178.04".into(),
                    kernel: KERNEL.into(),
                    truenas_version: TRUENAS_VERSION.into(),
                    original: truenas_nvidia_driver_manager::task_state::OriginalChoice::Skip,
                    image_id: format!("sha256:{}", "a".repeat(64)),
                    build_timeout_minutes: 30,
                    gpus: Vec::new(),
                    kernel_module_type: None,
                },
                truenas_nvidia_driver_manager::task_state::Phase::Download,
            );
            let paths = Paths::new(HOST_DATA_DIR);
            let _ = truenas_nvidia_driver_manager::task_state::write(host, &paths, &state);
            let _ = truenas_nvidia_driver_manager::tasks::start(
                host,
                std::path::Path::new(HOST_DATA_DIR),
                id,
            );
        }
        "drivers_rows" => {
            let rows = [
                (
                    "20260928T101530Z-3f9a0c11-build",
                    "official",
                    "580.173.02",
                    &"a".repeat(64)[..],
                    "580.173.02",
                ),
                (
                    "20260928T101530Z-3f9a0c12-build",
                    "official",
                    "580.178.04",
                    &"c".repeat(64)[..],
                    "580.178.04",
                ),
                (
                    "20260928T101530Z-3f9a0c13-build",
                    "official",
                    "580.95.05",
                    &"d".repeat(64)[..],
                    "580.95.05",
                ),
            ];
            for (id, kind, version, sha, _label) in rows {
                let bytes = format!("payload of {id}");
                host.put_text(&format!("/var/lib/nvm/drivers/{id}/nvidia.raw"), &bytes);
                let stored_sha = if id.ends_with("c11-build") {
                    sha256_hex(b"driver image")
                } else {
                    sha.to_string()
                };
                let target_kernel = if id.ends_with("c13-build") {
                    "6.12.96-production+truenas".to_string()
                } else {
                    KERNEL.to_string()
                };
                host.put_text(
                    &format!("/var/lib/nvm/drivers/{id}/record.json"),
                    &format!(
                        r#"{{"kind":"{kind}","version":"{version}","sha256":"{stored_sha}","size":{},"target_kernel":"{target_kernel}","created_at":1000,"fingerprint":null,"original_of":null,"keep":null}}"#,
                        bytes.len()
                    ),
                );
            }
            host.put_text(
                "/var/lib/nvm/drivers/20260928T101530Z-3f9a0c14-original/nvidia.raw",
                "original bytes",
            );
            host.put_text(
                "/var/lib/nvm/drivers/20260928T101530Z-3f9a0c14-original/record.json",
                &format!(
                    r#"{{"kind":"original","version":"580.173.02","sha256":"{}","size":14,"target_kernel":"{KERNEL}","created_at":900,"fingerprint":null,"original_of":{{"kernel":"{KERNEL}","truenas_version":"{TRUENAS_VERSION}"}},"keep":true}}"#,
                    sha256_hex(b"original bytes")
                ),
            );
        }
        _ => {}
    }
}

/// Prepare checksum-valid local rows and a replaceable host for an HTTP walkthrough.
fn prepare_local_replacement(host: &FakeHost, kernel: &str, truenas: &str) {
    host.set_uname(kernel);
    let original_bytes = b"driver image";
    host.put_file(CANONICAL_PATH, original_bytes.to_vec());
    host.set_listing(CANONICAL_PATH, &canonical_listing(kernel, "580.173.02"));
    // Each HTTP scenario starts with its own merged view, independent of the preceding task.
    host.model_sysext_merge();
    host.put_text(
        "/data/manifest.json",
        &json!({"version": truenas}).to_string(),
    );
    host.remove_mount("/usr");
    host.add_mount("tank/ROOT/default/usr", "/usr", "zfs", "ro");
    for module in ["nvidia_drm", "nvidia_modeset", "nvidia_uvm"] {
        host.unload_module(module);
    }
    host.load_module("nvidia", 0, &[]);
    host.set_driver_version("580.173.02");
    for (id, kind, version, bytes) in [
        (
            "20260928T101530Z-3f9a0c12-original",
            "original",
            "580.173.02",
            original_bytes.as_slice(),
        ),
        (
            "20260928T101530Z-3f9a0c12-build",
            "official",
            "580.178.04",
            b"agent requested driver image".as_slice(),
        ),
    ] {
        let row = Paths::new(HOST_DATA_DIR).row_dir(id);
        let image = row.join("nvidia.raw");
        host.put_file(image.to_str().unwrap(), bytes.to_vec());
        host.set_listing(image.to_str().unwrap(), &canonical_listing(kernel, version));
        host.put_text(
            row.join("record.json").to_str().unwrap(),
            &json!({
                "kind": kind,
                "version": version,
                "sha256": sha256_hex(bytes),
                "size": bytes.len(),
                "target_kernel": kernel,
                "created_at": 7,
                "fingerprint": null,
                "original_of": (kind == "original").then(|| json!({
                    "kernel": kernel, "truenas_version": truenas,
                })),
                "keep": (kind == "original").then_some(true),
            })
            .to_string(),
        );
    }
}

/// Stage one complete upload group so current upload/the upload dialog show it. The
/// `candidates` list is what the `.run` label carries; an empty list models a label without a
/// version, which the dialog asks the user to type.
fn stage_upload(state: &ServerState, toolkit: &str, candidates: &[&str]) {
    let paths = Paths::new(HOST_DATA_DIR);
    let upload = {
        let inner = state.inner.lock().unwrap();
        inner.ctx.upload.clone()
    };
    let group =
        match truenas_nvidia_driver_manager::upload::open_group(&state.host, &paths, &upload) {
            Ok(g) => g,
            Err(_) => return,
        };
    let run: Vec<u8> = match candidates.first() {
        Some(driver) => format!(
            "#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 {driver}\"\n"
        )
        .into_bytes(),
        None => {
            b"#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64\"\n".to_vec()
        }
    };
    state.host.put_file(
        &format!("/var/lib/nvm/manager/upload/{group}/driver.run"),
        run.clone(),
    );
    let mut debs = serde_json::Map::new();
    for package in truenas_nvidia_driver_manager::codes::TOOLKIT_PACKAGES {
        let body = format!("deb of {package}");
        state.host.put_file(
            &format!("/var/lib/nvm/manager/upload/{group}/toolkit/{package}.deb"),
            body.as_bytes().to_vec(),
        );
        debs.insert(
            package.to_string(),
            json!({
                "sha256": sha256_hex(body.as_bytes()),
                "size": body.len(),
                "version": format!("{toolkit}-1"),
            }),
        );
    }
    let group_json = json!({
        "run": {
            "sha256": sha256_hex(&run),
            "size": run.len(),
            "candidates": candidates,
        },
        "debs": debs,
    });
    state.host.put_text(
        &format!("/var/lib/nvm/manager/upload/{group}/group.json"),
        &group_json.to_string(),
    );
}

/// Every request goes through the dispatcher so the scenario can select the
/// setup-problem router without rebuilding the listener.
async fn dispatch(
    State(state): State<Arc<ServerState>>,
    request: axum::extract::Request,
) -> AxumResponse {
    let router = {
        let scenario = state.scenario.lock().unwrap().clone();
        let inner = state.inner.lock().unwrap();
        if scenario == "setup_problem" {
            inner.setup_problem.clone()
        } else {
            inner.normal.clone()
        }
    };
    router
        .oneshot(request)
        .await
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "dispatch failed").into_response())
}

async fn scenario(State(state): State<Arc<ServerState>>, Json(body): Json<Value>) -> Response {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("fresh")
        .to_string();
    // Reset the sandbox by rebuilding the virtual state on the same host. The
    // reset joins the previous scenario's runner threads, so it runs on the
    // blocking pool instead of stalling the async runtime.
    let host_for_reset = state.host.clone();
    let name_for_prepare = name.clone();
    let reset = tokio::task::spawn_blocking(move || -> Result<(), String> {
        apply_reset(&host_for_reset)?;
        prepare(&host_for_reset, &name_for_prepare);
        Ok(())
    })
    .await;
    match reset {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": message})),
            )
                .into_response()
        }
        Err(join) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": format!("the scenario reset failed: {join}")})),
            )
                .into_response()
        }
    }
    *state.scenario.lock().unwrap() = name.clone();
    // Rebuild the whole web process with a fresh AuthState: the limiter, logged_out and the
    // SSH challenges of the previous scene die here. The epoch is read back through
    // `/__fake/state`. The rebuild runs
    // before the upload staging: staging registers the group in the upload state of the (new)
    // live context, so doing it before the rebuild would drop the staged group again.
    {
        let mut inner = state.inner.lock().unwrap();
        let (ctx, normal, setup_problem) =
            build_routers(&state.host, inner.settings.clone(), inner.web_root.clone());
        inner.ctx = ctx;
        inner.normal = normal;
        inner.setup_problem = setup_problem;
        inner.auth_epoch += 1;
    }
    match name.as_str() {
        "upload_stage" => stage_upload(&state, "1.20.1", &["580.178.04"]),
        "upload_below_floor" => stage_upload(&state, "1.19.0", &["615.71.09"]),
        "upload_multi" => stage_upload(&state, "1.20.1", &["580.178.04", "590.48.01"]),
        "upload_manual" => stage_upload(&state, "1.20.1", &[]),
        _ => {}
    }
    Json(json!({"ok": true})).into_response()
}

/// Reset the virtual host into an empty state. The previous scenario's runner
/// threads are stopped **and joined** first: `clear_units` only clears the map,
/// and a still-running thread would keep writing state and issuing commands
/// inside the next scenario.
fn apply_reset(host: &FakeHost) -> Result<(), String> {
    if !host.stop_runners_and_wait(std::time::Duration::from_secs(10)) {
        return Err("a runner of the previous scenario did not stop in time".to_string());
    }
    host.clear_units();
    host.clear_presets_and_commands();
    // Remove everything under the data directory and the sysfs test trees.
    for path in [
        format!("{HOST_DATA_DIR}/manager"),
        format!("{HOST_DATA_DIR}/drivers"),
        "/sys/bus/pci/devices".to_string(),
        "/sys/class/drm".to_string(),
        "/run/extensions/nvidia.raw".to_string(),
    ] {
        host.remove_tree(&path);
    }
    host.mkdir(&format!("{HOST_DATA_DIR}/manager/secrets"));
    host.mkdir(&format!("{HOST_DATA_DIR}/manager/state"));
    host.mkdir(&format!("{HOST_DATA_DIR}/manager/tasks"));
    host.mkdir(&format!("{HOST_DATA_DIR}/manager/upload"));
    host.mkdir(&format!("{HOST_DATA_DIR}/manager/trash"));
    host.mkdir(&format!("{HOST_DATA_DIR}/manager/bin"));
    host.mkdir(&format!("{HOST_DATA_DIR}/drivers"));
    host.mkdir("/sys/bus/pci/devices");
    host.mkdir("/sys/class/drm");
    Ok(())
}

async fn state_view(State(state): State<Arc<ServerState>>) -> Response {
    // The std lock is never held across an await; extract the summary
    // pieces first (AuthState is itself Sync and outlives the guard).
    let (ctx, auth_epoch) = {
        let inner = state.inner.lock().unwrap();
        (inner.ctx.clone(), inner.auth_epoch)
    };
    // The limiter summary proves the bucket is fresh after a rebuild; the
    // logged-out count proves the logout table was dropped with it.
    let mut limiter_summary = json!({
        "tokens": null,
        "consecutive_failures": ctx.auth.consecutive_failures(),
    });
    {
        let mut limiter = ctx.auth.limiter.lock().await;
        if let Some(l) = limiter.as_mut() {
            limiter_summary["tokens"] = json!(l.available_tokens());
        }
    }
    let logged_out = ctx.auth.logged_out_len();
    let challenges = ctx.auth.ssh_challenges.outstanding(ctx.host.wall_ms());
    Json(json!({
        "scenario": state.scenario.lock().unwrap().clone(),
        "commands": state.host.argv_log(),
        "units": state.host.unit_ids(),
        "data_dir": HOST_DATA_DIR,
        "auth_epoch": auth_epoch,
        "limiter": limiter_summary,
        "logged_out": logged_out,
        "ssh_challenges": challenges,
    }))
    .into_response()
}

/// Re-point one PCI device's `driver` link between two requests, the way a VM start hands a
/// GPU to `vfio-pci`. It never touches the data directory, so a signed-in session survives
/// it.
async fn set_driver(State(state): State<Arc<ServerState>>, Json(body): Json<Value>) -> Response {
    let address = body
        .get("address")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let driver = body
        .get("driver")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    state.host.set_gpu_driver(&address, driver.as_deref());
    Json(json!({"ok": true})).into_response()
}

async fn clock(State(state): State<Arc<ServerState>>, Json(body): Json<Value>) -> Response {
    let ms = body.get("advance_ms").and_then(|v| v.as_u64()).unwrap_or(0);
    state.host.advance(std::time::Duration::from_millis(ms));
    Json(json!({"ok": true})).into_response()
}

async fn setup_code(State(state): State<Arc<ServerState>>) -> Response {
    let ctx = state.inner.lock().unwrap().ctx.clone();
    if let Some(issued) =
        ctx.auth
            .ensure_setup_code(&state.host, &ctx.paths, &ctx.settings, "reset")
    {
        issued.log();
    }
    let logs = String::from_utf8_lossy(&state.logs.0.lock().unwrap()).into_owned();
    let code = logs
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|entry| entry["event"] == "setup_code_issued")
        .and_then(|entry| {
            entry["msg"]
                .as_str()
                .and_then(|msg| msg.strip_prefix("SETUP CODE: "))
                .and_then(|msg| msg.split('.').next())
                .map(str::to_string)
        });
    Json(json!({"code": code})).into_response()
}

async fn expire_setup_code(State(state): State<Arc<ServerState>>) -> Response {
    state.inner.lock().unwrap().ctx.auth.expire_setup_code();
    Json(json!({"ok": true})).into_response()
}

async fn reset_credentials(State(state): State<Arc<ServerState>>) -> Response {
    state
        .host
        .remove("/var/lib/nvm/manager/secrets/web-credentials.json");
    Json(json!({"ok": true})).into_response()
}

async fn restart(State(state): State<Arc<ServerState>>) -> Response {
    let mut inner = state.inner.lock().unwrap();
    let (ctx, normal, setup_problem) =
        build_routers(&state.host, inner.settings.clone(), inner.web_root.clone());
    inner.ctx = ctx;
    inner.normal = normal;
    inner.setup_problem = setup_problem;
    inner.auth_epoch += 1;
    Json(json!({"ok": true})).into_response()
}

#[tokio::main]
async fn main() {
    let mut port: u16 = 18109;
    let mut web_root = PathBuf::from("web/dist");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                if let Some(value) = args.next() {
                    port = value.parse().unwrap_or(port);
                }
            }
            "--web-root" => {
                if let Some(value) = args.next() {
                    web_root = PathBuf::from(value);
                }
            }
            "--data-dir" => {
                // The fake host stores its data virtually.
                let _ = args.next();
            }
            _ => {}
        }
    }

    let host = default_host();
    prime_common(&host);
    prepare(&host, "fresh");

    let settings = Settings {
        username: Some("admin".into()),
        port,
        build_timeout_minutes: Some(30),
        problems: Vec::new(),
    };
    let files: StaticFiles = load_static_files(&web_root);
    let logs = common::SharedBuf::default();
    truenas_nvidia_driver_manager::log::set_sink(Box::new(FakeLog(logs.clone())));
    let (ctx_normal, normal, setup_problem) = build_routers(&host, settings.clone(), files.clone());
    let state = Arc::new(ServerState {
        host: host.clone(),
        logs,
        scenario: Mutex::new("fresh".into()),
        inner: Mutex::new(Inner {
            ctx: ctx_normal,
            normal,
            setup_problem,
            auth_epoch: 0,
            web_root: files,
            settings,
        }),
    });
    let control = Router::new()
        .route("/__fake/scenario", post(scenario))
        .route("/__fake/state", get(state_view))
        .route("/__fake/clock", post(clock))
        .route("/__fake/driver", post(set_driver))
        .route("/__fake/setup-code", get(setup_code))
        .route("/__fake/setup-code/expire", post(expire_setup_code))
        .route("/__fake/credentials/reset", post(reset_credentials))
        .route("/__fake/restart", post(restart))
        .with_state(state.clone());
    let app = control.fallback(dispatch).with_state(state);

    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("bind the test service");
    eprintln!("fake host server listening on http://{address} (web root {web_root:?})");
    axum::serve(listener, app).await.expect("serve");
}

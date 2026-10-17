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
use truenas_nvidia_driver_manager::plan::{Ctx, PlanSlot};
use truenas_nvidia_driver_manager::settings::{InitialPassword, Secret, Settings};
use truenas_nvidia_driver_manager::startup::Startup;
use truenas_nvidia_driver_manager::store::Paths;
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

struct ServerState {
    host: FakeHost,
    scenario: Mutex<String>,
    /// The live web process. Rebuilt with a fresh `AuthState` on every scenario switch so the
    /// login limiter, the logged-out table and the SSH challenge table never leak across
    /// scenes.
    inner: Mutex<Inner>,
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
    let make = |startup: Startup| -> Arc<Ctx> {
        Arc::new(Ctx {
            host: Arc::new(host.clone()),
            paths: Paths::new(HOST_DATA_DIR.to_string()),
            settings: settings.clone(),
            startup,
            process_lock: tokio::sync::Mutex::new(()),
            plan: std::sync::Mutex::new(PlanSlot::new()),
            upload: Arc::new(UploadState::new()),
            auth: Arc::new(truenas_nvidia_driver_manager::auth::AuthState::new()),
        })
    };
    let ctx_normal = make(healthy);
    let ctx_setup = make(broken);
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
    host.remove("/var/lib/nvm/manager/secrets/web-credentials.json");
    // apply_reset wipes /run; every scene except no_link gets the link back.
    if scenario != "no_link" {
        host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH);
    }
    match scenario {
        "fresh" | "no_link" | "upload_stage" | "drivers_rows" | "setup_problem" => {}
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
        "local_replacement" => prepare_local_replacement(host),
        "no_link" => host.remove("/run/extensions/nvidia.raw"),
        "running_task" => {
            let _ = host.set_gate("curl", &["--silent"]);
            let id = "20260928T101530Z-3f9a0c12";
            let state = truenas_nvidia_driver_manager::task_state::new_state(
                id,
                1,
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
fn prepare_local_replacement(host: &FakeHost) {
    host.set_uname(KERNEL);
    let original_bytes = b"driver image";
    prime_canonical(host, "580.173.02");
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
        host.set_listing(image.to_str().unwrap(), &canonical_listing(KERNEL, version));
        host.put_text(
            row.join("record.json").to_str().unwrap(),
            &json!({
                "kind": kind,
                "version": version,
                "sha256": sha256_hex(bytes),
                "size": bytes.len(),
                "target_kernel": KERNEL,
                "created_at": 7,
                "fingerprint": null,
                "original_of": (kind == "original").then(|| json!({
                    "kernel": KERNEL, "truenas_version": TRUENAS_VERSION,
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
                // Accepted for compatibility; the fake host is virtual.
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
        password: InitialPassword::Usable(Secret::new(
            std::env::var("NVM_E2E_PASSWORD").unwrap_or_else(|_| "initial-password-1".into()),
        )),
        port,
        build_timeout_minutes: Some(30),
        problems: Vec::new(),
    };
    let files: StaticFiles = load_static_files(&web_root);
    let (ctx_normal, normal, setup_problem) = build_routers(&host, settings.clone(), files.clone());
    let state = Arc::new(ServerState {
        host: host.clone(),
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
        .with_state(state.clone());
    let app = control.fallback(dispatch).with_state(state);

    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("bind the test service");
    eprintln!("fake host server listening on http://{address} (web root {web_root:?})");
    axum::serve(listener, app).await.expect("serve");
}

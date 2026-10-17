//! The runner's save-original, download and build failures, and delivery, followed by the admitted
//! replacement sequence.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::host::{Exit, Host};
use truenas_nvidia_driver_manager::runner;
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{
    self, new_state, ErrorRecord, OriginalChoice, Pass, Phase, PlanSource, ReadingJson,
    ReplaceRecord, Status, TaskState,
};

const ID: &str = "20260928T101530Z-3f9a0c12";

fn container_paths() -> Paths {
    Paths::new(CONTAINER_DATA_DIR)
}

fn host_paths() -> Paths {
    Paths::new(HOST_DATA_DIR)
}

fn state_text(host: &FakeHost) -> String {
    host.file_text(&format!("/var/lib/nvm/manager/state/{ID}.json"))
        .unwrap()
}

fn write_running(host: &FakeHost, original: OriginalChoice, source: PlanSource) -> TaskState {
    let mut state = new_state(ID, 1, sample_plan(source), Phase::ReadLoadedVersion);
    state.plan.as_mut().unwrap().original = original;
    state.plan.as_mut().unwrap().version = "580.178.04".into();
    task_state::write(host, &container_paths(), &state).unwrap();
    state
}

/// Run the task the way the unit does: with the build environment this host hands over, if it has
/// one.
fn start(host: &FakeHost, id: &str) -> std::process::ExitCode {
    let arc: Arc<dyn Host> = Arc::new(host.clone());
    match host.build_env() {
        Some(env) => runner::run_with_env(arc, host_paths(), id, env),
        None => runner::run_with(arc, host_paths(), id),
    }
}

fn run(host: &FakeHost) -> std::process::ExitCode {
    let _runner = host.enter_runner_scope();
    start(host, ID)
}

async fn base_host() -> FakeHost {
    let host = default_host();
    // These cases run past the build stage with a prepared build environment.
    host.prepare_build_env();
    prime_canonical(&host, "580.173.02");
    host.set_listing(
        &format!("{HOST_DATA_DIR}/manager/tasks/{ID}/row-original/nvidia.raw"),
        &canonical_listing(KERNEL, "580.173.02"),
    );
    host
}

#[tokio::test]
async fn qualified_470_rejects_the_unverified_official_run_before_archive_or_docker() {
    let host = base_host().await;
    let mut state = write_running(&host, OriginalChoice::Skip, PlanSource::Official);
    state.plan.as_mut().unwrap().version = COMMUNITY_PATCH_VERSION.into();
    task_state::write(&host, &container_paths(), &state).unwrap();
    let (index, responses) = toolkit_index();
    host.set_curl(TOOLKIT_INDEX_URL, 200, index.as_bytes());
    for (url, body) in responses {
        host.set_curl(&url, 200, &body);
    }
    let run_url = format!(
        "{CATALOG_URL}{COMMUNITY_PATCH_VERSION}/NVIDIA-Linux-x86_64-{COMMUNITY_PATCH_VERSION}-no-compat32.run"
    );
    host.set_curl(&run_url, 200, b"unverified official installer");
    host.set_curl(COMMUNITY_PATCH_ARCHIVE_URL, 200, b"unverified archive");

    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = match task_state::read(&host, &host_paths(), ID) {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!("failed task must pass read-back validation: {other:?}"),
    };
    assert_eq!(state.status, Status::Failed);
    let error = state.error.unwrap();
    assert_eq!(error.code, "DOWNLOAD_FAILED");
    assert_eq!(
        error.params,
        serde_json::json!({"reason": {
            "key": DOWNLOAD_CHECKSUM_MISMATCH,
            "params": {"file_name": format!("NVIDIA-Linux-x86_64-{COMMUNITY_PATCH_VERSION}-no-compat32.run")},
        }})
    );
    assert!(error.command.is_none());
    assert!(error.exit.is_none());
    let argv = host.argv_log();
    assert_eq!(
        host.count("curl"),
        6,
        "toolkit index/four packages/driver only: {argv:?}"
    );
    assert!(!argv
        .iter()
        .flatten()
        .any(|arg| arg == COMMUNITY_PATCH_ARCHIVE_URL));
    assert!(!argv
        .iter()
        .any(|args| args.first().is_some_and(|p| p == "docker")
            && args.get(1).is_some_and(|arg| arg == "run")));
    assert_eq!(host.file(CANONICAL_PATH).unwrap(), b"driver image".to_vec());
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/manager/tasks/{ID}")));
}

#[tokio::test]
async fn official_615_rejects_the_highest_common_toolkit_below_its_floor() {
    let host = base_host().await;
    let mut state = write_running(&host, OriginalChoice::Skip, PlanSource::Official);
    state.plan.as_mut().unwrap().version = "615.71.09".into();
    task_state::write(&host, &container_paths(), &state).unwrap();
    let (mut index, _) = toolkit_index_at("1.20.0-99");
    // A newer entry in only one package cannot change the four-way maximum.
    index.push_str("Package: libnvidia-container1\nArchitecture: amd64\nVersion: 1.20.2-1\nFilename: ./libnvidia-container1_1.20.2-1_amd64.deb\nSize: 1\nSHA256: aa\n\n");
    host.set_curl(TOOLKIT_INDEX_URL, 200, index.as_bytes());

    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = match task_state::read(&host, &host_paths(), ID) {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!("the terminal state must pass read-back validation: {other:?}"),
    };
    assert_eq!(state.status, Status::Failed);
    let error = state.error.expect("failed task error");
    assert_eq!(error.code, "DOWNLOAD_FAILED");
    assert_eq!(
        error.params,
        serde_json::json!({"reason": {
            "key": "download.toolkit_too_old",
            "params": {"driver_version": "615.71.09", "toolkit_version": "1.20.0"},
        }})
    );
    assert!(error.raw.is_none());
    assert!(error.command.is_none());
    assert!(error.exit.is_none());
    assert_eq!(host.count("curl"), 1, "only the index may be requested");
    let argv = host.argv_log();
    assert!(!argv
        .iter()
        .any(|a| a.first().is_some_and(|p| p == "docker") && a.get(1).is_some_and(|a| a == "run")));
    assert!(!argv
        .iter()
        .any(|a| a.first().is_some_and(|p| p == "rmmod" || p == "modprobe")));
    assert_eq!(host.file(CANONICAL_PATH).unwrap(), b"driver image".to_vec());
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/manager/tasks/{ID}")));
    assert!(!host.exists(&format!("/var/lib/nvm/drivers/{ID}-build")));
    let log = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap();
    assert!(log.contains("log.cmd") && log.contains("Packages"), "{log}");
    assert!(
        log.contains("log.toolkit_selected") && log.contains("1.20.0-99"),
        "{log}"
    );
    assert!(!log.contains("log.file_downloaded"), "{log}");
    assert!(!log.contains("\"phase\":\"build\""), "{log}");
}

#[tokio::test]
async fn official_toolkit_floor_keeps_the_614_and_exact_615_boundaries() {
    for (driver, toolkit) in [("614.99.01", "1.19.0-1"), ("615.71.09", "1.20.1-1")] {
        let host = base_host().await;
        let mut state = new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Download);
        state.plan.as_mut().unwrap().version = driver.into();
        let deadline = host.now() + std::time::Duration::from_secs(120);
        let mut ctx = runner::RunCtx {
            host: Arc::new(host.clone()),
            paths: host_paths(),
            id: ID.into(),
            state,
            build_deadline: Some(deadline),
        };
        let (index, responses) = toolkit_index_at(toolkit);
        host.set_curl(TOOLKIT_INDEX_URL, 200, index.as_bytes());
        for (url, body) in &responses {
            host.set_curl(url, 200, body);
        }
        assert!(
            runner::download::toolkit(&mut ctx, deadline).is_ok(),
            "{driver} / {toolkit}"
        );
        assert_eq!(host.count("curl"), 5);
        let log = host
            .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
            .unwrap();
        let selected = log.find("log.toolkit_selected").unwrap();
        let downloaded = log.find("log.file_downloaded").unwrap();
        assert!(selected < downloaded, "{log}");
    }
}

#[tokio::test]
async fn official_toolkit_metadata_and_no_common_keep_their_existing_reasons() {
    for (index, reason) in [
        (toolkit_index_at("1.20.0-99").0.replace("./libnvidia-container1_1.20.0-99_amd64.deb", "./wrong_1.20.0-99_amd64.deb"), DOWNLOAD_TOOLKIT_ENTRY_UNEXPECTED),
        (TOOLKIT_PACKAGES.iter().enumerate().map(|(i, name)| format!("Package: {name}\nArchitecture: amd64\nVersion: 1.20.{i}-1\nFilename: ./{name}_1.20.{i}-1_amd64.deb\nSize: 1\nSHA256: aa\n\n")).collect::<String>(), DOWNLOAD_TOOLKIT_NO_COMMON_VERSION),
    ] {
        let host = base_host().await;
        let mut state = new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Download);
        state.plan.as_mut().unwrap().version = "615.71.09".into();
        let deadline = host.now() + std::time::Duration::from_secs(120);
        let mut ctx = runner::RunCtx {
            host: Arc::new(host.clone()), paths: host_paths(), id: ID.into(), state,
            build_deadline: Some(deadline),
        };
        host.set_curl(TOOLKIT_INDEX_URL, 200, index.as_bytes());
        let failure = runner::download::toolkit(&mut ctx, deadline).unwrap_err();
        assert_eq!(failure.code, ErrorCode::DownloadFailed);
        assert_eq!(failure.params["reason"]["key"], reason);
        assert_eq!(host.count("curl"), 1);
    }
}

#[test]
fn deadline_after_toolkit_selection_wins_over_an_old_common_version() {
    let host = default_host();
    let mut state = new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Download);
    state.plan.as_mut().unwrap().version = "615.71.09".into();
    let deadline = host.now() + std::time::Duration::from_secs(5);
    let task = format!("{HOST_DATA_DIR}/manager/tasks/{ID}");
    host.put_text(
        &format!("{task}/dl/Packages"),
        &toolkit_index_at("1.20.0-1").0,
    );
    let gate = host.set_gate_output("curl", &["--silent"], FakeOutput::stdout("200"));
    let _release_gate = ReleaseGate(gate.clone());
    let worker = host.clone();
    let handle = std::thread::spawn(move || {
        let mut ctx = runner::RunCtx {
            host: Arc::new(worker),
            paths: host_paths(),
            id: ID.into(),
            state,
            build_deadline: Some(deadline),
        };
        runner::download::toolkit(&mut ctx, deadline)
    });
    let reached_gate = gate.wait_blocked(&handle);
    assert!(reached_gate);
    host.advance(std::time::Duration::from_millis(4_500));
    gate.release();
    let failure = handle.join().unwrap().unwrap_err();
    assert_eq!(failure.code, ErrorCode::BuildTimeout);
    assert_eq!(host.count("curl"), 1);
    let log = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap();
    assert!(log.contains("log.toolkit_selected"), "{log}");
    assert!(log.contains("log.build_timeout"), "{log}");
}

#[tokio::test]
async fn save_original_failure_stops_before_download() {
    let host = base_host().await;
    host.remove(CANONICAL_PATH);
    write_running(&host, OriginalChoice::Save, PlanSource::Official);
    let code = run(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let raw = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.json"))
        .unwrap_or_else(|| panic!("no state file; commands: {:?}", host.argv_log()));
    let state: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(state["status"], "FAILED", "{state}");
    assert_eq!(state["error"]["code"], "SAVE_ORIGINAL_FAILED");
    assert_eq!(
        state["error"]["params"]["save_reason"]["key"],
        SAVE_CANONICAL_UNREADABLE
    );
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/manager/tasks/{ID}/row-original")));
    let log = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap();
    assert!(log.contains("\"phase\":\"save_original\""));
    assert!(!log.contains("\"phase\":\"download\""));
}

#[tokio::test]
async fn download_failure_is_reported() {
    let host = base_host().await;
    write_running(&host, OriginalChoice::Save, PlanSource::Official);
    host.set_curl_failure(TOOLKIT_INDEX_URL, 6, "curl: (6) Could not resolve host");
    let code = run(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(state["status"], "FAILED");
    assert_eq!(state["error"]["code"], "DOWNLOAD_FAILED");
    assert_eq!(
        state["error"]["params"]["reason"]["key"],
        DOWNLOAD_REQUEST_FAILED
    );
    assert!(state["error"]["params"]["reason"]["params"]["file_name"]
        .as_str()
        .unwrap()
        .contains("Packages"));
    assert!(state["error"]["raw"]
        .as_str()
        .unwrap()
        .contains("Could not resolve"));
}

#[tokio::test]
async fn build_failure_is_reported_with_the_container_output() {
    let host = base_host().await;
    write_running(&host, OriginalChoice::Save, PlanSource::Official);
    official_downloads(&host, "580.178.04");
    host.preset(
        "docker",
        &["run"],
        FakeOutput {
            exit: truenas_nvidia_driver_manager::host::Exit::Code(1),
            stdout: b"NVM:STEP:check\nNVM:FAIL:installer_failed\n".to_vec(),
            stderr: Vec::new(),
            delay: std::time::Duration::ZERO,
        },
    );
    let code = run(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let raw = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.json"))
        .unwrap_or_else(|| panic!("no state file; commands: {:?}", host.argv_log()));
    let state: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(state["status"], "FAILED", "{state}");
    assert_eq!(state["error"]["code"], "BUILD_FAILED");
    assert_eq!(
        state["error"]["params"]["reason"]["key"],
        BUILD_INSTALLER_FAILED
    );
    // Machine lines are consumed as the reason; error.raw holds only the remaining (non-machine)
    // container output.
    assert!(state["error"]["raw"]
        .as_str()
        .map(|r| !r.contains("NVM:FAIL"))
        .unwrap_or(true));
    assert!(!host.exists(&format!("/var/lib/nvm/drivers/{ID}-build/nvidia.raw")));
}

#[tokio::test]
async fn a_signaled_build_finishes_failed_with_the_real_exit_and_cleanup() {
    let host = official_task_host().await;
    let container = format!("nvm-build-{ID}");
    host.preset(
        "docker",
        &["run"],
        FakeOutput {
            exit: truenas_nvidia_driver_manager::host::Exit::Signal(9),
            stdout: b"NVM:STEP:check\npartial build output\n".to_vec(),
            stderr: b"docker: interrupted\n".to_vec(),
            delay: std::time::Duration::ZERO,
        },
    );
    let gate = host.set_gate("docker", &["rm", "-f", &container]);
    let _release_gate = ReleaseGate(gate.clone());
    let worker = host.clone();
    let handle = std::thread::spawn(move || run(&worker));
    let cleanup_reached = gate.wait_blocked(&handle);
    let present_before_cleanup = host.has_build_container(&container);
    gate.release();
    assert_eq!(handle.join().unwrap(), std::process::ExitCode::SUCCESS);
    assert!(
        cleanup_reached,
        "stage 15 must reach the container removal; commands: {:?}",
        host.argv_log()
    );
    assert!(
        present_before_cleanup,
        "the signaled build left a container to remove"
    );
    assert!(
        !host.has_build_container(&container),
        "stage 15 must remove the container"
    );
    let state = match task_state::read(&host, &host_paths(), ID) {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!("the terminal state must pass read-back validation: {other:?}"),
    };
    assert_eq!(state.status, Status::Failed);
    assert_eq!(state.phase, Some(Phase::ReadLoadedVersionEnd));
    assert!(state.finished_at.is_some());
    let error = state.error.unwrap();
    assert_eq!(error.code, "BUILD_FAILED");
    assert_eq!(
        error.params["reason"],
        serde_json::json!({"key": "reason.signal", "params": {"signal": 9}})
    );
    assert_eq!(error.exit, Some(serde_json::json!({"signal": 9})));
    assert_eq!(error.command.unwrap()[0], "docker");
    assert!(error.raw.unwrap().contains("docker: interrupted"));
    assert!(host.argv_log().iter().any(|argv| argv.len() > 3
        && argv[0] == "docker"
        && argv[1] == "rm"
        && argv[3] == format!("nvm-build-{ID}")));
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/manager/tasks/{ID}")));
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/drivers/{ID}-build")));
    assert_eq!(
        host.file(CANONICAL_PATH).as_deref(),
        Some(b"driver image".as_slice())
    );
}

#[tokio::test]
async fn a_signaled_build_with_a_machine_failure_keeps_both_cause_and_exit() {
    use truenas_nvidia_driver_manager::host::Exit;

    let host = official_task_host().await;
    host.preset(
        "docker",
        &["run"],
        FakeOutput {
            exit: Exit::Signal(15),
            stdout: b"NVM:STEP:check\nNVM:FAIL:installer_failed\n".to_vec(),
            stderr: b"docker: interrupted".to_vec(),
            delay: std::time::Duration::ZERO,
        },
    );
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = match task_state::read(&host, &host_paths(), ID) {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!("the terminal state must pass read-back validation: {other:?}"),
    };
    let error = state.error.expect("build failure");
    assert_eq!(state.status, Status::Failed);
    assert_eq!(error.code, "BUILD_FAILED");
    assert_eq!(
        error.params["reason"],
        serde_json::json!({"key": BUILD_INSTALLER_FAILED, "params": {}})
    );
    assert_eq!(error.exit, Some(serde_json::json!({"signal": 15})));
    assert_eq!(error.command.unwrap()[0], "docker");
    assert!(error.raw.unwrap().contains("docker: interrupted"));
}

#[tokio::test]
async fn a_signaled_cleanup_command_logs_the_general_reason() {
    use truenas_nvidia_driver_manager::host::Exit;

    let host = official_task_host().await;
    host.preset("docker", &["run"], FakeOutput::code(1));
    let container = format!("nvm-build-{ID}");
    host.preset(
        "docker",
        &["rm", "-f", &container],
        FakeOutput {
            exit: Exit::Signal(15),
            stdout: Vec::new(),
            stderr: b"docker: killed".to_vec(),
            delay: std::time::Duration::ZERO,
        },
    );
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state = match task_state::read(&host, &host_paths(), ID) {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!("the failed task must read back as valid: {other:?}"),
    };
    assert_eq!(state.status, Status::Failed);
    let log = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap();
    let cleanup = log
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| {
            line["key"] == "log.cleanup_failed"
                && line["p"]["cleanup_step"] == "remove_build_container"
        })
        .expect("the failed stage-15 removal is logged");
    assert_eq!(
        cleanup["p"]["reason"],
        serde_json::json!({"key": REASON_SIGNAL, "params": {"signal": 15}, "raw": "docker: killed"})
    );
    assert!(
        host.has_build_container(&container),
        "failed cleanup must not claim removal"
    );
}

#[test]
fn a_failure_stays_running_during_the_final_observation() {
    let host = default_host();
    let state = write_running(&host, OriginalChoice::Skip, PlanSource::Official);
    let gate = host.set_gate("docker", &["ps"]);
    let _release_gate = ReleaseGate(gate.clone());
    let worker = host.clone();
    let handle = std::thread::spawn(move || {
        let mut ctx = runner::RunCtx {
            host: Arc::new(worker),
            paths: host_paths(),
            id: ID.to_string(),
            state,
            build_deadline: None,
        };
        ctx.fail(
            ErrorCode::GpuInUse,
            serde_json::json!({"occupancy": "busy"}),
            None,
        );
        runner::finish(&mut ctx)
    });
    let reached_gate = gate.wait_blocked(&handle);
    let blocked = reached_gate;
    let during: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    gate.release();
    assert!(handle.join().unwrap().is_ok());
    assert!(blocked, "stage 15 did not reach the GPU user observation");
    assert_eq!(during["status"], "RUNNING", "{during}");
    assert_eq!(during["phase"], "read_loaded_version_end");
    assert!(during["finished_at"].is_null());
    assert_eq!(during["error"]["code"], "GPU_IN_USE");

    let terminal: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(terminal["status"], "FAILED", "{terminal}");
    assert_eq!(terminal["error"]["code"], "GPU_IN_USE");
    assert!(terminal["loaded_version_end"].is_object());
    assert!(terminal["gpu_users"].is_object());
    assert!(terminal["finished_at"].is_number());
}

#[tokio::test]
async fn a_successful_build_is_delivered_and_replaced() {
    let host = base_host().await;
    host.remove_mount("/usr");
    host.add_mount("tank/ROOT/default/usr", "/usr", "zfs", "ro");
    host.unload_module("nvidia_uvm");
    host.unload_module("nvidia_modeset");
    host.unload_module("nvidia_drm");
    host.load_module("nvidia", 0, &[]);
    write_running(&host, OriginalChoice::Save, PlanSource::Official);
    official_downloads(&host, "580.178.04");
    let artifact = b"built image";
    put_build_output(&host, ID, "580.178.04", artifact);

    let code = run(&host);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(code, std::process::ExitCode::SUCCESS, "{state}");
    assert_eq!(state["status"], "SUCCEEDED", "{state}");
    assert_eq!(state["checks"][0]["refresh_ok"], true);
    let record: serde_json::Value = serde_json::from_str(
        &host
            .file_text(&format!("/var/lib/nvm/drivers/{ID}-build/record.json"))
            .expect("build row"),
    )
    .unwrap();
    assert_eq!(record["kind"], "official");
    assert_eq!(record["version"], "580.178.04");
    assert_eq!(record["sha256"], sha256_hex(artifact));
    assert_eq!(record["target_kernel"], KERNEL);
    assert!(host.exists(&format!("/var/lib/nvm/drivers/{ID}-original/record.json")));
    // The original file was only created and read, never deleted or rewritten.
    for op in host.ops() {
        if op.path.contains(&format!("{ID}-original")) {
            assert!(
                !matches!(op.kind, "remove_file" | "remove_dir_all"),
                "the original row was modified: {op:?}"
            );
        }
    }
    assert_eq!(host.file(CANONICAL_PATH).unwrap(), artifact.to_vec());
    host.assert_red_lines();
}

/// The failure keeps the command's argv and exit status and the verbatim external output, and the
/// runner logs the `log.cmd` line.
#[tokio::test]
async fn a_save_failure_carries_the_command_evidence() {
    let host = base_host().await;
    write_running(&host, OriginalChoice::Save, PlanSource::Official);
    let listing = format!("{HOST_DATA_DIR}/manager/tasks/{ID}/row-original/nvidia.raw");
    host.set_unsquashfs_broken(&listing);
    let code = run(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(state["status"], "FAILED");
    assert_eq!(state["error"]["code"], "SAVE_ORIGINAL_FAILED");
    assert_eq!(
        state["error"]["params"]["save_reason"]["key"],
        SAVE_FACTS_UNREADABLE
    );
    // The external output is verbatim: no internal `unsquashfs failed:` prefix.
    assert_eq!(state["error"]["raw"], "unsquashfs: failed");
    assert_eq!(state["error"]["command"][0], "unsquashfs");
    assert_eq!(state["error"]["command"][1], "-lls");
    assert_eq!(state["error"]["command"][2], listing);
    assert_eq!(state["error"]["exit"]["code"], 1);
    let log = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap();
    assert!(
        log.lines()
            .any(|l| l.contains("log.cmd") && l.contains("unsquashfs")),
        "the failing command must appear as a log.cmd line"
    );
}

/// Every downloaded input is flushed before it counts as landed.
#[tokio::test]
async fn a_successful_download_syncs_every_input() {
    let host = base_host().await;
    write_running(&host, OriginalChoice::Save, PlanSource::Official);
    official_downloads(&host, "580.178.04");
    // No docker preset: the download phase completes and the build fails.
    let _ = run(&host);
    let task = format!("{HOST_DATA_DIR}/manager/tasks/{ID}");
    for package in TOOLKIT_PACKAGES {
        // The bytes are flushed under the temporary download name before the verified file is
        // renamed to its fixed name.
        let part = format!("{task}/dl/{package}.deb.part");
        assert!(
            host.ops_for(&part).contains(&"fsync_file"),
            "{part} was not flushed: {:?}",
            host.ops_for(&part)
        );
        let path = format!("{task}/in/toolkit/{package}.deb");
        assert!(
            host.ops()
                .iter()
                .any(|o| o.kind == "rename" && o.path.contains(&path)),
            "{path} did not arrive through a rename: {:?}",
            host.ops()
        );
    }
    assert!(
        host.ops_for(&format!("{task}/in/driver.run.part"))
            .contains(&"fsync_file"),
        "the installer part was not flushed"
    );
}

#[tokio::test]
async fn a_local_source_task_skips_download_and_build() {
    let host = base_host().await;
    let bytes = b"local row bytes";
    host.put_file(
        "/var/lib/nvm/drivers/20260928T101530Z-3f9a0c10-build/nvidia.raw",
        bytes.to_vec(),
    );
    write_running(
        &host,
        OriginalChoice::Skip,
        PlanSource::Local {
            row_id: "20260928T101530Z-3f9a0c10-build".into(),
            row_kind: truenas_nvidia_driver_manager::task_state::RowKind::Official,
            fingerprint: None,
            sha256: sha256_hex(bytes),
        },
    );
    let code = run(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(state["status"], "FAILED", "{state}");
    let log = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap_or_default();
    assert!(!log.contains("\"phase\":\"download\""));
    assert!(!log.contains("\"phase\":\"build\""));
    assert!(log.contains("\"phase\":\"read_loaded_version\""));
}

async fn official_task_host() -> FakeHost {
    let host = base_host().await;
    write_running(&host, OriginalChoice::Skip, PlanSource::Official);
    official_downloads(&host, "580.178.04");
    host
}

#[tokio::test]
async fn each_build_container_carries_its_own_name_and_keeps_the_inputs_when_unresolved() {
    // Two runs of the same version, in two task ids. Both end with an unresolved driver
    // responsibility (the unload was refused), so the terminal cleanup must not delete the task
    // directory or its inputs.
    for (id, version) in [
        ("20260928T101530Z-3f9a0c12", "580.178.04"),
        ("20260928T101600Z-4a0b1d23", "580.178.04"),
    ] {
        let host = official_task_host().await;
        put_build_output(&host, id, version, b"built image");
        let task_dir = format!("{HOST_DATA_DIR}/manager/tasks/{id}");
        host.put_text(&format!("{task_dir}/in/driver.run"), "the only copy");
        let mut state = new_state(
            id,
            1,
            sample_plan(PlanSource::Official),
            Phase::ReadLoadedVersion,
        );
        state.plan.as_mut().unwrap().original = OriginalChoice::Skip;
        task_state::write(&host, &container_paths(), &state).unwrap();
        let _runner = host.enter_runner_scope();
        let code = start(&host, id);
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        let run_argv = host
            .argv_log()
            .into_iter()
            .find(|a| a[0] == "docker" && a[1] == "run")
            .expect("docker run");
        assert!(
            run_argv
                .iter()
                .any(|a| a == &format!("--name=nvm-build-{id}")),
            "{run_argv:?}"
        );
        let record = host
            .file_text(&format!("/var/lib/nvm/drivers/{id}-build/record.json"))
            .expect("build row");
        assert!(record.contains("580.178.04"), "{record}");
        // The run really is unresolved: the unload was refused after the replacement intent was
        // persisted.
        let final_state = match task_state::read(&host, &host_paths(), id) {
            truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
            other => panic!("state unreadable: {other:?}"),
        };
        assert!(!final_state.replace.is_empty(), "{final_state:?}");
        assert!(
            truenas_nvidia_driver_manager::tasks::driver_settled(
                &host,
                &host_paths(),
                &final_state
            )
            .is_err(),
            "the fixture must stay unresolved for this test to mean anything"
        );
        // The unique input and the state are kept; nothing was cleaned up.
        assert!(
            host.exists(&format!("{task_dir}/in/driver.run")),
            "the only input copy is preserved"
        );
        assert!(host.exists(&format!("{HOST_DATA_DIR}/manager/state/{id}.json")));
    }
}

#[tokio::test]
async fn a_missing_toolkit_program_fails_before_delivery() {
    let host = base_host().await;
    write_running(&host, OriginalChoice::Skip, PlanSource::Official);
    official_downloads(&host, "580.178.04");
    put_build_output(&host, ID, "580.178.04", b"built image");
    // Keep the file version and kernel correct but drop nvidia-ctk.
    let ko = format!("usr/lib/modules/{KERNEL}/kernel/drivers/video/nvidia.ko");
    host.set_listing(
        &format!("{HOST_DATA_DIR}/manager/tasks/{ID}/out/nvidia.raw"),
        &format!(
            "drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr\n\
             drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr/lib/modules/{KERNEL}\n\
             -rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/{ko}\n\
             -rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.580.178.04\n\
             -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime\n\
             -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime-hook\n"
        ),
    );
    let canonical_before = host.file(CANONICAL_PATH).unwrap();
    let code = run(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(state["status"], "FAILED", "{state}");
    assert_eq!(state["error"]["code"], "BUILD_FAILED");
    assert_eq!(
        state["error"]["params"]["reason"]["key"],
        "artifact.tool_missing"
    );
    assert_eq!(
        state["error"]["params"]["reason"]["params"]["program"],
        "nvidia-ctk"
    );
    assert!(!host.exists(&format!("/var/lib/nvm/drivers/{ID}-build")));
    assert_eq!(host.file(CANONICAL_PATH).unwrap(), canonical_before);
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/manager/tasks/{ID}")));
}

#[tokio::test]
async fn an_upload_source_has_no_download_phase_and_starts_at_check() {
    let host = base_host().await;
    let mut state = new_state(
        ID,
        1,
        sample_plan(PlanSource::Official),
        Phase::ReadLoadedVersion,
    );
    let plan = state.plan.as_mut().unwrap();
    plan.original = OriginalChoice::Skip;
    plan.source = PlanSource::Upload {
        fingerprint: "a".repeat(FINGERPRINT_HEX),
        run_sha256: "b".repeat(64),
        deb_sha256: TOOLKIT_PACKAGES
            .iter()
            .map(|p| (p.to_string(), "c".repeat(64)))
            .collect(),
    };
    task_state::write(&host, &container_paths(), &state).unwrap();
    host.preset(
        "docker",
        &["run"],
        FakeOutput {
            exit: truenas_nvidia_driver_manager::host::Exit::Code(1),
            stdout: b"NVM:STEP:check\nNVM:FAIL:installer_failed\n".to_vec(),
            stderr: Vec::new(),
            delay: std::time::Duration::ZERO,
        },
    );
    let code = run(&host);
    let raw = state_text(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS, "{raw}");
    let log = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap();
    assert!(!log.contains("\"phase\":\"download\""), "{log}");
    let phases: Vec<String> = log
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["key"] == "log.phase_enter")
        .map(|v| v["p"]["phase"].as_str().unwrap().to_string())
        .collect();
    assert!(!phases.iter().any(|p| p == "download"), "{phases:?}");
    let build_at = phases
        .iter()
        .position(|p| p == "build")
        .expect("build phase");
    let end_at = phases
        .iter()
        .position(|p| p == "read_loaded_version_end")
        .expect("the terminal phase");
    assert!(build_at < end_at, "{phases:?}");
    assert!(
        phases[..build_at]
            .iter()
            .all(|p| p == "read_loaded_version"),
        "{phases:?}"
    );
    let first_step = log
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["key"] == "log.build_step")
        .expect("a build step line");
    assert_eq!(first_step["p"]["build_step"], "check");
}

#[tokio::test]
async fn a_build_that_passes_the_deadline_is_removed_and_reported() {
    let host = official_task_host().await;
    let gate = host.set_gate("docker", &["run"]);
    let _release_gate = ReleaseGate(gate.clone());
    let worker = host.clone();
    let handle = std::thread::spawn(move || {
        let _runner = worker.enter_runner_scope();
        start(&worker, ID)
    });
    // Let the runner reach the build container, then move the clock past the 30-minute window.
    let reached_gate = gate.wait_blocked(&handle);
    assert!(reached_gate, "the runner reached the build container");
    host.advance(std::time::Duration::from_secs(31 * 60));
    gate.release();
    let code = handle.join().unwrap();
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(state["status"], "FAILED", "{state}");
    assert_eq!(state["error"]["code"], "BUILD_TIMEOUT");
    assert!(host.argv_log().iter().any(|a| a.len() > 3
        && a[0] == "docker"
        && a[1] == "rm"
        && a[3] == format!("nvm-build-{ID}")));
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/manager/tasks/{ID}")));
    assert_eq!(host.file(CANONICAL_PATH).unwrap(), b"driver image".to_vec());
}

#[tokio::test]
async fn a_download_that_passes_the_deadline_is_a_build_timeout() {
    let host = base_host().await;
    write_running(&host, OriginalChoice::Skip, PlanSource::Official);
    let gate = host.set_gate("curl", &["--silent"]);
    let _release_gate = ReleaseGate(gate.clone());
    let worker = host.clone();
    let handle = std::thread::spawn(move || {
        let arc: Arc<dyn Host> = Arc::new(worker.clone());
        runner::run_with(arc, host_paths(), ID)
    });
    let reached_gate = gate.wait_blocked(&handle);
    assert!(reached_gate, "the runner reached the first download");
    host.advance(std::time::Duration::from_secs(31 * 60));
    gate.release();
    let code = handle.join().unwrap();
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(state["status"], "FAILED", "{state}");
    assert_eq!(state["error"]["code"], "BUILD_TIMEOUT");
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/manager/tasks/{ID}")));
    assert_eq!(host.file(CANONICAL_PATH).unwrap(), b"driver image".to_vec());
}

#[tokio::test]
async fn a_save_readback_mismatch_stops_before_the_download() {
    let host = base_host().await;
    write_running(&host, OriginalChoice::Save, PlanSource::Official);
    let copy = format!("{HOST_DATA_DIR}/manager/tasks/{ID}/row-original/nvidia.raw");
    host.change_on_read(&copy, 1);
    let code = run(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(state["status"], "FAILED", "{state}");
    assert_eq!(state["error"]["code"], "SAVE_ORIGINAL_FAILED");
    assert_eq!(
        state["error"]["params"]["save_reason"]["key"],
        SAVE_READBACK_MISMATCH
    );
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/manager/tasks/{ID}/row-original")));
    let log = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap();
    assert!(!log.contains("\"phase\":\"download\""));
    assert_eq!(host.file(CANONICAL_PATH).unwrap(), b"driver image".to_vec());
}

#[tokio::test]
async fn the_plan_module_type_reaches_the_build_argv_and_its_failure() {
    for (module_type, wire) in [
        (
            Some(truenas_nvidia_driver_manager::catalog::KernelModuleType::Open),
            Some("open"),
        ),
        (
            Some(truenas_nvidia_driver_manager::catalog::KernelModuleType::Proprietary),
            Some("proprietary"),
        ),
        (None, None),
    ] {
        let host = official_task_host().await;
        let mut state = new_state(
            ID,
            1,
            sample_plan(PlanSource::Official),
            Phase::ReadLoadedVersion,
        );
        let plan = state.plan.as_mut().unwrap();
        plan.original = OriginalChoice::Skip;
        plan.kernel_module_type = module_type;
        task_state::write(&host, &container_paths(), &state).unwrap();
        put_build_output(&host, ID, "580.178.04", b"built image");
        let code = run(&host);
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        let run_argv = host
            .argv_log()
            .into_iter()
            .find(|a| a[0] == "docker" && a[1] == "run")
            .expect("docker run");
        match wire {
            Some(value) => assert!(
                run_argv
                    .iter()
                    .any(|a| a == &format!("NVM_KERNEL_MODULE_TYPE={value}")),
                "{run_argv:?}"
            ),
            None => assert!(!run_argv
                .iter()
                .any(|a| a.starts_with("NVM_KERNEL_MODULE_TYPE="))),
        }
        // The build container has no network access.
        assert!(run_argv.iter().any(|a| a == "--network"));
        assert!(run_argv.iter().any(|a| a == "none"));
        assert!(run_argv.iter().any(|a| a == "--pull"));
        assert!(run_argv.iter().any(|a| a == "never"));
    }

    // A package without the flag: the build fails with the reason.
    let host = official_task_host().await;
    let mut state = new_state(
        ID,
        1,
        sample_plan(PlanSource::Official),
        Phase::ReadLoadedVersion,
    );
    let plan = state.plan.as_mut().unwrap();
    plan.original = OriginalChoice::Skip;
    plan.kernel_module_type = Some(truenas_nvidia_driver_manager::catalog::KernelModuleType::Open);
    task_state::write(&host, &container_paths(), &state).unwrap();
    host.preset(
        "docker",
        &["run"],
        FakeOutput {
            exit: truenas_nvidia_driver_manager::host::Exit::Code(1),
            stdout: b"NVM:STEP:probe\nNVM:FAIL:kernel_module_type_unsupported\n".to_vec(),
            stderr: Vec::new(),
            delay: std::time::Duration::ZERO,
        },
    );
    let code = run(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(state["status"], "FAILED", "{state}");
    assert_eq!(state["error"]["code"], "BUILD_FAILED");
    assert_eq!(
        state["error"]["params"]["reason"]["key"],
        "kernel_module_type_unsupported"
    );
    assert_eq!(host.file(CANONICAL_PATH).unwrap(), b"driver image".to_vec());
}

#[tokio::test]
async fn a_task_starting_with_no_driver_modules_records_the_default_targets() {
    let host = FakeHost::new();
    prime_canonical(&host, "580.173.02");
    let mut state = new_state(
        ID,
        1,
        sample_plan(PlanSource::Local {
            row_id: "20260928T101530Z-3f9a0c10-build".into(),
            row_kind: truenas_nvidia_driver_manager::task_state::RowKind::Official,
            fingerprint: None,
            sha256: "a".repeat(64),
        }),
        Phase::ReadLoadedVersion,
    );
    state.plan.as_mut().unwrap().original = OriginalChoice::Skip;
    task_state::write(&host, &container_paths(), &state).unwrap();
    let code = run(&host);
    let raw = state_text(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS, "{raw}");
    let state: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(state["status"], "FAILED", "{raw}");
    assert_eq!(state["error"]["code"], "WRITE_FAILED", "{raw}");
    assert_eq!(
        state["reload_targets"]["basis"], "none_loaded_at_start",
        "{raw}"
    );
    let names: Vec<String> = state["reload_targets"]["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["nvidia", "nvidia_drm", "nvidia_modeset"]);
    let log = host
        .file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap();
    assert!(log.contains("log.reload_target_default"), "{log}");
}

#[tokio::test]
async fn every_outbound_url_is_one_of_the_required_hosts() {
    let host = official_task_host().await;
    put_build_output(&host, ID, "580.178.04", b"built image");
    let _ = run(&host);
    for argv in host.argv_log() {
        if argv[0] != "curl" {
            continue;
        }
        let url = argv.last().unwrap();
        assert!(
            url.starts_with(CATALOG_URL)
                || url.starts_with(TOOLKIT_INDEX_URL)
                || url.starts_with("https://nvidia.github.io/")
                || url.starts_with("https://download.nvidia.com/"),
            "{url}"
        );
        assert!(argv.iter().any(|a| a == "--proto"), "{argv:?}");
    }
    // Build-kit packages come from snapshot.debian.org; this case checks the source allowlist.
    let manifest = truenas_nvidia_driver_manager::build_kit::packages()
        .expect("the embedded build kit manifest parses");
    assert!(
        manifest.iter().all(|package| package
            .url
            .starts_with("https://snapshot.debian.org/archive/")),
        "a build kit package comes from another host"
    );
}

/// The failure's verbatim output is the last `ERROR_RAW_LINES` lines of the command, not its whole
/// stream.
#[tokio::test]
async fn a_save_facts_failure_keeps_the_last_error_lines() {
    let host = base_host().await;
    write_running(&host, OriginalChoice::Save, PlanSource::Official);
    let listing = format!("{HOST_DATA_DIR}/manager/tasks/{ID}/row-original/nvidia.raw");
    let lines: Vec<String> = (1..=110).map(|i| format!("unsquashfs: line {i}")).collect();
    host.preset(
        "unsquashfs",
        &["-lls", listing.as_str()],
        FakeOutput::code(1).with_stderr(&lines.join("\n")),
    );
    let code = run(&host);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(&state_text(&host)).unwrap();
    assert_eq!(state["status"], "FAILED");
    assert_eq!(state["error"]["code"], "SAVE_ORIGINAL_FAILED");
    let raw = state["error"]["raw"].as_str().unwrap();
    let got: Vec<&str> = raw.lines().collect();
    assert_eq!(got.len(), ERROR_RAW_LINES, "{raw}");
    assert_eq!(got[0], "unsquashfs: line 11");
    assert_eq!(got[ERROR_RAW_LINES - 1], "unsquashfs: line 110");
}

/// A `.deb` transfer that fails after writing bytes leaves no partial file at the fixed toolkit
/// path; the unverified bytes stay under `dl/`.
#[tokio::test]
async fn a_failed_toolkit_transfer_leaves_no_deb_at_the_fixed_path() {
    let host = base_host().await;
    let _ = host.create_dir(
        std::path::Path::new(&format!("{HOST_DATA_DIR}/manager/tasks/{ID}")),
        0o755,
    );
    let state = new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Download);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut ctx = runner::RunCtx {
        host: Arc::new(host.clone()),
        paths: host_paths(),
        id: ID.to_string(),
        state,
        build_deadline: Some(deadline),
    };
    let (index, responses) = toolkit_index();
    host.set_curl(TOOLKIT_INDEX_URL, 200, index.as_bytes());
    host.set_curl_partial_failure(&responses[0].0, b"partial-deb", 7, "curl: (7) failed");
    let failed = runner::download::toolkit(&mut ctx, deadline);
    assert!(failed.is_err());
    let task = format!("{HOST_DATA_DIR}/manager/tasks/{ID}");
    assert!(
        !host.exists(&format!("{task}/in/toolkit/libnvidia-container1.deb")),
        "the fixed `.deb` path must never receive unverified bytes"
    );
    assert_eq!(
        host.file_text(&format!("{task}/dl/libnvidia-container1.deb.part"))
            .as_deref(),
        Some("partial-deb"),
        "the partial transfer stays under dl/"
    );
}

/// A `curl` ended by its own `--max-time` is `BUILD_TIMEOUT` and still carries the executed
/// command's stderr (and argv/exit) as the external command raw output; only a budget that expired
/// before curl started may lack it.
#[tokio::test]
async fn a_download_timeout_keeps_the_executed_curl_stderr() {
    let host = base_host().await;
    let state = new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Download);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut ctx = runner::RunCtx {
        host: Arc::new(host.clone()),
        paths: host_paths(),
        id: ID.to_string(),
        state,
        build_deadline: Some(deadline),
    };
    host.set_curl_failure(
        TOOLKIT_INDEX_URL,
        28,
        "curl: (28) Operation timed out after test bytes",
    );
    let failed = runner::download::toolkit(&mut ctx, deadline).unwrap_err();
    assert_eq!(host.count("curl"), 1, "a timeout is not retried");
    assert_eq!(failed.code, ErrorCode::BuildTimeout, "{failed:?}");
    assert!(
        failed
            .raw
            .as_deref()
            .unwrap_or("")
            .contains("Operation timed out"),
        "the timeout dropped the executed command's raw output: {failed:?}"
    );
    let (argv, exit) = failed
        .command
        .as_ref()
        .expect("the command must travel too");
    assert_eq!(*exit, truenas_nvidia_driver_manager::host::Exit::Code(28));
    // The invocation carries its own connect and retry bounds, and the whole seconds left of the
    // deadline stay its `--max-time`.
    let flag = |name: &str| {
        argv.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].as_str())
    };
    assert_eq!(flag("--connect-timeout"), Some("20"), "{argv:?}");
    assert_eq!(flag("--retry"), Some("2"), "{argv:?}");
    assert!(
        argv.iter().any(|arg| arg == "--retry-connrefused"),
        "{argv:?}"
    );
    assert_eq!(
        flag("--max-time"),
        Some("1"),
        "one whole second is left of the 2 s deadline: {argv:?}"
    );

    // A budget that expired before curl was started has no command output.
    let state = new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Download);
    let mut ctx = runner::RunCtx {
        host: Arc::new(host.clone()),
        paths: host_paths(),
        id: ID.to_string(),
        state,
        build_deadline: Some(std::time::Instant::now()),
    };
    let expired = std::time::Instant::now();
    let failed = runner::download::toolkit(&mut ctx, expired).unwrap_err();
    assert_eq!(failed.code, ErrorCode::BuildTimeout, "{failed:?}");
    assert_eq!(failed.raw, None);
    assert!(failed.command.is_none());
    assert_eq!(host.count("curl"), 1, "no curl may start after the budget");
}

/// Run the driver download against preset headers and a two-byte `--output` file, returning the
/// classification of the size check.
fn run_driver_download(headers_text: &str, part: &[u8]) -> Result<(), ErrorCode> {
    let host = FakeHost::new();
    let task = format!("{HOST_DATA_DIR}/manager/tasks/{ID}");
    host.put_file(&format!("{task}/in/driver.run.part"), part.to_vec());
    host.put_text(&format!("{task}/dl/driver.headers"), headers_text);
    host.preset("curl", &[], FakeOutput::stdout("200"));
    let mut ctx = runner::RunCtx {
        host: Arc::new(host.clone()),
        paths: host_paths(),
        id: ID.to_string(),
        state: new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Download),
        build_deadline: None,
    };
    let deadline = host.now() + std::time::Duration::from_secs(1800);
    runner::download::driver(&mut ctx, deadline).map_err(|e| e.code)
}

/// The size check uses the `Content-Length` of the *last* response segment: an earlier proxy
/// `CONNECT`/`100 Continue` value never decides the final response, and a final response without
/// the header is an unknown total.
#[tokio::test]
async fn the_last_response_segment_decides_the_content_length() {
    // A preceding segment carries a total; the final 200 response is chunked.
    let with_earlier_total = "HTTP/1.1 100 Continue\r\nContent-Length: 1\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
    assert_eq!(run_driver_download(with_earlier_total, b"AB"), Ok(()));

    // A CONNECT-style first segment behaves the same way.
    let connect_style = "HTTP/1.1 200 Connection established\r\nContent-Length: 1\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
    assert_eq!(run_driver_download(connect_style, b"AB"), Ok(()));

    // The final segment's own header still decides: matching passes, a mismatch fails.
    let matches = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n";
    assert_eq!(run_driver_download(matches, b"AB"), Ok(()));
    let mismatch = "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n";
    assert_eq!(
        run_driver_download(mismatch, b"AB"),
        Err(ErrorCode::DownloadFailed)
    );
}

// ---- an occupancy refusal before the first unload is settled ----------

/// A `GPU_IN_USE` failure returns before any `rmmod`: the pass holds no module work, no uncertain
/// `rmmod` and an unchanged loaded version, so the task is settled and its directory is reclaimed.
/// The error code alone is not the proof: the same record with a changed loaded version keeps its
/// directory.
#[test]
fn an_occupancy_refusal_before_the_first_unload_is_settled() {
    let host = default_host();
    let mut state = new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Unload);
    state.status = Status::Failed;
    state.phase = Some(Phase::Unload);
    state.loaded_version_start = Some(ReadingJson::value("580.173.02".to_string()));
    state.loaded_version_end = Some(ReadingJson::value("580.173.02".to_string()));
    state.error = Some(ErrorRecord::new(
        ErrorCode::GpuInUse,
        serde_json::json!({"occupancy": "busy"}),
    ));
    state.replace.push(ReplaceRecord {
        pass: Pass::Update,
        unloaded: Vec::new(),
        reloaded: Vec::new(),
        refresh_exit: None,
        ldconfig_exit: None,
        readback_ok: None,
        readonly_after: None,
    });
    let task_dir = container_paths().task_dir(ID);
    let task_dir = task_dir.to_str().unwrap();
    task_state::write(&host, &container_paths(), &state).unwrap();
    host.mkdir(task_dir);

    assert!(truenas_nvidia_driver_manager::tasks::driver_settled(
        &host,
        &container_paths(),
        &state
    )
    .is_ok());
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &container_paths());
    assert!(!host.exists(task_dir), "the task directory is reclaimed");
    assert_eq!(reconciled.tasks.len(), 1);

    // The same record with a changed loaded version is not settled: the directory stays as the
    // recovery entry.
    let host = default_host();
    host.set_driver_version("575.00.00");
    task_state::write(&host, &container_paths(), &state).unwrap();
    host.mkdir(task_dir);
    assert!(truenas_nvidia_driver_manager::tasks::driver_settled(
        &host,
        &container_paths(),
        &state
    )
    .is_err());
    let _ = truenas_nvidia_driver_manager::tasks::reconcile(&host, &container_paths());
    assert!(host.exists(task_dir));
}

// ---- the download curl: bounded connect and retries -----------------------

/// Seconds `--connect-timeout` must carry.
const CONNECT_TIMEOUT: &str = "20";
/// Value `--retry` must carry.
const RETRY: &str = "2";

fn driver_url(version: &str) -> String {
    format!(
        "https://download.nvidia.com/XFree86/Linux-x86_64/{v}/NVIDIA-Linux-x86_64-{v}-no-compat32.run",
        v = version
    )
}

/// A download context on the fake host, with `deadline` as the budget left.
fn download_ctx(host: &FakeHost, deadline: Instant) -> runner::RunCtx {
    runner::RunCtx {
        host: Arc::new(host.clone()),
        paths: host_paths(),
        id: ID.to_string(),
        state: new_state(ID, 1, sample_plan(PlanSource::Official), Phase::Download),
        build_deadline: Some(deadline),
    }
}

fn pairs(argv: &[String]) -> Vec<(&str, &str)> {
    argv.windows(2)
        .map(|pair| (pair[0].as_str(), pair[1].as_str()))
        .collect()
}

fn first_curl(host: &FakeHost) -> Vec<String> {
    host.commands()
        .into_iter()
        .find(|cmd| cmd.program == "curl")
        .expect("the download ran curl")
        .argv()
}

fn seconds(argv: &[String], flag: &str) -> u64 {
    argv.windows(2)
        .find(|pair| pair[0] == flag)
        .unwrap_or_else(|| panic!("{flag} is missing: {argv:?}"))
        .get(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("{flag} does not carry whole seconds: {argv:?}"))
}

/// The download `curl` carries a bounded connect timeout and a bounded retry budget, and the
/// remaining budget stays the transfer's own `--max-time`.
#[test]
fn the_download_curl_bounds_the_connect_and_the_retries() {
    let host = default_host();
    let url = driver_url("580.178.04");
    host.set_curl(&url, 200, b"installer");
    let before = host.now();
    let deadline = before + Duration::from_secs(600);
    let mut ctx = download_ctx(&host, deadline);
    runner::download::driver(&mut ctx, deadline).expect("the download lands");
    let after = host.now();

    let argv = first_curl(&host);
    let pairs = pairs(&argv);
    assert!(
        pairs.contains(&("--connect-timeout", CONNECT_TIMEOUT)),
        "a stalled connect must fail at its own bound: {argv:?}"
    );
    assert!(
        pairs.contains(&("--retry", RETRY)),
        "a transient error must be retried inside the invocation: {argv:?}"
    );
    assert!(
        argv.iter().any(|arg| arg == "--retry-connrefused"),
        "a refused connection is one of the transient errors: {argv:?}"
    );
    assert!(
        !pairs.iter().any(|(flag, _)| *flag == "--speed-limit"),
        "a speed limit would change the timeout classification: {argv:?}"
    );
    // The invocation was created between the two readings, so its whole-second bound is the
    // remaining deadline as the call site saw it.
    let lowest = (deadline - after).as_secs();
    let highest = (deadline - before).as_secs();
    let max_time = seconds(&argv, "--max-time");
    assert!(
        (lowest..=highest).contains(&max_time),
        "--max-time {max_time} is not the remaining deadline ({lowest}..={highest}): {argv:?}"
    );
    assert_eq!(
        host.count("curl"),
        1,
        "a download that answers once is not invoked twice"
    );
}

/// A failed download is still the existing request failure, with the executed command and its raw
/// output, and it is invoked exactly once: the bounded retries happen inside `curl` itself.
#[test]
fn a_failed_download_keeps_the_request_failure_and_one_invocation() {
    let host = default_host();
    let url = driver_url("580.178.04");
    host.set_curl_failure(
        &url,
        7,
        "curl: (7) Failed to connect to download.nvidia.com",
    );
    let deadline = host.now() + Duration::from_secs(600);
    let mut ctx = download_ctx(&host, deadline);
    let failure = runner::download::driver(&mut ctx, deadline).unwrap_err();

    assert_eq!(host.count("curl"), 1, "the download ran more than once");
    assert_eq!(failure.code, ErrorCode::DownloadFailed, "{failure:?}");
    assert_eq!(
        failure.params["reason"]["key"], DOWNLOAD_REQUEST_FAILED,
        "{failure:?}"
    );
    assert!(
        failure
            .raw
            .as_deref()
            .unwrap_or("")
            .contains("Failed to connect"),
        "the failure dropped the executed command's output: {failure:?}"
    );
    let (argv, exit) = failure.command.expect("the failing command travels");
    assert_eq!(argv.last().map(String::as_str), Some(url.as_str()));
    assert_eq!(exit, Exit::Code(7));
}

/// Exit 28 (curl's own `--max-time`, including after its retries) stays the build timeout and still
/// carries the executed command.
#[test]
fn a_curl_timeout_stays_the_build_timeout() {
    let host = default_host();
    let url = driver_url("580.178.04");
    host.set_curl_failure(&url, 28, "curl: (28) Operation timed out");
    let deadline = host.now() + Duration::from_secs(600);
    let mut ctx = download_ctx(&host, deadline);
    let failure = runner::download::driver(&mut ctx, deadline).unwrap_err();

    assert_eq!(failure.code, ErrorCode::BuildTimeout, "{failure:?}");
    let (argv, exit) = failure.command.expect("the failing command travels");
    assert_eq!(argv.last().map(String::as_str), Some(url.as_str()));
    assert_eq!(exit, Exit::Code(28));
}

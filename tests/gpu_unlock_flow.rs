//! An update with the tensor unlock through the runner and the pre-delivery check, on the fake
//! host.

mod common;

use std::sync::Arc;

use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::gpu_unlock::artifact::check_gpu_unlock;
use truenas_nvidia_driver_manager::gpu_unlock::{
    firmware_names, rule_file, FIRMWARE_VERSION, INSTALLER,
};
use truenas_nvidia_driver_manager::host::Host;
use truenas_nvidia_driver_manager::runner::build::{self, BuildFailure};
use truenas_nvidia_driver_manager::runner::build_env::BuildEnv;
use truenas_nvidia_driver_manager::runner::{self, RunCtx};
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{
    self, new_state, OriginalChoice, Phase, PlanSource,
};

const ID: &str = "20260928T101530Z-3f9a0c12";
const VERSION: &str = "580.178.04";
const TARGETS: [&str; 2] = ["0000:01:00.0", "0000:02:00.0"];

fn targets() -> Vec<String> {
    TARGETS.iter().map(|t| t.to_string()).collect()
}

fn host_paths() -> Paths {
    Paths::new(HOST_DATA_DIR)
}

fn log_text(host: &FakeHost) -> String {
    host.file_text(&format!("/var/lib/nvm/manager/state/{ID}.log"))
        .unwrap()
}

// ---- the whole runner --------------------------------------------------------------------

fn task_host(unlock: bool) -> FakeHost {
    let host = default_host();
    host.prepare_build_env();
    prime_canonical(&host, "580.173.02");
    official_downloads(&host, VERSION);
    let mut state = new_state(
        ID,
        1,
        sample_plan(PlanSource::Official),
        Phase::ReadLoadedVersion,
    );
    let plan = state.plan.as_mut().unwrap();
    plan.original = OriginalChoice::Skip;
    plan.version = VERSION.into();
    if unlock {
        plan.gpu_unlock_targets = targets();
    }
    task_state::write(&host, &Paths::new(CONTAINER_DATA_DIR), &state).unwrap();
    host
}

fn run(host: &FakeHost) -> std::process::ExitCode {
    let _runner = host.enter_runner_scope();
    let arc: Arc<dyn Host> = Arc::new(host.clone());
    runner::run_with_env(arc, host_paths(), ID, host.build_env().unwrap())
}

#[test]
fn an_installer_that_is_not_the_pinned_file_fails_the_task_and_leaves_the_driver_alone() {
    let host = task_host(true);
    // The task installs another release, so the stage downloads the 610.43.03 installer, and
    // the download brings other bytes.
    let url = format!("{CATALOG_URL}{FIRMWARE_VERSION}/{}", INSTALLER.name);
    host.set_curl(&url, 200, b"not the pinned installer");
    assert_eq!(run(&host), std::process::ExitCode::SUCCESS);
    let state: serde_json::Value = serde_json::from_str(
        &host
            .file_text(&format!("/var/lib/nvm/manager/state/{ID}.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(state["status"], "FAILED", "{state}");
    assert_eq!(state["error"]["code"], "DOWNLOAD_FAILED");
    assert_eq!(
        state["error"]["params"]["reason"]["key"],
        DOWNLOAD_CHECKSUM_MISMATCH
    );
    // The phase table: the firmware stage came before anything of the build.
    let log = log_text(&host);
    assert!(log.contains("\"phase\":\"unlock_firmware\""), "{log}");
    assert!(!log.contains("\"phase\":\"build\""), "{log}");
    // Nothing was built, nothing was delivered, the driver file was not touched.
    assert!(!host
        .argv_log()
        .iter()
        .any(|a| a[0] == "docker" && a[1] == "run"));
    assert!(!host.has_build_container(&format!("nvm-build-{ID}")));
    assert!(!host.exists(&format!("/var/lib/nvm/drivers/{ID}-build")));
    assert!(!host.exists(&format!(
        "{HOST_DATA_DIR}/manager/gpu-unlock/firmware/610.43.03"
    )));
    assert_eq!(host.file(CANONICAL_PATH).unwrap(), b"driver image".to_vec());
}

#[test]
fn an_update_without_the_unlock_has_no_firmware_stage() {
    let host = task_host(false);
    put_build_output(&host, ID, VERSION, b"built image");
    let _ = run(&host);
    let log = log_text(&host);
    assert!(!log.contains("unlock_firmware"), "{log}");
    assert!(!host
        .argv_log()
        .iter()
        .any(|a| a[0] == "docker" && a[1] == "exec"));
    let build_run = host
        .argv_log()
        .into_iter()
        .find(|a| a[0] == "docker" && a[1] == "run")
        .expect("the build container");
    assert!(!build_run
        .iter()
        .any(|a| a.contains("gpu-unlock") || a.contains("GPU_UNLOCK")));
}

// ---- the build and the pre-delivery check ------------------------------------------------

struct Image {
    /// Files of the image beyond the driver's own, as image paths.
    files: Vec<String>,
    dep: String,
    rule: String,
    module_vermagic: Option<String>,
    nvidia_vermagic: Option<String>,
}

fn modules() -> String {
    format!("usr/lib/modules/{KERNEL}")
}

impl Image {
    /// An image that passes the check for `TARGETS`.
    fn with_unlock() -> Image {
        let mut files = vec![
            format!("{}/extra/gpu_unlock.ko", modules()),
            format!("{}/modules.dep", modules()),
            "usr/lib/modprobe.d/gpu-unlock.conf".to_string(),
            "usr/share/licenses/gpu-unlock/GPL-2.0.txt".to_string(),
            "usr/share/licenses/gpu-unlock-firmware/NVIDIA-LICENSE".to_string(),
        ];
        files
            .extend(firmware_names().map(|n| format!("usr/lib/firmware/gpu-unlock/610.43.03/{n}")));
        let vermagic = format!("{KERNEL} SMP preempt mod_unload modversions");
        Image {
            files,
            dep: "kernel/drivers/video/nvidia.ko:\nextra/gpu_unlock.ko:\n".into(),
            rule: rule_file(&targets()),
            module_vermagic: Some(vermagic.clone()),
            nvidia_vermagic: Some(vermagic),
        }
    }

    /// An image of a build without the unlock.
    fn plain() -> Image {
        Image {
            files: vec![format!("{}/modules.dep", modules())],
            dep: "kernel/drivers/video/nvidia.ko:\n".into(),
            rule: String::new(),
            module_vermagic: None,
            nvidia_vermagic: Some(format!("{KERNEL} SMP")),
        }
    }

    fn without(mut self, path: &str) -> Image {
        self.files.retain(|f| f != path);
        self
    }

    fn with(mut self, path: &str) -> Image {
        self.files.push(path.to_string());
        self
    }

    fn install(&self, host: &FakeHost) {
        put_build_output(host, ID, VERSION, b"built image");
        let task = format!("{HOST_DATA_DIR}/manager/tasks/{ID}");
        let nvidia = format!("{}/kernel/drivers/video/nvidia.ko", modules());
        let mut listing = format!(
            "drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr\n\
             drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/{}\n\
             -rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.580.178.04\n\
             -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-ctk\n\
             -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime\n\
             -rwxr-xr-x root/root 10 2026-09-28 10:15 squashfs-root/usr/bin/nvidia-container-runtime-hook\n",
            modules()
        );
        for path in std::iter::once(&nvidia).chain(&self.files) {
            listing.push_str(&format!(
                "-rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/{path}\n"
            ));
        }
        host.set_listing(&format!("{task}/out/nvidia.raw"), &listing);
        // check_artifact extracts the driver module, the unlock check the files it names.
        let inspect = format!("{task}/inspect-unlock");
        host.set_extracted(
            &format!("{inspect}/{}/modules.dep", modules()),
            self.dep.as_bytes(),
        );
        host.set_extracted(
            &format!("{inspect}/usr/lib/modprobe.d/gpu-unlock.conf"),
            self.rule.as_bytes(),
        );
        if let Some(vermagic) = &self.nvidia_vermagic {
            host.set_modinfo_field(&format!("{task}/inspect/{nvidia}"), "vermagic", vermagic);
        }
        if let Some(vermagic) = &self.module_vermagic {
            let module = format!("{inspect}/{}/extra/gpu_unlock.ko", modules());
            host.set_modinfo_field(&module, "vermagic", vermagic);
        }
    }
}

fn ctx(host: &FakeHost, unlock: bool) -> RunCtx {
    let mut plan = sample_plan(PlanSource::Official);
    plan.version = VERSION.into();
    if unlock {
        plan.gpu_unlock_targets = targets();
    }
    let ctx = RunCtx {
        host: Arc::new(host.clone()),
        paths: host_paths(),
        id: ID.into(),
        state: new_state(ID, 1, plan, Phase::Build),
        build_deadline: None,
    };
    host.mkdir(ctx.paths.state_log(ID).parent().unwrap().to_str().unwrap());
    ctx
}

/// The unlock check after the driver check, on that check's listing and extracted module.
fn check_in(ctx: &mut RunCtx) -> Result<(), BuildFailure> {
    let artifact = build::check_artifact(ctx, KERNEL).expect("the driver itself passes");
    check_gpu_unlock(ctx, KERNEL, &artifact)
}

fn check(image: &Image, unlock: bool) -> Result<(), BuildFailure> {
    let host = default_host();
    image.install(&host);
    check_in(&mut ctx(&host, unlock))
}

fn failed_with(result: Result<(), BuildFailure>) -> String {
    let failure = result.expect_err("the check must refuse the image");
    assert_eq!(failure.code, ErrorCode::BuildFailed);
    failure.params["reason"]["key"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn an_image_with_the_module_the_rule_and_the_firmware_passes() {
    check(&Image::with_unlock(), true).unwrap();
}

#[test]
fn the_module_must_be_there_exactly_once() {
    let module = format!("{}/extra/gpu_unlock.ko", modules());
    let image = Image::with_unlock().without(&module);
    assert_eq!(
        failed_with(check(&image, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
    for second in [
        format!("{}/kernel/drivers/video/gpu_unlock.ko", modules()),
        format!("{}/extra/more/gpu_unlock.ko", modules()),
    ] {
        let image = Image::with_unlock().with(&second);
        assert_eq!(
            failed_with(check(&image, true)),
            BUILD_ARTIFACT_GPU_UNLOCK_MODULE
        );
    }
    // Only the one path the dependency index and the rule expect counts.
    let image = Image::with_unlock()
        .without(&module)
        .with(&format!("{}/kernel/gpu_unlock.ko", modules()));
    assert_eq!(
        failed_with(check(&image, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
}

#[test]
fn the_module_vermagic_must_equal_the_drivers_and_start_with_the_kernel() {
    let mut image = Image::with_unlock();
    image.module_vermagic = Some(format!("{KERNEL} SMP other options"));
    assert_eq!(
        failed_with(check(&image, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
    // Both modules built for the wrong kernel agree with each other, and still fail.
    let mut image = Image::with_unlock();
    let wrong = "6.12.1-production+truenas SMP".to_string();
    image.module_vermagic = Some(wrong.clone());
    image.nvidia_vermagic = Some(wrong);
    assert_eq!(
        failed_with(check(&image, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
    // The kernel name must be followed by a space, not just be a prefix.
    let mut image = Image::with_unlock();
    image.module_vermagic = Some(format!("{KERNEL}x SMP"));
    image.nvidia_vermagic = Some(format!("{KERNEL}x SMP"));
    assert_eq!(
        failed_with(check(&image, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
    // A module `modinfo` cannot read.
    let mut image = Image::with_unlock();
    image.module_vermagic = None;
    assert_eq!(
        failed_with(check(&image, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
}

#[test]
fn the_module_must_be_in_the_dependency_index() {
    let mut image = Image::with_unlock();
    image.dep = "kernel/drivers/video/nvidia.ko:\n".into();
    assert_eq!(
        failed_with(check(&image, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
    let index = format!("{}/modules.dep", modules());
    let image = Image::with_unlock().without(&index);
    assert_eq!(
        failed_with(check(&image, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
}

#[test]
fn the_rule_must_be_the_generated_text_byte_for_byte() {
    let ok = rule_file(&targets());
    for bad in [
        rule_file(&targets()[..1]),
        ok.replace("vfio-pci gpu_unlock", "gpu_unlock"),
        format!("{ok}softdep nvidia pre: other\n"),
        ok.trim_end().to_string(),
        String::new(),
    ] {
        let mut image = Image::with_unlock();
        image.rule = bad;
        assert_eq!(
            failed_with(check(&image, true)),
            BUILD_ARTIFACT_GPU_UNLOCK_FILES
        );
    }
    let image = Image::with_unlock().without("usr/lib/modprobe.d/gpu-unlock.conf");
    assert_eq!(
        failed_with(check(&image, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_FILES
    );
}

#[test]
fn the_firmware_directory_holds_exactly_the_sixteen_names() {
    let dir = "usr/lib/firmware/gpu-unlock/610.43.03";
    let first = firmware_names().next().unwrap();
    let missing = Image::with_unlock().without(&format!("{dir}/{first}"));
    assert_eq!(
        failed_with(check(&missing, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_FILES
    );
    for extra in [
        format!("{dir}/stray.bin"),
        format!("{dir}/NVIDIA-LICENSE"),
        format!("{dir}/nested/{first}"),
    ] {
        let image = Image::with_unlock().with(&extra);
        assert_eq!(
            failed_with(check(&image, true)),
            BUILD_ARTIFACT_GPU_UNLOCK_FILES,
            "{extra}"
        );
    }
    let none = {
        let mut image = Image::with_unlock();
        image.files.retain(|f| !f.starts_with(dir));
        image
    };
    assert_eq!(
        failed_with(check(&none, true)),
        BUILD_ARTIFACT_GPU_UNLOCK_FILES
    );
}

#[test]
fn an_image_without_the_unlock_carries_none_of_its_paths() {
    check(&Image::plain(), false).unwrap();
    for stray in [
        format!("{}/extra/gpu_unlock.ko", modules()),
        "usr/lib/modprobe.d/gpu-unlock.conf".to_string(),
        "usr/lib/firmware/gpu-unlock/610.43.03/gsp_ga10x.bin".to_string(),
        "usr/lib/firmware/gpu-unlock".to_string(),
        "usr/share/licenses/gpu-unlock/GPL-2.0.txt".to_string(),
        "usr/share/licenses/gpu-unlock-firmware/NVIDIA-LICENSE".to_string(),
    ] {
        let image = Image::plain().with(&stray);
        assert_eq!(
            failed_with(check(&image, false)),
            BUILD_ARTIFACT_GPU_UNLOCK_UNEXPECTED,
            "{stray}"
        );
    }
}

#[test]
fn an_unlock_failure_is_logged_with_its_reason_like_the_other_checks() {
    let host = default_host();
    let image = Image::with_unlock().without("usr/lib/modprobe.d/gpu-unlock.conf");
    image.install(&host);
    let mut ctx = ctx(&host, true);
    let failure = check_in(&mut ctx).unwrap_err();
    assert_eq!(
        failure.params["reason"]["key"],
        BUILD_ARTIFACT_GPU_UNLOCK_FILES
    );
    let log = task_state::log_tail(&host, &ctx.paths, ID, None);
    let line = log
        .lines
        .iter()
        .find(|l| l.key == "log.artifact_check_failed")
        .expect("the failure line");
    assert_eq!(
        line.p["build_reason"],
        json!({"key": BUILD_ARTIFACT_GPU_UNLOCK_FILES, "params": {}})
    );
}

#[test]
fn a_build_with_the_unlock_delivers_a_row_that_records_the_targets() {
    let host = default_host();
    host.mkdir(&format!("{HOST_DATA_DIR}/manager/build-kit"));
    Image::with_unlock().install(&host);
    let mut ctx = ctx(&host, true);
    let env = BuildEnv::new(
        truenas_nvidia_driver_manager::build_kit::BUILD_ENV_IMAGE_ID.into(),
        false,
        vec!["/build-kit/a.deb".into()],
    );
    build::build(&mut ctx, &env, "gcc-12").unwrap();
    let record: serde_json::Value = serde_json::from_slice(
        &host
            .file(&format!("{HOST_DATA_DIR}/drivers/{ID}-build/record.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(record["gpu_unlock_targets"], json!(TARGETS));
    let run = host
        .argv_log()
        .into_iter()
        .find(|a| a[0] == "docker" && a[1] == "run")
        .unwrap();
    assert!(run.contains(&"NVM_GPU_UNLOCK=1".to_string()));
    // A plain build of the same task id records nothing.
    let host = default_host();
    Image::plain().install(&host);
    let mut ctx = self::ctx(&host, false);
    build::build(&mut ctx, &env, "gcc-12").unwrap();
    let record = host
        .file_text(&format!("{HOST_DATA_DIR}/drivers/{ID}-build/record.json"))
        .unwrap();
    assert!(!record.contains("gpu_unlock_targets"), "{record}");
}

#[test]
fn a_build_whose_image_lacks_the_requested_unlock_files_is_not_delivered() {
    let host = default_host();
    Image::plain().install(&host);
    let mut ctx = ctx(&host, true);
    let env = BuildEnv::new(
        truenas_nvidia_driver_manager::build_kit::BUILD_ENV_IMAGE_ID.into(),
        false,
        vec![],
    );
    let failure = build::build(&mut ctx, &env, "gcc-12").unwrap_err();
    assert_eq!(
        failure.params["reason"]["key"],
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/drivers/{ID}-build")));
}

// ---- the trial load in the build stage ---------------------------------------------------

fn build_env() -> BuildEnv {
    BuildEnv::new(
        truenas_nvidia_driver_manager::build_kit::BUILD_ENV_IMAGE_ID.into(),
        false,
        vec![],
    )
}

fn log_keys(ctx: &RunCtx) -> Vec<String> {
    task_state::log_tail(ctx.host.as_ref(), &ctx.paths, ID, None)
        .lines
        .into_iter()
        .map(|l| l.key)
        .collect()
}

#[test]
fn a_build_with_the_unlock_test_loads_the_extracted_module_before_the_row_exists() {
    let host = default_host();
    Image::with_unlock().install(&host);
    let mut ctx = ctx(&host, true);
    build::build(&mut ctx, &build_env(), "gcc-12").unwrap();
    let module = format!(
        "{HOST_DATA_DIR}/manager/tasks/{ID}/inspect-unlock/{}/extra/gpu_unlock.ko",
        modules()
    );
    assert!(host
        .argv_log()
        .contains(&vec!["insmod".to_string(), module]));
    // Delivering the local row is part of the build again.
    assert_eq!(ctx.state.phase, Some(Phase::Build));
    let keys = log_keys(&ctx);
    let at = |key: &str| keys.iter().position(|k| k == key).unwrap();
    assert!(at("log.unlock_preloaded") < at("log.row_created"));
    let lines = task_state::log_tail(ctx.host.as_ref(), &ctx.paths, ID, None).lines;
    let entered = |phase: &str| {
        lines
            .iter()
            .rposition(|l| l.key == "log.phase_enter" && l.p["phase"] == phase)
            .expect("the phase")
    };
    assert!(entered("unlock_preload") < at("log.unlock_preloaded"));
    assert!(at("log.unlock_preloaded") < entered("build"));
    assert!(entered("build") < at("log.row_created"));
}

#[test]
fn a_rejected_module_leaves_no_local_row() {
    let host = default_host();
    Image::with_unlock().install(&host);
    host.preset(
        "insmod",
        &[],
        FakeOutput::code(1)
            .with_stderr("insmod: ERROR: could not insert module: Invalid module format"),
    );
    let mut ctx = ctx(&host, true);
    let failure = build::build(&mut ctx, &build_env(), "gcc-12").unwrap_err();
    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(failure.params["reason"]["key"], GPU_UNLOCK_MODULE_REJECTED);
    assert!(failure.raw.unwrap().contains("Invalid module format"));
    assert_eq!(failure.command.unwrap().0[0], "insmod");
    assert!(!host.exists(&format!("{HOST_DATA_DIR}/drivers/{ID}-build")));
    // The task stops in the trial load, which is where the page shows it.
    assert_eq!(ctx.state.phase, Some(Phase::UnlockPreload));
}

#[test]
fn a_module_already_in_the_kernel_is_not_loaded_by_a_build() {
    let host = default_host();
    host.load_module("gpu_unlock", 0, &[]);
    Image::with_unlock().install(&host);
    let mut ctx = ctx(&host, true);
    build::build(&mut ctx, &build_env(), "gcc-12").unwrap();
    assert_eq!(host.count("insmod"), 0);
    assert!(log_keys(&ctx).contains(&"log.unlock_preload_skipped".to_string()));
}

#[test]
fn a_build_without_the_unlock_never_runs_insmod() {
    let host = default_host();
    Image::plain().install(&host);
    let mut ctx = ctx(&host, false);
    build::build(&mut ctx, &build_env(), "gcc-12").unwrap();
    assert_eq!(host.count("insmod"), 0);
    assert_ne!(ctx.state.phase, Some(Phase::UnlockPreload));
}

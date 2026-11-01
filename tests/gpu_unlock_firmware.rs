//! The firmware stage on the fake host: the cache around the generator (`stage`) with a closure
//! as the generator, and where the installer comes from. The last case generates the firmware
//! from the real NVIDIA installer and needs that file.

mod common;

use std::cell::RefCell;
use std::io::Read;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{sha256_hex, FakeHost, HOST_DATA_DIR};
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::gpu_unlock::{firmware, FixedFile, INSTALLER, OUTPUTS};
use truenas_nvidia_driver_manager::host::Host;
use truenas_nvidia_driver_manager::runner::build::BuildFailure;
use truenas_nvidia_driver_manager::runner::{unlock_firmware, RunCtx};
use truenas_nvidia_driver_manager::store::{self, Paths};
use truenas_nvidia_driver_manager::task_state::{
    self, new_state, OriginalChoice, Phase, Plan, PlanSource,
};

const ID: &str = "20261024T010203Z-1a2b3c4d";

fn cache() -> String {
    format!("{HOST_DATA_DIR}/manager/gpu-unlock/firmware/610.43.03")
}

fn staged() -> String {
    format!("{}.new", cache())
}

/// Small files under the real names, with their own pinned values.
fn outputs() -> Vec<(FixedFile, Vec<u8>)> {
    OUTPUTS
        .iter()
        .map(|f| {
            let bytes = format!("output {}", f.name).into_bytes();
            let sha256: &'static str = Box::leak(sha256_hex(&bytes).into_boxed_str());
            let file = FixedFile {
                name: f.name,
                bytes: bytes.len() as u64,
                sha256,
            };
            (file, bytes)
        })
        .collect()
}

struct World {
    host: FakeHost,
    ctx: RunCtx,
    outputs: Vec<(FixedFile, Vec<u8>)>,
}

fn world() -> World {
    let host = FakeHost::new();
    let plan = Plan {
        source: PlanSource::Official,
        version: "580.178.04".into(),
        kernel: common::KERNEL.into(),
        truenas_version: common::TRUENAS_VERSION.into(),
        original: OriginalChoice::Skip,
        image_id: format!("sha256:{}", "a".repeat(64)),
        build_timeout_minutes: 30,
        gpus: Vec::new(),
        kernel_module_type: None,
        gpu_unlock_targets: vec!["0000:01:00.0".into()],
    };
    let ctx = RunCtx {
        host: Arc::new(host.clone()),
        paths: Paths::new(HOST_DATA_DIR),
        id: ID.into(),
        state: new_state(ID, 1, plan, Phase::UnlockFirmware),
        build_deadline: None,
    };
    host.mkdir(ctx.paths.state_log(ID).parent().unwrap().to_str().unwrap());
    World {
        host,
        ctx,
        outputs: outputs(),
    }
}

impl World {
    fn put_outputs(&self, dir: &str) {
        for (file, bytes) in &self.outputs {
            self.host
                .put_file(&format!("{dir}/{}", file.name), bytes.clone());
        }
    }

    /// Run the stage with a generator that writes the expected files, after `tweak` changed the
    /// staging directory. Returns the result and how often the generator ran.
    fn stage_with(&mut self, tweak: impl FnOnce(&FakeHost)) -> (Result<(), BuildFailure>, usize) {
        let runs = RefCell::new(0);
        let host = self.host.clone();
        let files: Vec<FixedFile> = self.outputs.iter().map(|(f, _)| *f).collect();
        let contents = self.outputs.clone();
        let deadline = self.host.now() + Duration::from_secs(600);
        let result = unlock_firmware::stage(&mut self.ctx, deadline, &files, |_, dir| {
            *runs.borrow_mut() += 1;
            assert_eq!(dir, Path::new(&staged()));
            for (file, bytes) in &contents {
                host.put_file(&format!("{}/{}", dir.display(), file.name), bytes.clone());
            }
            tweak(&host);
            Ok(())
        });
        (result, runs.into_inner())
    }

    fn log_keys(&self) -> Vec<String> {
        task_state::log_tail(&self.host, &self.ctx.paths, ID, None)
            .lines
            .into_iter()
            .map(|l| l.key)
            .collect()
    }

    fn cache_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .host
            .read_dir(Path::new(&cache()))
            .map(|n| n.iter().map(|s| s.to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    fn assert_cache_is_the_outputs(&self) {
        let mut want: Vec<String> = self.outputs.iter().map(|(f, _)| f.name.into()).collect();
        want.sort();
        assert_eq!(self.cache_names(), want);
        for (file, bytes) in &self.outputs {
            assert_eq!(
                self.host
                    .file(&format!("{}/{}", cache(), file.name))
                    .unwrap(),
                *bytes,
                "{}",
                file.name
            );
        }
        assert!(!self.host.exists(&staged()));
    }
}

fn reason(failure: &BuildFailure) -> (&'static str, String) {
    let key = failure.params["reason"]["key"].as_str().unwrap_or_default();
    (failure.code.as_str(), key.to_string())
}

#[test]
fn a_complete_cache_is_reused_without_a_generator() {
    let mut w = world();
    w.put_outputs(&cache());
    let (result, runs) = w.stage_with(|_| {});
    result.unwrap();
    assert_eq!(runs, 0);
    assert!(w
        .log_keys()
        .contains(&"log.unlock_firmware_reused".to_string()));
    w.assert_cache_is_the_outputs();
}

#[test]
fn an_empty_cache_is_generated_checked_and_published() {
    let mut w = world();
    let (result, runs) = w.stage_with(|_| {});
    result.unwrap();
    assert_eq!(runs, 1);
    w.assert_cache_is_the_outputs();
    assert!(w
        .log_keys()
        .contains(&"log.unlock_firmware_generated".to_string()));
}

#[test]
fn a_cache_with_a_wrong_or_an_extra_file_is_generated_again() {
    for damage in ["wrong", "extra", "missing", "short"] {
        let mut w = world();
        w.put_outputs(&cache());
        let victim = format!("{}/{}", cache(), OUTPUTS[3].name);
        match damage {
            "wrong" => w.host.put_file(
                &victim,
                w.outputs[3].1.iter().map(|b| b ^ 1).collect::<Vec<u8>>(),
            ),
            "extra" => w
                .host
                .put_file(&format!("{}/NOTICE.md", cache()), b"notice".to_vec()),
            "missing" => w.host.remove(&victim),
            _ => w.host.put_file(&victim, b"x".to_vec()),
        }
        let (result, runs) = w.stage_with(|_| {});
        result.unwrap();
        assert_eq!(runs, 1, "{damage}");
        w.assert_cache_is_the_outputs();
    }
}

#[test]
fn the_generator_gets_an_empty_staging_directory() {
    let mut w = world();
    w.host
        .put_file(&format!("{}/stale.bin", staged()), b"old".to_vec());
    w.host.put_file(
        &format!("{}/{}", staged(), OUTPUTS[1].name),
        b"old".to_vec(),
    );
    let host = w.host.clone();
    let deadline = w.host.now() + Duration::from_secs(600);
    let files: Vec<FixedFile> = w.outputs.iter().map(|(f, _)| *f).collect();
    let seen = RefCell::new(None);
    let _ = unlock_firmware::stage(&mut w.ctx, deadline, &files, |_, dir| {
        *seen.borrow_mut() = Some(host.read_dir(dir).unwrap());
        Ok(())
    });
    assert_eq!(seen.into_inner().unwrap().len(), 0);
}

#[test]
fn a_leftover_in_the_staging_directory_never_reaches_the_cache() {
    let mut w = world();
    w.host
        .put_file(&format!("{}/stale.bin", staged()), b"old".to_vec());
    let (result, _) = w.stage_with(|_| {});
    result.unwrap();
    w.assert_cache_is_the_outputs();
}

#[test]
fn what_the_generator_wrote_is_checked_and_a_mismatch_is_never_published() {
    for (label, tweak) in [
        ("wrong", "wrong"),
        ("extra", "extra"),
        ("missing", "missing"),
    ] {
        let mut w = world();
        // A previous generation is in place; a failed stage leaves it as it was.
        w.host
            .put_file(&format!("{}/old.bin", cache()), b"previous".to_vec());
        let name = OUTPUTS[2].name;
        let (result, runs) = w.stage_with(|host| {
            let file = format!("{}/{name}", staged());
            match tweak {
                "wrong" => host.put_file(&file, b"not the pinned bytes".to_vec()),
                "extra" => host.put_file(&format!("{}/stray.bin", staged()), b"x".to_vec()),
                _ => host.remove(&file),
            }
        });
        assert_eq!(runs, 1);
        assert_eq!(
            reason(&result.unwrap_err()),
            ("BUILD_FAILED", GPU_UNLOCK_INSTALLER_UNUSABLE.into()),
            "{label}"
        );
        assert_eq!(w.cache_names(), ["old.bin"], "{label}");
    }
}

#[test]
fn a_generator_failure_goes_out_unchanged_and_leaves_the_cache_alone() {
    let mut w = world();
    w.host
        .put_file(&format!("{}/old.bin", cache()), b"previous".to_vec());
    let files: Vec<FixedFile> = w.outputs.iter().map(|(f, _)| *f).collect();
    let deadline = w.host.now() + Duration::from_secs(600);
    let failure = unlock_firmware::stage(&mut w.ctx, deadline, &files, |_, _| {
        Err(BuildFailure {
            code: ErrorCode::DownloadFailed,
            params: serde_json::json!({"reason": {"key": "download.something", "params": {}}}),
            raw: Some("raw".into()),
            command: None,
        })
    })
    .unwrap_err();
    assert_eq!(
        reason(&failure),
        ("DOWNLOAD_FAILED", "download.something".into())
    );
    assert_eq!(w.cache_names(), ["old.bin"]);
}

#[test]
fn a_stage_that_finds_no_time_left_hashing_the_cache_is_a_build_timeout() {
    let mut w = world();
    w.put_outputs(&cache());
    let files: Vec<FixedFile> = w.outputs.iter().map(|(f, _)| *f).collect();
    let deadline = w.host.now();
    let failure = unlock_firmware::stage(&mut w.ctx, deadline, &files, |_, _| {
        panic!("the generator must not run")
    })
    .unwrap_err();
    assert_eq!(failure.code, ErrorCode::BuildTimeout);
}

// ---- where the installer comes from --------------------------------------------------------

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// A small installer under the real name, with its own pinned values.
fn small_installer() -> (FixedFile, Vec<u8>) {
    let bytes = b"small installer".to_vec();
    let pin = FixedFile {
        name: INSTALLER.name,
        bytes: bytes.len() as u64,
        sha256: leak(sha256_hex(&bytes)),
    };
    (pin, bytes)
}

fn task_driver() -> String {
    format!("{HOST_DATA_DIR}/manager/tasks/{ID}/in/driver.run")
}

fn downloaded() -> String {
    format!("{HOST_DATA_DIR}/manager/tasks/{ID}/dl/{}", INSTALLER.name)
}

impl World {
    /// The task installs `version`.
    fn installing(mut self, version: &str) -> World {
        self.ctx.state.plan.as_mut().unwrap().version = version.into();
        self
    }

    fn installer(&mut self, pin: &FixedFile) -> Result<String, BuildFailure> {
        let deadline = self.host.now() + Duration::from_secs(600);
        unlock_firmware::installer(&mut self.ctx, deadline, pin).map(|p| p.display().to_string())
    }

    fn serve_installer(&self, bytes: &[u8]) {
        let url = format!("{CATALOG_URL}610.43.03/{}", INSTALLER.name);
        self.host.set_curl(&url, 200, bytes);
    }
}

#[test]
fn an_update_to_the_release_uses_its_own_installer_whatever_its_bytes() {
    // Downloaded or uploaded alike: the pinned outputs, not a hash of the input, decide.
    let mut w = world().installing("610.43.03");
    let (pin, _) = small_installer();
    w.host
        .put_file(&task_driver(), b"an uploaded variant".to_vec());
    assert_eq!(w.installer(&pin).unwrap(), task_driver());
    assert_eq!(w.host.count("curl"), 0);
}

#[test]
fn a_task_without_a_plan_fails_without_a_download() {
    let mut w = world();
    w.ctx.state.plan = None;
    w.serve_installer(b"would be downloaded");
    let failure = w.installer(&INSTALLER).unwrap_err();
    assert_eq!(
        reason(&failure),
        ("BUILD_FAILED", GPU_UNLOCK_FIRMWARE_FAILED.into())
    );
    assert_eq!(failure.raw.as_deref(), Some("the task has no plan"));
    assert_eq!(w.host.count("curl"), 0);
}

#[test]
fn an_installer_that_does_not_give_the_firmware_stops_the_task_without_a_download() {
    let mut w = world().installing("610.43.03");
    w.host
        .put_file(&task_driver(), b"not an NVIDIA installer".to_vec());
    w.serve_installer(b"would be downloaded");
    let deadline = w.host.now() + Duration::from_secs(600);
    let failure = unlock_firmware::run(&mut w.ctx, deadline).unwrap_err();
    assert_eq!(
        reason(&failure),
        ("BUILD_FAILED", GPU_UNLOCK_INSTALLER_UNUSABLE.into())
    );
    assert_eq!(w.host.count("curl"), 0);
    assert!(w.cache_names().is_empty());
}

/// Review round 1, B1: an upload whose header says 610.43.03 but whose content is cut short. The
/// plan announces no download, so the stage must not download either; it stops as unusable.
#[test]
fn review_same_selected_version_does_not_download_without_a_prompt() {
    let mut w = world().installing("610.43.03");
    let body =
        b"#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 610.43.03\"\n";
    let header = truenas_nvidia_driver_manager::upload::run_header(body).unwrap();
    assert_eq!(header[0].text(), "610.43.03");
    w.host.put_file(&task_driver(), body.to_vec());
    w.serve_installer(b"would be downloaded");
    assert!(!unlock_firmware::downloads(
        &w.host,
        Path::new(HOST_DATA_DIR),
        "610.43.03"
    ));
    let _ = w.installer(&INSTALLER);
    let deadline = w.host.now() + Duration::from_secs(600);
    let failure = unlock_firmware::run(&mut w.ctx, deadline).unwrap_err();
    assert_eq!(
        reason(&failure),
        ("BUILD_FAILED", GPU_UNLOCK_INSTALLER_UNUSABLE.into())
    );
    assert_eq!(w.host.count("curl"), 0, "no extra, unannounced download");
}

#[test]
fn another_release_downloads_the_installer_by_its_pinned_size_and_digest() {
    let mut w = world();
    let (pin, bytes) = small_installer();
    w.host
        .put_file(&task_driver(), b"the 580.178.04 installer".to_vec());
    w.serve_installer(&bytes);
    assert_eq!(w.installer(&pin).unwrap(), downloaded());
    assert_eq!(w.host.file(&downloaded()).unwrap(), bytes);
    assert!(w.ctx.state.download.is_none());
    // Other bytes of the same size are refused and never take the name.
    let mut w = world();
    w.serve_installer(&bytes.iter().map(|b| b ^ 1).collect::<Vec<u8>>());
    let failure = w.installer(&pin).unwrap_err();
    assert_eq!(
        reason(&failure),
        ("DOWNLOAD_FAILED", DOWNLOAD_CHECKSUM_MISMATCH.into())
    );
    assert!(!w.host.exists(&downloaded()));
}

#[test]
fn a_download_is_announced_only_for_another_release_without_the_cache() {
    let host = FakeHost::new();
    let downloads = |version| unlock_firmware::downloads(&host, Path::new(HOST_DATA_DIR), version);
    assert!(!downloads("610.43.03"));
    assert!(downloads("615.71.09"));
    for file in OUTPUTS {
        host.put_file(&format!("{}/{}", cache(), file.name), b"x".to_vec());
    }
    assert!(!downloads("615.71.09"));
    host.remove(&format!("{}/{}", cache(), OUTPUTS[5].name));
    assert!(downloads("615.71.09"));
}

// ---- the real installer ---------------------------------------------------------------------

/// The pinned NVIDIA installer alone gives exactly the fixed firmware, in process. It needs the
/// 344 MB file and runs in the gpu-unlock workflow (`ci/job/gpu-unlock.sh firmware`) or by hand:
/// `NVM_GPU_UNLOCK_INSTALLER=<file> cargo test --release --test gpu_unlock_firmware -- --ignored`.
#[test]
#[ignore = "needs the NVIDIA installer named by NVM_GPU_UNLOCK_INSTALLER"]
fn the_pinned_installer_alone_gives_the_fixed_firmware() {
    let path = std::env::var_os("NVM_GPU_UNLOCK_INSTALLER").expect("NVM_GPU_UNLOCK_INSTALLER");
    let open = || std::fs::File::open(&path).unwrap();
    let hash = |data: &mut dyn Read| {
        let digest = store::copy_hashed(data, &mut std::io::sink(), |_| ControlFlow::Continue(()));
        digest.map(|d| (d.size, d.sha256))
    };
    let pinned = (INSTALLER.bytes, INSTALLER.sha256.to_string());
    assert_eq!(
        hash(&mut open()).unwrap(),
        pinned,
        "not the pinned installer"
    );
    let mut made = Vec::new();
    let mut output = |name: &str, data: &mut dyn Read| {
        made.push((name.to_string(), hash(data)?));
        Ok(())
    };
    firmware::generate(open(), &mut output).unwrap();
    made.sort();
    for (name, (bytes, sha256)) in &made {
        println!("{sha256}  {bytes:>10}  {name}");
    }
    let fixed: Vec<_> = OUTPUTS
        .iter()
        .map(|f| (f.name.to_string(), (f.bytes, f.sha256.to_string())))
        .collect();
    assert_eq!(made, fixed);
}

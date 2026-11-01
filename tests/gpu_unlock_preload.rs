//! The trial load of the unlock module and the load-order item of the check, on local rows,
//! through the whole runner on the fake host.

mod common;

use std::sync::Arc;

use common::*;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::drivers::{OriginalOf, Record};
use truenas_nvidia_driver_manager::host::Host;
use truenas_nvidia_driver_manager::runner;
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{
    self, new_state, OriginalChoice, Phase, PlanSource, RowKind, Status, TaskState,
};

const ID: &str = "20260928T101530Z-3f9a0c12";
const NEW_BYTES: &[u8] = b"new driver image";
const OLD_BYTES: &[u8] = b"stock driver image";
const TARGETS: [&str; 2] = ["0000:01:00.0", "0000:02:00.0"];
const RULE_LINE: &str =
    "-rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/usr/lib/modprobe.d/gpu-unlock.conf\n";
const MODULE_LINE: &str = "-rw-r--r-- root/root 10 2026-09-28 10:15 squashfs-root/usr/lib/modules/6.12.105-production+truenas/extra/gpu_unlock.ko\n";
const SMI: [&str; 2] = ["--query-gpu=driver_version", "--format=csv,noheader"];

fn paths() -> Paths {
    Paths::new(HOST_DATA_DIR)
}

fn targets() -> Vec<String> {
    TARGETS.iter().map(|t| t.to_string()).collect()
}

fn put_row(host: &FakeHost, row_id: &str, kind: RowKind, version: &str, bytes: &[u8], rule: bool) {
    let row = paths().row_dir(row_id);
    let record = Record {
        kind,
        version: version.into(),
        sha256: sha256_hex(bytes),
        size: bytes.len() as u64,
        target_kernel: KERNEL.into(),
        created_at: 1_790_000_000_000,
        fingerprint: None,
        original_of: (kind == RowKind::Original).then(|| OriginalOf {
            kernel: KERNEL.into(),
            truenas_version: TRUENAS_VERSION.into(),
        }),
        keep: (kind == RowKind::Original).then_some(false),
        gpu_unlock_targets: if rule && kind != RowKind::Original {
            targets()
        } else {
            Vec::new()
        },
    };
    let image = row.join("nvidia.raw");
    host.put_file(image.to_str().unwrap(), bytes.to_vec());
    host.put_text(
        row.join("record.json").to_str().unwrap(),
        &serde_json::to_string(&record).unwrap(),
    );
    let mut listing = canonical_listing(KERNEL, version);
    if rule {
        listing.push_str(RULE_LINE);
        listing.push_str(MODULE_LINE);
    }
    host.set_listing(image.to_str().unwrap(), &listing);
}

/// A local update to a row that carries the rule when `row_rule`; the saved original carries
/// it when `original_rule` (`None`: no original is saved).
fn scene(row_rule: bool, original_rule: Option<bool>) -> FakeHost {
    let host = FakeHost::new();
    host.remove_mount("/usr");
    host.add_mount("tank/ROOT/default/usr", "/usr", "zfs", "ro");
    host.put_file(CANONICAL_PATH, OLD_BYTES.to_vec());
    host.set_listing(CANONICAL_PATH, &canonical_listing(KERNEL, "580.173.02"));
    host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH);
    host.load_module("nvidia", 0, &[]);
    let row_id = format!("{ID}-build");
    put_row(
        &host,
        &row_id,
        RowKind::Official,
        "580.178.04",
        NEW_BYTES,
        row_rule,
    );
    if let Some(rule) = original_rule {
        let original = format!("{ID}-original");
        put_row(
            &host,
            &original,
            RowKind::Original,
            "580.173.02",
            OLD_BYTES,
            rule,
        );
    }
    let mut plan = sample_plan(PlanSource::Local {
        row_id,
        row_kind: RowKind::Official,
        fingerprint: None,
        sha256: sha256_hex(NEW_BYTES),
    });
    plan.original = match original_rule {
        Some(_) => OriginalChoice::AlreadySaved,
        None => OriginalChoice::Skip,
    };
    if row_rule {
        plan.gpu_unlock_targets = targets();
    }
    let state = new_state(ID, 1, plan, Phase::ReadLoadedVersion);
    task_state::write(&host, &paths(), &state).unwrap();
    host
}

fn run(host: &FakeHost) -> TaskState {
    let _runner = host.enter_runner_scope();
    let code = runner::run_with(Arc::new(host.clone()) as Arc<dyn Host>, paths(), ID);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    match task_state::read(host, &paths(), ID) {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!("state unreadable: {other:?}"),
    }
}

fn log_keys(host: &FakeHost) -> Vec<String> {
    task_state::log_tail(host, &paths(), ID, None)
        .lines
        .into_iter()
        .map(|l| l.key)
        .collect()
}

fn position(keys: &[String], key: &str) -> usize {
    keys.iter()
        .position(|k| k == key)
        .unwrap_or_else(|| panic!("{key} in {keys:?}"))
}

fn index_of(host: &FakeHost, program: &str) -> Option<usize> {
    host.argv_log().iter().position(|a| a[0] == program)
}

fn phases(host: &FakeHost) -> Vec<String> {
    task_state::log_tail(host, &paths(), ID, None)
        .lines
        .into_iter()
        .filter(|l| l.key == "log.phase_enter")
        .map(|l| l.p["phase"].as_str().unwrap().to_string())
        .collect()
}

fn reject(host: &FakeHost) {
    host.preset(
        "insmod",
        &[],
        FakeOutput::code(1).with_stderr(
            "insmod: ERROR: could not insert module gpu_unlock.ko: Invalid module format",
        ),
    );
}

#[test]
fn a_local_row_with_the_rule_is_test_loaded_before_anything_is_unloaded() {
    let host = scene(true, None);
    let state = run(&host);
    assert_eq!(state.status, Status::Succeeded, "{state:?}");
    let insmod = index_of(&host, "insmod").expect("the trial load");
    assert!(insmod < index_of(&host, "rmmod").expect("the unload"));
    let argv = &host.argv_log()[insmod];
    assert!(argv[1].ends_with(&format!(
        "inspect-unlock/usr/lib/modules/{KERNEL}/extra/gpu_unlock.ko"
    )));
    let keys = log_keys(&host);
    assert!(position(&keys, "log.unlock_preloaded") < position(&keys, "log.module_unloaded"));
    let phases = phases(&host);
    assert_eq!(
        phases[phases.iter().position(|p| p == "unlock_preload").unwrap() + 1],
        "unload"
    );
}

#[test]
fn a_rejected_module_stops_a_local_task_before_any_module_is_unloaded() {
    let host = scene(true, None);
    reject(&host);
    let state = run(&host);
    assert_eq!(state.status, Status::Failed);
    let error = state.error.unwrap();
    assert_eq!(error.code, "BUILD_FAILED");
    assert_eq!(error.params["reason"]["key"], GPU_UNLOCK_MODULE_REJECTED);
    assert!(error.raw.unwrap().contains("Invalid module format"));
    assert_eq!(error.command.unwrap()[0], "insmod");
    assert_eq!(host.count("rmmod"), 0);
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
}

#[test]
fn a_module_that_is_in_the_kernel_is_not_loaded_again() {
    let host = scene(true, None);
    host.load_module("gpu_unlock", 0, &[]);
    let state = run(&host);
    assert_eq!(state.status, Status::Succeeded, "{state:?}");
    assert_eq!(host.count("insmod"), 0);
    assert!(log_keys(&host).contains(&"log.unlock_preload_skipped".to_string()));
}

#[test]
fn a_local_row_without_the_rule_enters_no_phase_and_loads_nothing() {
    let host = scene(false, None);
    let state = run(&host);
    assert_eq!(state.status, Status::Succeeded, "{state:?}");
    assert_eq!(host.count("insmod"), 0);
    assert!(!phases(&host).contains(&"unlock_preload".to_string()));
    assert!(state.checks[0].load_order.is_none());
}

fn row_image(row: &str) -> String {
    paths()
        .row_dir(row)
        .join("nvidia.raw")
        .to_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_local_image_that_cannot_be_listed_is_the_writes_failure_before_the_unload() {
    for broken in [true, false] {
        let host = scene(true, None);
        let image = row_image(&format!("{ID}-build"));
        if broken {
            host.set_unsquashfs_broken(&image);
        } else {
            host.remove(&image);
        }
        let state = run(&host);
        assert_eq!(state.status, Status::Failed, "{state:?}");
        let error = state.error.unwrap();
        assert_eq!(error.code, "WRITE_FAILED");
        assert_eq!(error.params["write_reason"]["key"], WRITE_SOURCE_UNREADABLE);
        assert!(error.command.is_some());
        assert_eq!(host.count("rmmod"), 0);
        assert_eq!(host.count("insmod"), 0);
        assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
        assert!(!phases(&host).contains(&"unload".to_string()));
    }
}

#[test]
fn a_rule_without_its_module_is_an_image_problem_and_nothing_is_loaded() {
    let host = scene(true, None);
    let image = row_image(&format!("{ID}-build"));
    let mut listing = canonical_listing(KERNEL, "580.178.04");
    listing.push_str(RULE_LINE);
    host.set_listing(&image, &listing);
    let state = run(&host);
    assert_eq!(state.status, Status::Failed, "{state:?}");
    let error = state.error.unwrap();
    assert_eq!(error.code, "BUILD_FAILED");
    assert_eq!(
        error.params["reason"]["key"],
        BUILD_ARTIFACT_GPU_UNLOCK_MODULE
    );
    assert!(error.raw.unwrap().contains("gpu_unlock.ko"));
    assert_eq!(host.count("insmod"), 0);
    assert_eq!(host.count("rmmod"), 0);
}

#[test]
fn an_original_without_the_rule_is_neither_loaded_nor_logged() {
    let host = scene(false, Some(false));
    failing_update_check(&host);
    let state = run(&host);
    assert_eq!(state.status, Status::StockRestored, "{state:?}");
    assert_eq!(host.count("insmod"), 0);
    let keys = log_keys(&host);
    assert!(!keys.contains(&"log.unlock_preloaded".to_string()));
    assert!(!keys.contains(&"log.unlock_preload_failed".to_string()));
}

#[test]
fn an_original_that_cannot_be_listed_is_logged_and_the_restore_goes_on() {
    let host = scene(false, Some(true));
    failing_update_check(&host);
    host.set_unsquashfs_broken(&row_image(&format!("{ID}-original")));
    let state = run(&host);
    assert_eq!(state.status, Status::StockRestored, "{state:?}");
    assert_eq!(host.count("insmod"), 0);
    assert!(log_keys(&host).contains(&"log.unlock_preload_failed".to_string()));
}

#[test]
fn a_failing_show_depends_fails_both_items_whatever_it_printed() {
    let host = scene(true, Some(false));
    host.preset(
        "modprobe",
        &["--show-depends", "nvidia"],
        FakeOutput::stdout(&format!(
            "insmod /m/vfio-pci.ko \ninsmod /m/gpu_unlock.ko targets={} \ninsmod /m/nvidia.ko \n",
            TARGETS.join(",")
        ))
        .with_stderr("modprobe: ERROR")
        .with_code(1),
    );
    let state = run(&host);
    assert_eq!(state.status, Status::StockRestored, "{state:?}");
    let check = &state.checks[0];
    let order = check.load_order.as_ref().unwrap();
    assert!(matches!(
        order.show_depends,
        task_state::ReadingJson::Unknown { .. }
    ));
    assert!(!order.vfio_pci_first && !order.gpu_unlock_before_nvidia);
    assert!(!check.passed);
}

#[test]
fn an_unreadable_isolation_file_leaves_the_order_unproven() {
    let host = scene(true, Some(false));
    host.put_text("/etc/modprobe.d/nvidia.conf", "x");
    host.fail_read("/etc/modprobe.d/nvidia.conf");
    let state = run(&host);
    let order = state.checks[0].load_order.as_ref().unwrap();
    assert!(matches!(
        order.nvidia_conf,
        task_state::ReadingJson::Unknown { .. }
    ));
    assert!(!order.vfio_pci_first);
    assert!(!state.checks[0].passed);
}

fn failing_update_check(host: &FakeHost) {
    host.preset_once("nvidia-smi", &SMI, FakeOutput::stdout("0.0\n"));
}

#[test]
fn a_saved_original_with_the_rule_is_test_loaded_before_it_is_put_back() {
    let host = scene(false, Some(true));
    failing_update_check(&host);
    let state = run(&host);
    assert_eq!(state.status, Status::StockRestored, "{state:?}");
    assert_eq!(host.count("insmod"), 1);
    let keys = log_keys(&host);
    assert!(position(&keys, "log.restore_start") < position(&keys, "log.unlock_preloaded"));
    // No phase of its own: the canonical file has already changed.
    assert!(!phases(&host).contains(&"unlock_preload".to_string()));
    assert!(state.checks[1].load_order.is_none());
}

#[test]
fn a_refused_original_is_logged_and_the_restore_goes_on() {
    let host = scene(false, Some(true));
    failing_update_check(&host);
    reject(&host);
    let state = run(&host);
    assert_eq!(state.status, Status::StockRestored, "{state:?}");
    let lines = task_state::log_tail(&host, &paths(), ID, None).lines;
    let line = lines
        .iter()
        .find(|l| l.key == "log.unlock_preload_failed")
        .expect("the log line");
    assert!(line.p["raw"]
        .as_str()
        .unwrap()
        .contains("Invalid module format"));
    assert_eq!(host.file(CANONICAL_PATH).as_deref(), Some(OLD_BYTES));
}

#[test]
fn vfio_pci_missing_from_an_isolated_host_fails_the_check_and_restores() {
    let host = scene(true, Some(false));
    host.put_text(
        "/etc/modprobe.d/nvidia.conf",
        "softdep nvidia pre: vfio-pci\n",
    );
    let state = run(&host);
    assert_eq!(state.status, Status::StockRestored, "{state:?}");
    let order = state.checks[0]
        .load_order
        .as_ref()
        .expect("the load-order item");
    assert!(!order.vfio_pci_first);
    assert!(!state.checks[0].passed);
    assert!(state.checks[1].load_order.is_none());
    assert!(log_keys(&host).contains(&"log.unlock_load_order".to_string()));
}

#[test]
fn a_rule_that_does_not_reach_the_order_is_recorded_and_the_task_succeeds() {
    let host = scene(true, None);
    host.preset(
        "modprobe",
        &["--show-depends", "nvidia"],
        FakeOutput::stdout("insmod /m/vfio-pci.ko \ninsmod /m/nvidia.ko \n"),
    );
    let state = run(&host);
    assert_eq!(state.status, Status::Succeeded, "{state:?}");
    let order = state.checks[0].load_order.as_ref().unwrap();
    assert!(order.vfio_pci_first);
    assert!(!order.gpu_unlock_before_nvidia);
    assert!(state.checks[0].passed);
}

#[test]
fn the_full_order_is_recorded_as_effective() {
    let host = scene(true, None);
    host.preset(
        "modprobe",
        &["--show-depends", "nvidia"],
        FakeOutput::stdout(&format!(
            "insmod /m/vfio-pci.ko \ninsmod /m/gpu_unlock.ko targets={} \ninsmod /m/nvidia.ko \n",
            TARGETS.join(",")
        )),
    );
    let state = run(&host);
    let order = state.checks[0].load_order.as_ref().unwrap();
    assert!(order.vfio_pci_first && order.gpu_unlock_before_nvidia);
}

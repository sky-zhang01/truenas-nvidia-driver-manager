//! The runner log records carry the values the log lines render.

mod common;

use std::sync::Arc;

use common::*;
use serde_json::{json, Value};

use truenas_nvidia_driver_manager::codes::ErrorCode;
use truenas_nvidia_driver_manager::facts::Reading;
use truenas_nvidia_driver_manager::runner::build::{check_artifact, deliver, Artifact};
use truenas_nvidia_driver_manager::runner::check::{check, sysext_since};
use truenas_nvidia_driver_manager::runner::{finish, RunCtx};
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{
    new_state, ErrorRecord, Pass, Phase, PlanSource, Status, TaskState,
};

const ID: &str = "20260928T101530Z-3f9a0c12";
const LOG_PATH: &str = "/var/lib/nvm/manager/state/20260928T101530Z-3f9a0c12.log";

fn running(phase: Phase) -> TaskState {
    new_state(ID, 1, sample_plan(PlanSource::Official), phase)
}

fn ctx(host: &FakeHost, state: TaskState) -> RunCtx {
    RunCtx {
        host: Arc::new(host.clone()),
        paths: Paths::new(HOST_DATA_DIR),
        id: ID.to_string(),
        state,
        build_deadline: None,
    }
}

fn log_lines(host: &FakeHost, key: &str) -> Vec<Value> {
    let text = host.file_text(LOG_PATH).expect("the task log exists");
    text.lines()
        .filter(|line| line.contains(key))
        .map(|line| serde_json::from_str(line).expect("a log line is JSON"))
        .collect()
}

#[test]
fn build_deadline_log_carries_the_wall_clock_time() {
    let host = default_host();
    host.set_wall_ms(1_790_581_000_000);
    let mut ctx = ctx(&host, running(Phase::Download));
    ctx.set_build_deadline();

    let lines = log_lines(&host, "log.build_deadline_set");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["p"]["minutes"], 30);
    // The interface renders this instant as {time}.
    assert_eq!(
        lines[0]["p"]["deadline_ms"],
        json!(1_790_581_000_000u64 + 30 * 60 * 1000)
    );
}

#[test]
fn check_log_has_one_line_per_item_with_its_readings() {
    let host = default_host();
    host.set_driver_version("580.178.04");
    host.preset(
        "nvidia-smi",
        &["--query-gpu=driver_version", "--format=csv,noheader"],
        FakeOutput::stdout("580.178.04\n"),
    );
    let mut ctx = ctx(&host, running(Phase::Check));
    let rec = check(
        &mut ctx,
        Pass::Update,
        (
            Reading::Value(1_759_050_930_123_456),
            Reading::Value(1_759_050_930_223_456),
            None,
            None,
            true,
        ),
        "580.178.04",
    );
    assert!(rec.passed);

    let lines = log_lines(&host, "log.check_item");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["p"]["check_item"], "refresh");
    assert_eq!(lines[0]["p"]["passed"], true);
    assert_eq!(
        lines[0]["p"]["since_before"]["value"],
        1_759_050_930_123_456u64
    );
    assert_eq!(
        lines[0]["p"]["since_after"]["value"],
        1_759_050_930_223_456u64
    );
    assert_eq!(lines[1]["p"]["check_item"], "version");
    assert_eq!(lines[1]["p"]["passed"], true);
    assert_eq!(lines[1]["p"]["nvidia_smi"]["value"], "580.178.04");
    assert_eq!(lines[1]["p"]["proc_version"]["value"], "580.178.04");
    assert_eq!(lines[1]["p"]["expected"], "580.178.04");
}

#[test]
fn check_log_marks_distinct_times_unconfirmed_when_unmerge_was_not_confirmed() {
    let host = default_host();
    host.set_driver_version("580.178.04");
    host.preset(
        "nvidia-smi",
        &["--query-gpu=driver_version", "--format=csv,noheader"],
        FakeOutput::stdout("580.178.04\n"),
    );
    let mut ctx = ctx(&host, running(Phase::Check));
    let rec = check(
        &mut ctx,
        Pass::Update,
        (Reading::Value(1), Reading::Value(2), None, None, false),
        "580.178.04",
    );
    assert!(!rec.refresh_ok);
    assert!(rec.version_ok);
    assert!(!rec.passed);
    let lines = log_lines(&host, "log.check_item");
    assert_eq!(lines[0]["p"]["check_item"], "refresh");
    assert_eq!(lines[0]["p"]["passed"], false);
    assert_eq!(lines[0]["p"]["since_before"]["value"], 1);
    assert_eq!(lines[0]["p"]["since_after"]["value"], 2);
}

#[test]
fn sysext_log_keeps_each_streams_last_hundred_lines_without_truncating_the_parse() {
    let host = default_host();
    let status = r#"[{"hierarchy":"/usr","extensions":["nvidia"],"since":123}]"#;
    let stdout = format!("{status}\n{}", " \n".repeat(120));
    let stderr = (0..120)
        .map(|i| format!("err{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    host.preset_once(
        "systemd-sysext",
        &["status", "--json=short"],
        FakeOutput {
            exit: truenas_nvidia_driver_manager::host::Exit::Code(0),
            stdout: stdout.into_bytes(),
            stderr: stderr.into_bytes(),
            delay: std::time::Duration::ZERO,
        },
    );
    let ctx = ctx(&host, running(Phase::Refresh));
    assert_eq!(sysext_since(&ctx).0, Reading::Value(123));
    let lines = log_lines(&host, "log.cmd");
    assert_eq!(lines.len(), 1);
    let saved_stdout = lines[0]["p"]["stdout"].as_str().unwrap();
    let saved_stderr = lines[0]["p"]["stderr"].as_str().unwrap();
    assert_eq!(saved_stdout.lines().count(), 100);
    assert!(!saved_stdout.contains("hierarchy"));
    let stderr_lines = saved_stderr.lines().collect::<Vec<_>>();
    assert_eq!(stderr_lines.len(), 100);
    assert_eq!(stderr_lines[0], "err20");
    assert_eq!(stderr_lines[99], "err119");
}

#[test]
fn finish_logs_gpu_users_with_their_types() {
    let host = default_host();
    host.set_docker_ps(&[&"a".repeat(64)]);
    host.set_docker_inspect(
        r#"[{"Name":"/jellyfin","Config":{"Env":[]},"HostConfig":{"DeviceRequests":[{"Driver":"nvidia","Capabilities":[["gpu"]]}]}}]"#,
    );
    let mut state = running(Phase::ReadLoadedVersionEnd);
    state.status = Status::Failed;
    state.error = Some(ErrorRecord::new(
        ErrorCode::GpuInUse,
        json!({"occupancy": "busy"}),
    ));
    let mut ctx = ctx(&host, state);
    assert!(finish(&mut ctx).is_ok());

    let lines = log_lines(&host, "log.gpu_users");
    assert_eq!(lines.len(), 1);
    let applications = lines[0]["p"]["applications"]
        .as_array()
        .expect("applications is a list");
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0]["kind"], "docker");
    assert!(applications[0]["name"]
        .as_str()
        .expect("a name")
        .contains("jellyfin"));
    // Unreadable categories are reported separately, never mixed into the application list.
    assert!(lines[0]["p"]["errors"].as_array().is_some());
}

fn upload_state(fingerprint: &str) -> TaskState {
    new_state(
        ID,
        1,
        sample_plan(PlanSource::Upload {
            run_sha256: "a".repeat(64),
            fingerprint: fingerprint.to_string(),
            deb_sha256: std::collections::BTreeMap::new(),
        }),
        Phase::Build,
    )
}

/// Pre-delivery failure logs preserve observed reason parameters so the interface can show the
/// actual kernel.
#[test]
fn artifact_check_failure_log_keeps_the_observed_kernel_params() {
    let host = default_host();
    host.set_uname("6.12.105-production+truenas");
    let mut ctx = ctx(&host, running(Phase::Build));
    let out = ctx.task_dir().join("out");
    put_build_output(&host, ID, "580.178.04", b"payload");
    let listing = "drwxr-xr-x root/root 0 2026-09-28 10:15 squashfs-root/usr/lib/modules/6.12.96\n";
    host.preset(
        "unsquashfs",
        &["-lls", out.join("nvidia.raw").to_str().unwrap()],
        FakeOutput::stdout(listing),
    );

    let failure = check_artifact(&mut ctx, "6.12.105-production+truenas")
        .expect_err("the built kernel differs from the host's");
    assert_eq!(failure.code, ErrorCode::BuildFailed);
    let lines = log_lines(&host, "log.artifact_check_failed");
    assert_eq!(lines.len(), 1, "one pre-delivery failure line");
    assert_eq!(
        lines[0]["p"]["build_reason"]["key"],
        "artifact.kernel_mismatch"
    );
    assert_eq!(
        lines[0]["p"]["build_reason"]["params"]["built_kernel"],
        "6.12.96"
    );
    assert_eq!(
        lines[0]["p"]["build_reason"]["params"]["current_kernel"],
        "6.12.105-production+truenas"
    );
}

/// `log.row_created` carries the published row's source and upload fingerprint.
#[test]
fn row_created_log_carries_the_published_source_and_upload_fingerprint() {
    let host = default_host();
    let mut ctx = ctx(&host, upload_state("A1B2C3D4"));
    let out = ctx.task_dir().join("out");
    host.put_text(out.join("nvidia.raw").to_str().unwrap(), "payload");
    let artifact = Artifact {
        sha256: "a".repeat(64),
        size: 7,
        version: "580.178.04".to_string(),
        target_kernel: "6.12.105-production+truenas".to_string(),
    };
    let row_id = deliver(&mut ctx, &artifact).expect("the row is delivered");

    let lines = log_lines(&host, "log.row_created");
    assert_eq!(lines.len(), 1, "one row-created line");
    assert_eq!(lines[0]["p"]["source"], "upload");
    assert_eq!(lines[0]["p"]["fingerprint"], "A1B2C3D4");
    assert_eq!(lines[0]["p"]["version"], "580.178.04");
    let record_path = format!("{HOST_DATA_DIR}/drivers/{row_id}/record.json");
    let record: Value =
        serde_json::from_str(&host.file_text(&record_path).expect("the record exists")).unwrap();
    assert_eq!(record["fingerprint"], "A1B2C3D4");
    assert_eq!(record["kind"], "upload");
}

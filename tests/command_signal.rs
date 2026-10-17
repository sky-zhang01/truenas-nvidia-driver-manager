//! A real signal must never be reported as a process exit code.

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::catalog;
use truenas_nvidia_driver_manager::codes::{CONTAINER_DATA_DIR, HOST_QUERY_TIMEOUT};
use truenas_nvidia_driver_manager::host::{Exit, Output, Reason};
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::{facts, gpu_users, tasks};

fn output(exit: Exit, stderr: &[u8]) -> Output {
    Output {
        exit,
        stdout: Vec::new(),
        stderr: stderr.to_vec(),
        elapsed: Duration::ZERO,
    }
}

#[test]
fn host_reason_distinguishes_signal_from_exit_code() {
    let signaled = output(Exit::Signal(15), b"");
    assert_eq!(
        signaled.failure_reason(Duration::from_secs(180)).to_json(),
        json!({"key": "reason.signal", "params": {"signal": 15}})
    );
    assert_eq!(signaled.exit.to_json(), json!({"signal": 15}));
    assert_eq!(
        output(Exit::Code(15), b"")
            .failure_reason(Duration::from_secs(180))
            .to_json(),
        json!({"key": "reason.exit_status", "params": {"code": 15}})
    );
    assert_eq!(
        output(Exit::Signal(15), b"original diagnostic").failure_reason(Duration::from_secs(180)),
        Reason::Stderr("original diagnostic".into())
    );
}

#[test]
fn catalog_reason_distinguishes_signal_from_curl_exit_code() {
    assert_eq!(
        catalog::fetch_failure_reason(&output(Exit::Signal(15), b"curl diagnostic")).to_json(),
        json!({"key": "reason.signal", "params": {"signal": 15}})
    );
    assert_eq!(
        catalog::fetch_failure_reason(&output(Exit::Code(15), b"curl diagnostic")).to_json(),
        json!({"key": "reason.exit_status", "params": {"code": 15}})
    );
}

#[test]
fn canonical_file_query_keeps_signal_and_raw_diagnostic() {
    let host = default_host();
    host.preset_once(
        "unsquashfs",
        &["-lls"],
        FakeOutput {
            exit: Exit::Signal(15),
            ..FakeOutput::stderr("unsquashfs: interrupted")
        },
    );
    let failure = facts::canonical_facts(&host).unwrap_err();
    assert_eq!(
        failure.reason_json(),
        json!({"key": "reason.signal", "params": {"signal": 15}})
    );
    assert_eq!(failure.raw.as_deref(), Some("unsquashfs: interrupted"));
}

#[test]
fn unit_query_signal_is_unreadable_without_an_invented_exit_code() {
    let host = default_host();
    host.preset_once(
        "systemctl",
        &["list-units"],
        FakeOutput {
            exit: Exit::Signal(15),
            ..FakeOutput::stderr("systemctl: interrupted")
        },
    );
    let reconciled = tasks::reconcile(&host, &Paths::new(CONTAINER_DATA_DIR));
    let reason = reconciled.unit_query_error.unwrap();
    assert_eq!(reason.key, "reason.signal");
    assert_eq!(reason.params, json!({"signal": 15}));
}

#[test]
fn gpu_user_query_signal_is_unreadable_without_an_invented_exit_code() {
    let host = default_host();
    host.preset_once(
        "docker",
        &["ps"],
        FakeOutput {
            exit: Exit::Signal(15),
            ..FakeOutput::ok()
        },
    );
    let users = gpu_users::list(&host, HOST_QUERY_TIMEOUT);
    let reason = users
        .errors
        .iter()
        .find(|error| error.kind == gpu_users::UserKind::Docker)
        .expect("docker query failed");
    assert_eq!(reason.reason.key, "reason.signal");
    assert_eq!(reason.reason.params, json!({"signal": 15}));
}

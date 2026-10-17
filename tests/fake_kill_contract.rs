//! The FakeHost runner-interruption capability. `systemd-run` starts a real background thread that
//! calls `runner::run`; `kill_unit` must model the runner being killed: the thread stops at the
//! step where the test killed it, the unit becomes failed, and releasing the gate afterwards must
//! not let the thread run another command or write anything. These tests exercise the model itself
//! (the runner threads here are the ones the fake `systemd-run` builtin starts); the ordinary
//! release, reboot and reconcile behaviour around it stays covered by the other integration files.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_host::Gate;
use common::*;
use truenas_nvidia_driver_manager::codes::{CONTAINER_DATA_DIR, TOOLKIT_INDEX_URL};
use truenas_nvidia_driver_manager::host::{At, Cmd, Host};
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{Phase, Status};
use truenas_nvidia_driver_manager::tasks;

const ID_A: &str = "20260928T101530Z-3f9a0c12";
const ID_B: &str = "20260928T101530Z-3f9a0c13";
const WAIT: Duration = Duration::from_secs(5);

fn container_paths() -> Paths {
    Paths::new(CONTAINER_DATA_DIR)
}

fn state_path(id: &str) -> String {
    container_paths().state(id).to_string_lossy().into_owned()
}

/// The parsed state file; the tests read the product's JSON, never a substring, so formatting
/// cannot decide an assertion.
fn state_json(host: &FakeHost, id: &str) -> serde_json::Value {
    let text = host
        .file_text(&state_path(id))
        .expect("the state file exists");
    serde_json::from_str(&text).expect("the state file is JSON")
}

/// The state file bytes before the test does anything else; every kill test binds its "before" to
/// this file.
fn state_before(host: &FakeHost, id: &str) -> Vec<u8> {
    let bytes = host.file(&state_path(id)).expect("the state file exists");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("the state file is JSON");
    assert_eq!(value["status"], "RUNNING", "{value}");
    bytes
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn unit_show(host: &FakeHost, id: &str) -> String {
    let out = host.run(
        &Cmd::new(At::Host, "systemctl", WAIT)
            .arg("show")
            .arg(format!("nvm-task-{id}.service"))
            .arg("--property=LoadState,ActiveState,SubState,Result,ExecMainStatus"),
    );
    out.stdout_text()
}

fn curl_calls(host: &FakeHost) -> usize {
    host.argv_log()
        .iter()
        .filter(|argv| argv.first().map(String::as_str) == Some("curl"))
        .count()
}

/// Start one official task and block its runner at the first gated `curl`.
fn gated_download_host(id: &str) -> (FakeHost, Arc<Gate>) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let gate = host.set_gate("curl", &["--silent"]);
    write_state(&host, id, Status::Running, Some(Phase::Download));
    let started = tasks::start(&host, Path::new(HOST_DATA_DIR), id);
    assert!(matches!(started, tasks::StartResult::Started));
    wait_until("the gated download", || gate.blocked());
    (host, gate)
}

/// The happy download replies of the official plan; the task then reaches the gated build
/// container.
fn toolkit_index() -> (String, Vec<(String, Vec<u8>)>) {
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
            "Package: {name}\nArchitecture: amd64\nVersion: 1.20.1-1\nFilename: ./{name}_1.20.1-1_amd64.deb\nSize: {}\nSHA256: {}\n\n",
            body.len(),
            sha256_hex(body)
        ));
        responses.push((
            format!("https://nvidia.github.io/libnvidia-container/stable/deb/amd64/{name}_1.20.1-1_amd64.deb"),
            body.to_vec(),
        ));
    }
    (index, responses)
}

/// Start one official task whose downloads pass and whose runner blocks at the gated `docker run`
/// of the build stage. The compiling task gets an already prepared build environment.
fn gated_build_host(id: &str) -> (FakeHost, Arc<Gate>) {
    let host = default_host();
    host.prepare_build_env();
    prime_canonical(&host, "580.173.02");
    write_state(&host, id, Status::Running, Some(Phase::Download));
    let (index, responses) = toolkit_index();
    host.set_curl(TOOLKIT_INDEX_URL, 200, index.as_bytes());
    for (url, body) in &responses {
        host.set_curl(url, 200, body);
    }
    host.set_curl(
        "https://download.nvidia.com/XFree86/Linux-x86_64/580.178.04/NVIDIA-Linux-x86_64-580.178.04-no-compat32.run",
        200,
        b"installer",
    );
    let gate = host.set_gate("docker", &["run"]);
    let started = tasks::start(&host, Path::new(HOST_DATA_DIR), id);
    assert!(matches!(started, tasks::StartResult::Started));
    wait_until("the gated build container", || gate.blocked());
    (host, gate)
}

#[test]
fn a_kill_stops_the_gated_download_runner() {
    let (host, gate) = gated_download_host(ID_A);
    let before = state_before(&host, ID_A);
    let commands = host.commands().len();
    let ops = host.ops().len();

    host.kill_unit(ID_A);
    gate.release();
    host.wait_for_units(WAIT);

    assert_eq!(
        Some(before),
        host.file(&state_path(ID_A)),
        "the killed runner wrote state after the gate was released"
    );
    assert_eq!(
        commands,
        host.commands().len(),
        "the killed runner ran another host command"
    );
    assert_eq!(
        ops,
        host.ops().len(),
        "the killed runner touched the sandbox after its death"
    );
    assert!(
        host.unit_ids().contains(&ID_A.to_string()),
        "the killed unit disappeared"
    );
    let show = unit_show(&host, ID_A);
    assert!(show.contains("ActiveState=failed"), "{show}");
    assert!(show.contains("SubState=failed"), "{show}");
    assert!(show.contains("Result=signal"), "{show}");
}

#[test]
fn a_kill_stops_the_gated_build_runner() {
    let (host, gate) = gated_build_host(ID_A);
    let before = state_before(&host, ID_A);
    let commands = host.commands().len();
    let ops = host.ops().len();

    host.kill_unit(ID_A);
    gate.release();
    host.wait_for_units(WAIT);

    assert_eq!(
        Some(before),
        host.file(&state_path(ID_A)),
        "the killed runner wrote state after the gate was released"
    );
    assert_eq!(
        commands,
        host.commands().len(),
        "the killed runner ran another host command"
    );
    assert_eq!(
        ops,
        host.ops().len(),
        "the killed runner touched the sandbox after its death"
    );
    let show = unit_show(&host, ID_A);
    assert!(show.contains("ActiveState=failed"), "{show}");
}

#[test]
fn a_repeated_or_unknown_kill_changes_nothing() {
    let (host, gate) = gated_download_host(ID_A);
    let before = state_before(&host, ID_A);
    let commands = host.commands().len();

    host.kill_unit(ID_A);
    host.kill_unit(ID_A);
    host.kill_unit("20990101T000000Z-00000000");
    gate.release();
    host.wait_for_units(WAIT);

    assert_eq!(
        Some(before),
        host.file(&state_path(ID_A)),
        "a repeated kill released the dead runner"
    );
    assert_eq!(
        commands,
        host.commands().len(),
        "a repeated kill let the runner run another host command"
    );
}

#[test]
fn another_unit_finishes_while_a_killed_unit_stays_put() {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let gate = host.set_gate("curl", &["--silent"]);
    write_state(&host, ID_A, Status::Running, Some(Phase::Download));
    write_state(&host, ID_B, Status::Running, Some(Phase::Download));
    assert!(matches!(
        tasks::start(&host, Path::new(HOST_DATA_DIR), ID_A),
        tasks::StartResult::Started
    ));
    wait_until("the gated download", || curl_calls(&host) >= 1);

    let before = state_before(&host, ID_A);
    host.kill_unit(ID_A);
    gate.release();
    host.wait_for_units(WAIT);

    assert_eq!(
        Some(before),
        host.file(&state_path(ID_A)),
        "the killed unit kept running"
    );
    assert!(
        host.unit_ids().contains(&ID_A.to_string()),
        "the killed unit disappeared"
    );
    // The execution lock is per host: the killed runner released it when its thread unwound, so the
    // second admitted unit can run to its own end. A live runner would instead refuse a concurrent
    // execution.
    assert!(matches!(
        tasks::start(&host, Path::new(HOST_DATA_DIR), ID_B),
        tasks::StartResult::Started
    ));
    host.wait_for_units(WAIT);
    let other = state_json(&host, ID_B);
    assert_eq!(other["status"], "FAILED", "{other}");
    assert!(
        !host.unit_ids().contains(&ID_B.to_string()),
        "the surviving unit did not finish"
    );
}

#[test]
fn an_uninterrupted_gate_release_still_completes_the_runner() {
    let (host, gate) = gated_download_host(ID_A);
    let commands = host.commands().len();

    gate.release();
    host.wait_for_units(WAIT);

    let value = state_json(&host, ID_A);
    assert_eq!(value["status"], "FAILED", "{value}");
    assert!(
        !host.unit_ids().contains(&ID_A.to_string()),
        "the finished unit stayed behind"
    );
    assert!(
        host.commands().len() > commands,
        "the released runner ran no further host command"
    );
}

#[test]
fn the_killed_unit_is_reconciled_as_interrupted() {
    let (host, gate) = gated_download_host(ID_A);
    host.kill_unit(ID_A);
    gate.release();
    host.wait_for_units(WAIT);

    let reconciled = tasks::reconcile(&host, &container_paths());
    let task = reconciled.state_of(ID_A).expect("the reconciled task");
    assert_eq!(task.status, Some(Status::Interrupted));
    assert!(
        task.state_read_error.is_none() && task.state_note.is_none(),
        "the killed runner's state file was not readable: {task:?}"
    );
    let value = state_json(&host, ID_A);
    assert_eq!(value["status"], "INTERRUPTED", "{value}");

    // The reboot control: units disappear and the interrupted task is still what a reconcile
    // reports afterwards.
    host.reboot_host();
    assert!(host.unit_ids().is_empty(), "units survived a reboot");
    let again = tasks::reconcile(&host, &container_paths());
    assert_eq!(
        again.state_of(ID_A).unwrap().status,
        Some(Status::Interrupted)
    );
}

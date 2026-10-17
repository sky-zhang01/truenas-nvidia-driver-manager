//! FakeHost sandbox and startup parser/classifier boundary tests. Each case creates its own inputs
//! and prints a machine-readable line with its actual classification. Scope: the FakeHost model and
//! the startup parser/classifier only. No claim about the real host, mounts, or `nsenter`
//! reachability.

mod common;

use common::*;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Duration;

use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::host::{At, Cmd, Create, Exit, Host};
use truenas_nvidia_driver_manager::startup::{self, MappingReason, StartupProblem};
use truenas_nvidia_driver_manager::store::{random_hex, Paths};

fn paths() -> Paths {
    Paths::new(CONTAINER_DATA_DIR)
}

fn marker(tag: &str) -> String {
    format!("boundary-{tag}-{}", random_hex(8))
}

/// A real file just outside the sandbox carrying a distinct sentinel; the test removes it after
/// checking that virtual-path operations did not touch it.
fn outside_marker(host: &FakeHost, name: &str) -> PathBuf {
    let outside = host.sandbox_root().parent().unwrap().join(name);
    assert!(outside.starts_with(std::env::temp_dir()));
    std::fs::write(&outside, b"outside sentinel").unwrap();
    outside
}

fn assert_outside_untouched(outside: &Path) {
    assert_eq!(
        std::fs::read(outside).unwrap(),
        b"outside sentinel",
        "the outside marker was read, written or removed"
    );
    std::fs::remove_file(outside).unwrap();
}

#[test]
fn root_parent_never_reaches_outside_for_reads() {
    let host = FakeHost::new();
    let name = marker("read");
    let outside = outside_marker(&host, &name);
    host.put_text(&format!("/{name}"), "virtual sentinel");
    host.put_link("/escape", &format!("../{name}"));
    host.put_link("/dir/deep", &format!("../../{name}"));
    host.put_link("/abs", &format!("/{name}"));
    host.put_link("/abs-parent", &format!("/../{name}"));
    host.put_link("/parent-dir/link", &format!("../../{name}"));
    let cases = [
        format!("/{name}"),
        format!("/../{name}"),
        format!("/missing/../../{name}"),
        format!("/dir/../{name}"),
        "/escape".to_string(),
        "/dir/deep".to_string(),
        "/abs".to_string(),
        "/abs-parent".to_string(),
        "/parent-dir/link".to_string(),
    ];
    let mut failures = Vec::new();
    for case in &cases {
        match host.read(Path::new(case)) {
            Ok(bytes) if bytes == b"virtual sentinel" => {}
            other => failures.push(format!("{case}: {other:?}")),
        }
    }
    println!("sandbox_read cases={cases:?} expected=virtual sentinel failures={failures:?}");
    assert_outside_untouched(&outside);
    assert!(
        failures.is_empty(),
        "reads escaped the virtual root: {failures:?}"
    );
    host.assert_red_lines();
}

#[test]
fn mutation_callers_share_the_root_guard() {
    let host = FakeHost::new();
    let name = marker("write");
    let outside = outside_marker(&host, &name);

    // Create through a root-parent path writes the sandbox copy only
    {
        let mut writer = host
            .create(Path::new(&format!("/../{name}")), Create::New, 0o600)
            .unwrap();
        writer.write_all(b"payload").unwrap();
        writer.sync_all().unwrap();
    }
    assert_eq!(
        std::fs::read(host.sandbox_root().join(&name)).unwrap(),
        b"payload"
    );
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        b"outside sentinel",
        "create wrote the outside marker"
    );

    // Rename through root-parent paths moves the sandbox copy only
    let moved = format!("{name}-moved");
    host.rename(
        Path::new(&format!("/../{name}")),
        Path::new(&format!("/../{moved}")),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(host.sandbox_root().join(&moved)).unwrap(),
        b"payload"
    );
    assert!(!host.sandbox_root().parent().unwrap().join(&moved).exists());

    // Remove through a root-parent path leaves the outside namesake alone
    host.remove_file(Path::new(&format!("/../{moved}")))
        .unwrap();
    assert!(!host.sandbox_root().join(&moved).exists());
    assert!(outside.exists(), "remove_file deleted the outside namesake");

    // Create_dir and fsync_dir use the same boundary
    host.create_dir(Path::new("/../fsync-dir"), 0o755).unwrap();
    host.fsync_dir(Path::new("/../fsync-dir")).unwrap();
    assert!(host.sandbox_root().join("fsync-dir").is_dir());
    assert!(!host
        .sandbox_root()
        .parent()
        .unwrap()
        .join("fsync-dir")
        .exists());

    std::fs::remove_file(&outside).unwrap();
    println!(
        "sandbox_mutation name={name} moved={moved} sandbox={:?} pass=true",
        host.sandbox_root()
    );
    host.assert_red_lines();
}

#[test]
fn symlink_loop_fails_closed_without_escape() {
    let host = FakeHost::new();
    let name = marker("loop");
    let outside = outside_marker(&host, &name);
    host.put_text(&format!("/{name}"), "virtual sentinel");
    host.put_link("/self-loop", "/self-loop");

    let direct = host.read(Path::new("/self-loop"));
    let via_parent = host.read(Path::new(&format!("/../self-loop/{name}")));
    // `file` goes through the FakeHost helper fallback for unresolvable paths.
    let helper = host.file("/../self-loop");
    println!("sandbox_symlink_loop direct={direct:?} via_parent={via_parent:?} helper={helper:?}");
    assert!(direct.is_err(), "a self-loop must fail, got {direct:?}");
    assert!(
        via_parent.is_err(),
        "a loop through the root must fail, got {via_parent:?}"
    );
    assert!(
        helper.is_none(),
        "the fallback must not resolve the loop, got {helper:?}"
    );
    assert_outside_untouched(&outside);
    host.assert_red_lines();
}

fn mount_row(n: u32, id: &str, suffix: &str) -> String {
    format!(
        "{n} 1 0:30 /var/lib/docker/containers/{id}/{suffix} /etc/hostname rw,relatime - ext4 /dev/sda1 rw\n"
    )
}

#[test]
fn exactly_one_container_hostname_mount_is_required() {
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    let valid = mount_row(25, &a, "hostname");
    let cases = [
        (
            "two-same-id",
            format!("{valid}{}", mount_row(26, &a, "hostname")),
        ),
        (
            "two-distinct-id",
            format!("{valid}{}", mount_row(26, &b, "hostname")),
        ),
        (
            "invalid-then-valid",
            format!("{}{valid}", mount_row(26, &b, "not-hostname")),
        ),
    ];
    for (name, text) in &cases {
        // The inspect answer is registered so a call would be recorded and visible in the command
        // log; it must not run.
        let inner = default_host();
        inner.set_docker_inspect(&inspect_json(
            "runc",
            &[(HOST_DATA_DIR, CONTAINER_DATA_DIR)],
        ));
        let host = inner.clone();
        host.put_file("/proc/self/mountinfo", text.as_bytes().to_vec());
        let parsed = startup::container_id_from_mountinfo(text);
        let state = startup::check(&host, &paths());
        let reason_key = match state.problems.first() {
            Some(StartupProblem::DataMapping { reason }) => reason
                .to_json()
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        };
        let ok = parsed.is_none()
            && matches!(
                state.problems.first(),
                Some(StartupProblem::DataMapping {
                    reason: MappingReason::NoHostnameLine
                })
            )
            && reason_key == "startup.mapping.no_hostname_line"
            && inner.commands().is_empty();
        println!(
            "hostname_mapping case={name} input={text:?} parser={parsed:?} reason_key={reason_key} commands={:?} pass={ok}",
            inner.commands()
        );
        assert!(ok, "case {name} failed: {state:?}");
        inner.assert_red_lines();
    }
}

#[test]
fn hostname_mount_classification_handles_valid_and_invalid_rows() {
    let a = "a".repeat(64);
    assert_eq!(
        startup::container_id_from_mountinfo(&mount_row(25, &a, "hostname")).as_deref(),
        Some(a.as_str()),
        "single-valid"
    );
    assert_eq!(
        startup::container_id_from_mountinfo("1 0 0:1 / / rw - tmpfs tmpfs rw\n"),
        None,
        "zero"
    );
    assert_eq!(
        startup::container_id_from_mountinfo(&mount_row(25, &a, "wrong")),
        None,
        "single-wrong-suffix"
    );
    assert_eq!(
        startup::container_id_from_mountinfo(&mount_row(25, &"A".repeat(64), "hostname")),
        None,
        "single-upper-id"
    );
    // A second row for another mount point is not a hostname row: exactly one still holds and the
    // id is taken (normal control, not a relaxation).
    let other_row = format!(
        "{}26 1 0:31 /somewhere /data rw - ext4 /dev/sdb1 rw\n",
        mount_row(25, &a, "hostname")
    );
    assert_eq!(
        startup::container_id_from_mountinfo(&other_row).as_deref(),
        Some(a.as_str()),
        "one-hostname-plus-other-mount"
    );
    println!("hostname_mapping_controls cases=single-valid,zero,single-wrong-suffix,single-upper-id,one-hostname-plus-other-mount pass=true");
}

#[test]
fn namespace_entry_failure_requires_exit_one_and_the_exact_prefix() {
    let cases: Vec<(&str, Exit, &str, &str, serde_json::Value)> = vec![
        (
            "exit2-prefix",
            Exit::Code(2),
            "nsenter: independently injected raw",
            REASON_EXIT_STATUS,
            serde_json::json!({"code": 2}),
        ),
        (
            "timeout-prefix",
            Exit::TimedOut,
            "nsenter: independently injected raw",
            REASON_TIMED_OUT,
            serde_json::json!({"seconds": 30}),
        ),
        (
            "exit1-leading-space",
            Exit::Code(1),
            " nsenter: independently injected raw",
            REASON_EXIT_STATUS,
            serde_json::json!({"code": 1}),
        ),
    ];
    for (name, exit, stderr, key, params) in cases {
        let host = default_host();
        let mut output = FakeOutput::code(1).with_stderr(stderr);
        output.exit = exit;
        host.preset("docker", &["inspect"], output);
        let state = startup::check(&host, &paths());
        let ok = matches!(
            state.problems.first(),
            Some(StartupProblem::DataMapping {
                reason: MappingReason::InspectFailed { reason, raw }
            }) if reason.key == key && reason.params == params && raw.as_deref() == Some(stderr)
        );
        println!(
            "namespace_entry case={name} exit={exit:?} stderr={stderr:?} problems={:?} pass={ok}",
            state.problems
        );
        assert!(ok, "case {name} failed: {:?}", state.problems);
        host.assert_red_lines();
    }
}

#[test]
fn namespace_entry_classification_preserves_exit_and_stderr() {
    // The one nsenter shape: exit 1 and the exact prefix.
    let host = default_host();
    host.preset(
        "docker",
        &["inspect"],
        FakeOutput::code(1).with_stderr("nsenter: cannot open /proc/1/ns/mnt"),
    );
    let state = startup::check(&host, &paths());
    assert!(matches!(
        state.problems.first(),
        Some(StartupProblem::DataMapping {
            reason: MappingReason::NsenterFailed
        })
    ));
    assert_eq!(
        MappingReason::NsenterFailed.to_json()["key"],
        "startup.mapping.nsenter_failed"
    );

    // A plain docker failure: inspect_failed with the key, reason and the untrimmed raw text.
    let raw = "docker: Error response from daemon: No such object: bogus";
    let host = default_host();
    host.preset("docker", &["inspect"], FakeOutput::code(2).with_stderr(raw));
    let state = startup::check(&host, &paths());
    let json = state.problems.first().unwrap().to_json();
    assert_eq!(json["key"], "startup.data_mapping");
    assert_eq!(
        json["params"]["mapping_reason"]["key"],
        "startup.mapping.inspect_failed"
    );
    assert_eq!(
        json["params"]["mapping_reason"]["params"]["read_reason"]["key"],
        REASON_EXIT_STATUS
    );
    assert_eq!(
        json["params"]["mapping_reason"]["params"]["read_reason"]["params"]["code"],
        2
    );
    assert_eq!(state.problems.first().unwrap().raw(), Some(raw));
    println!("namespace_entry_controls nsenter_exact=pass inspect_exit2_raw={raw:?} pass=true");
    host.assert_red_lines();
}

// 6. the test-end red-line assertion cannot be disarmed -----------

fn collect_command() -> Cmd {
    Cmd::new(At::Host, "systemd-run", Duration::from_secs(5)).arg(concat!("--", "collect"))
}

#[test]
fn guard_end_detects_a_violation_without_an_early_assert() {
    let caught = catch_unwind(AssertUnwindSafe(|| {
        let host = FakeHost::new();
        host.preset("systemd-run", &[], FakeOutput::ok());
        host.run(&collect_command());
    }));
    println!("GUARD_END no_prior_assert end_detected={}", caught.is_err());
    assert!(
        caught.is_err(),
        "the test-end guard must detect the recorded violation"
    );
}

#[test]
fn guard_end_does_not_panic_twice_for_a_captured_violation() {
    let host = FakeHost::new();
    host.preset("systemd-run", &[], FakeOutput::ok());
    host.run(&collect_command());
    let first = catch_unwind(AssertUnwindSafe(|| host.assert_red_lines()));
    let second = catch_unwind(AssertUnwindSafe(|| drop(host)));
    println!(
        "GUARD_END caught_previous_violation={} duplicate_panic={}",
        first.is_err(),
        second.is_err()
    );
    assert!(first.is_err(), "the explicit assert must see the violation");
    assert!(
        second.is_ok(),
        "the captured violation must not panic twice"
    );
}

#[test]
fn guard_end_rearms_after_a_successful_early_assert() {
    let caught = catch_unwind(AssertUnwindSafe(|| {
        let host = FakeHost::new();
        host.preset("systemd-run", &[], FakeOutput::ok());
        host.assert_red_lines();
        host.run(&collect_command());
        println!(
            "GUARD_END prior_success=true commands={:?}",
            host.commands()
        );
    }));
    println!("GUARD_END rearmed end_detected={}", caught.is_err());
    assert!(
        caught.is_err(),
        "a successful early assert cannot replace the test-end assertion of commands recorded later"
    );
}

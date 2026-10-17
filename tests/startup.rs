//! The startup checks.

mod common;

use common::*;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::startup::{self, MappingReason, StartupProblem};
use truenas_nvidia_driver_manager::store::Paths;

fn paths() -> Paths {
    Paths::new(CONTAINER_DATA_DIR)
}

#[test]
fn an_uppercase_image_id_is_rejected() {
    // The image id is `^sha256:[0-9a-f]{64}$`; an uppercase spelling is an unreadable inspect
    // answer, not a valid id.
    let host = default_host();
    host.set_docker_inspect(
        &inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")])
            .replace(&"a".repeat(64), &"A".repeat(64)),
    );
    let s = startup::check(&host, &paths());
    assert!(s.image_id.is_none(), "{s:?}");
    assert!(s.problems.iter().any(|p| matches!(
        p,
        StartupProblem::DataMapping {
            reason: MappingReason::InspectFailed { reason, .. }
        } if reason.key == REASON_FORMAT_UNRECOGNIZED
    )));
}

#[test]
fn the_container_mount_root_must_end_with_hostname() {
    // The root field matches `…/containers/<id>/hostname`; any other suffix is not the hostname
    // bind mount.
    let id = "a".repeat(64);
    let wrong = format!(
        "25 1 0:30 /var/lib/docker/containers/{id}/not-hostname /etc/hostname rw,relatime - ext4 /dev/sda1 rw\n"
    );
    assert_eq!(startup::container_id_from_mountinfo(&wrong), None);
    let good = format!(
        "25 1 0:30 /var/lib/docker/containers/{id}/hostname /etc/hostname rw,relatime - ext4 /dev/sda1 rw\n"
    );
    assert_eq!(
        startup::container_id_from_mountinfo(&good).as_deref(),
        Some(id.as_str())
    );
}

#[test]
fn healthy_host_passes_all_checks() {
    let host = default_host();
    host.set_docker_inspect(&inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]));
    let s = startup::check(&host, &paths());
    assert!(s.problems.is_empty(), "{:?}", s.problems);
    assert!(s.pid_host_ok);
    assert_eq!(
        s.host_data_dir.as_deref(),
        Some(std::path::Path::new(HOST_DATA_DIR))
    );
    assert_eq!(s.image_id, Some(format!("sha256:{}", "a".repeat(64))));
}

#[test]
fn missing_pid_host_skips_the_other_checks() {
    let host = default_host();
    host.set_same_namespace(true);
    let s = startup::check(&host, &paths());
    assert_eq!(s.problems, vec![StartupProblem::PidHost]);
    assert!(!s.pid_host_ok);
}

#[test]
fn wrong_runtime_is_reported() {
    let host = default_host();
    host.set_docker_inspect(&inspect_json(
        "io.containerd.runc.v2",
        &[(HOST_DATA_DIR, "/var/lib/nvm")],
    ));
    let s = startup::check(&host, &paths());
    assert!(matches!(
        s.problems.first(),
        Some(StartupProblem::Runtime { value: Some(v) }) if v == "io.containerd.runc.v2"
    ));
}

#[test]
fn mount_count_must_be_one() {
    let host = default_host();
    host.set_docker_inspect(&inspect_json("runc", &[]));
    let s = startup::check(&host, &paths());
    assert!(matches!(
        s.problems.first(),
        Some(StartupProblem::DataMapping {
            reason: MappingReason::MountCount { count: 0 }
        })
    ));
    let host = default_host();
    host.set_docker_inspect(&inspect_json(
        "runc",
        &[
            (HOST_DATA_DIR, "/var/lib/nvm"),
            ("/mnt/<pool>/other", "/var/lib/nvm"),
        ],
    ));
    let s = startup::check(&host, &paths());
    assert!(matches!(
        s.problems.first(),
        Some(StartupProblem::DataMapping {
            reason: MappingReason::MountCount { count: 2 }
        })
    ));
}

#[test]
fn mounts_under_the_data_directory_are_reported() {
    let host = default_host();
    host.set_docker_inspect(&inspect_json(
        "runc",
        &[
            (HOST_DATA_DIR, "/var/lib/nvm"),
            ("/mnt/<pool>/extra", "/var/lib/nvm/extra"),
        ],
    ));
    let s = startup::check(&host, &paths());
    assert!(matches!(
        s.problems.first(),
        Some(StartupProblem::DataMapping {
            reason: MappingReason::MountUnder { mount_point }
        }) if mount_point == "/var/lib/nvm/extra"
    ));
}

#[test]
fn data_directory_path_characters_are_rejected() {
    let host = default_host();
    host.set_docker_inspect(&inspect_json(
        "runc",
        &[(&format!("/mnt/<pool>{}data", ','), "/var/lib/nvm")],
    ));
    let s = startup::check(&host, &paths());
    assert!(matches!(
        s.problems.first(),
        Some(StartupProblem::DataMapping {
            reason: MappingReason::PathCharacters
        })
    ));
}

#[test]
fn nsenter_failure_is_reported() {
    let host = default_host();
    host.preset(
        "docker",
        &["inspect"],
        FakeOutput::code(1)
            .with_stderr("nsenter: failed to execute docker: No such file or directory"),
    );
    let s = startup::check(&host, &paths());
    assert!(matches!(
        s.problems.first(),
        Some(StartupProblem::DataMapping {
            reason: MappingReason::NsenterFailed
        })
    ));
}

#[test]
fn unrecognized_inspect_output_drops_the_environment() {
    let host = default_host();
    let secret = "NVM_INITIAL_ADMIN_PASSWORD=hunter12-secret";
    host.set_docker_inspect(&format!(
        r#"[{{"Config":{{"Env":["{secret}"]}},"Image":"sha256:{}"#,
        "a".repeat(8)
    ));
    let s = startup::check(&host, &paths());
    let problem = s
        .problems
        .first()
        .expect("inspect output was not a problem");
    assert!(problem.raw().is_none(), "{problem:?}");
    let rendered = format!("{s:?}");
    assert!(!rendered.contains("hunter12-secret"), "{rendered}");
    assert!(
        !rendered.contains("NVM_INITIAL_ADMIN_PASSWORD"),
        "{rendered}"
    );

    // A valid inspect array may still carry Config.Env. Unknown fields are ignored, and that text
    // is not stored on the problem.
    host.set_docker_inspect(
        &inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]).replacen(
            '{',
            &format!(r#"{{"Config":{{"Env":["{secret}"]}},"#),
            1,
        ),
    );
    let parsed = startup::check(&host, &paths());
    assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
    assert!(!format!("{parsed:?}").contains("hunter12-secret"));
}

#[test]
fn inspect_failure_carries_the_raw_output() {
    let host = default_host();
    host.preset(
        "docker",
        &["inspect"],
        FakeOutput::code(1).with_stderr("Error: No such object: abc"),
    );
    let s = startup::check(&host, &paths());
    match s.problems.first() {
        Some(StartupProblem::DataMapping {
            reason: MappingReason::InspectFailed { raw, .. },
        }) => assert!(raw.as_deref().unwrap().contains("No such object")),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn no_hostname_line_is_reported() {
    let host = default_host();
    host.set_hostname_container_id("zzz");
    let s = startup::check(&host, &paths());
    assert!(matches!(
        s.problems.first(),
        Some(StartupProblem::DataMapping {
            reason: MappingReason::NoHostnameLine
        })
    ));
}

#[test]
fn readback_mismatch_is_reported() {
    let host = default_host();
    host.set_docker_inspect(&inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]));
    host.preset("cat", &[], FakeOutput::stdout("something else"));
    let s = startup::check(&host, &paths());
    assert!(matches!(
        s.problems.first(),
        Some(StartupProblem::DataMapping {
            reason: MappingReason::CheckMismatch
        })
    ));
}

#[test]
fn readback_failure_is_reported() {
    let host = default_host();
    host.set_docker_inspect(&inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]));
    host.preset(
        "cat",
        &[],
        FakeOutput::code(1).with_stderr("cat: No such file or directory"),
    );
    let s = startup::check(&host, &paths());
    assert!(matches!(
        s.problems.first(),
        Some(StartupProblem::DataMapping {
            reason: MappingReason::CheckReadFailed { .. }
        })
    ));
}

#[test]
fn signaled_startup_commands_report_the_signal_in_login_status_and_logs() {
    use truenas_nvidia_driver_manager::host::Exit;

    for (program, args, mapping_key) in [
        ("docker", &["inspect"][..], "startup.mapping.inspect_failed"),
        ("cat", &[][..], "startup.mapping.check_read_failed"),
    ] {
        let host = default_host();
        host.set_docker_inspect(&inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]));
        host.preset(
            program,
            args,
            FakeOutput {
                exit: Exit::Signal(15),
                stdout: Vec::new(),
                stderr: b"command: killed".to_vec(),
                delay: std::time::Duration::ZERO,
            },
        );
        let startup = startup::check(&host, &paths());
        let problem = startup.problems.first().expect("the signaled check fails");
        assert_eq!(
            problem.to_json(),
            serde_json::json!({
                "key": "startup.data_mapping",
                "params": {"mapping_reason": {
                    "key": mapping_key,
                    "params": {"read_reason": {
                        "key": "reason.signal",
                        "params": {"signal": 15},
                    }},
                }},
            })
        );
        assert_eq!(problem.log_reason_json(), problem.to_json());
        assert_eq!(problem.raw(), Some("command: killed"));
    }
}

#[test]
fn unknown_image_id_shape_fails_the_inspect_check() {
    let host = default_host();
    host.set_docker_inspect(
        &inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")])
            .replace(&format!("sha256:{}", "a".repeat(64)), "latest"),
    );
    let s = startup::check(&host, &paths());
    match s.problems.first() {
        Some(StartupProblem::DataMapping {
            reason: MappingReason::InspectFailed { reason, .. },
        }) => assert_eq!(reason.key, REASON_FORMAT_UNRECOGNIZED),
        other => panic!("unexpected {other:?}"),
    }
}

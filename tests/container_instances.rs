//! Select the instance API from actual host capability, independently of Docker.
mod common;
use common::{default_host, FakeOutput};
use std::time::Duration;
use truenas_nvidia_driver_manager::gpu_users::list;

const MISSING: &str = "[ENOMETHOD] Method does not exist";

#[test]
fn container_query_reads_embedded_nvidia_devices_and_running_state() {
    let host = default_host();
    host.add_drm_node("renderD128", "nvidia");
    host.add_drm_node("renderD129", "i915");
    host.set_midclt("call container.query", FakeOutput::stdout(r#"[
      {"id":1,"name":"cuda","status":{"state":"RUNNING"},"devices":[{"attributes":{"dtype":"GPU","gpu_type":"NVIDIA","pci_address":"0000:01:00.0"}}]},
      {"id":2,"name":"intel","status":{"state":"RUNNING"},"devices":[{"attributes":{"dtype":"GPU","gpu_type":"INTEL","pci_address":"0000:02:00.0"}}]},
      {"id":3,"name":"stopped","status":{"state":"STOPPED"},"devices":[{"attributes":{"dtype":"GPU","gpu_type":"NVIDIA","pci_address":"0000:01:00.0"}}]},
      {"id":4,"name":"dri","status":{"state":"RUNNING"},"devices":[{"attributes":{"dtype":"FILESYSTEM","source":"/dev/dri/renderD128","target":"/dev/dri/renderD128"}}]},
      {"id":5,"name":"other-dri","status":{"state":"RUNNING"},"devices":[{"attributes":{"dtype":"FILESYSTEM","source":"/dev/dri/renderD129","target":"/dev/dri/renderD128"}}]}
    ]"#));
    host.set_midclt("call virt.instance.query", FakeOutput::code(1));
    let result = list(&host, Duration::from_secs(2));
    assert!(result.errors.is_empty(), "{result:?}");
    assert_eq!(
        result
            .users
            .iter()
            .map(|u| u.name.as_str())
            .collect::<Vec<_>>(),
        ["cuda", "dri"]
    );
}

#[test]
fn available_empty_container_query_does_not_fall_back() {
    let host = default_host();
    host.set_midclt("call container.query", FakeOutput::stdout("[]"));
    host.set_midclt("call virt.instance.query", FakeOutput::code(1));
    let result = list(&host, Duration::from_secs(2));
    assert!(result.users.is_empty());
    assert!(result.errors.is_empty());
}

#[test]
fn only_virt_interface_keeps_existing_container_gpu_behavior() {
    let host = default_host();
    host.set_midclt(
        "call container.query",
        FakeOutput::code(1).with_stderr(MISSING),
    );
    host.set_midclt(
        "call virt.instance.query",
        FakeOutput::stdout(
            r#"[{"id":"cuda","name":"cuda","type":"CONTAINER","status":"RUNNING"}]"#,
        ),
    );
    host.set_midclt(
        "call virt.instance.device_list cuda",
        FakeOutput::stdout(r#"[{"dev_type":"GPU"}]"#),
    );
    let result = list(&host, Duration::from_secs(2));
    assert!(result.errors.is_empty());
    assert_eq!(result.users[0].name, "cuda");
    assert_eq!(
        serde_json::to_value(&result.users[0]).unwrap()["usage"],
        "unknown"
    );
}

#[test]
fn neither_instance_interface_means_no_instances() {
    let host = default_host();
    host.set_midclt(
        "call container.query",
        FakeOutput::code(1).with_stderr(MISSING),
    );
    host.set_midclt(
        "call virt.instance.query",
        FakeOutput::code(1).with_stderr(MISSING),
    );
    let result = list(&host, Duration::from_secs(2));
    assert!(result.users.is_empty());
    assert!(result.errors.is_empty(), "{result:?}");
}

#[test]
fn container_errors_and_malformed_responses_do_not_fall_back() {
    for out in [
        FakeOutput::code(1).with_stderr("connection refused"),
        FakeOutput::stdout("{}"),
        FakeOutput::stdout(r#"[{"id":1,"name":"broken","status":"RUNNING","devices":[]}]"#),
    ] {
        let host = default_host();
        host.set_midclt("call container.query", out);
        let result = list(&host, Duration::from_secs(2));
        assert_eq!(result.errors.len(), 1, "{result:?}");
    }
}

#[test]
fn exact_missing_service_and_method_messages_select_virt() {
    for message in [
        "[ENOMETHOD] Service 'container' not found",
        "[ENOMETHOD] Method 'query' not found in 'container'",
    ] {
        let host = default_host();
        host.set_midclt(
            "call container.query",
            FakeOutput::code(1).with_stderr(message),
        );
        host.set_midclt("call virt.instance.query", FakeOutput::stdout("[]"));
        assert!(list(&host, Duration::from_secs(2)).errors.is_empty());
    }
}

#[test]
fn timeout_signal_and_permission_errors_are_reported_without_fallback() {
    use truenas_nvidia_driver_manager::host::Exit;
    for (exit, message, reason) in [
        (Exit::TimedOut, MISSING, "reason.timed_out"),
        (Exit::Signal(9), MISSING, "reason.signal"),
        (Exit::SpawnFailed, MISSING, "reason.spawn_failed"),
        (
            Exit::Code(1),
            "[EACCES] permission denied",
            "reason.exit_status",
        ),
        (
            Exit::Code(1),
            "some unrelated file not found",
            "reason.exit_status",
        ),
    ] {
        let host = default_host();
        let mut output = FakeOutput::code(1).with_stderr(message);
        output.exit = exit;
        host.set_midclt("call container.query", output);
        let result = list(&host, Duration::from_secs(2));
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].reason.key, reason);
    }
}

#[test]
fn supported_kernel_scenes_keep_compiler_and_instance_api_in_sync_across_resets() {
    use truenas_nvidia_driver_manager::facts;
    let host = default_host();
    for (kernel, truenas, compiler, major, fallback) in [
        (
            "6.18.52-production+truenas",
            "27.0.0-RC.1",
            "gcc (Debian 14.2.0-19) 14.2.0",
            14,
            false,
        ),
        (
            common::KERNEL,
            common::TRUENAS_VERSION,
            "gcc (Debian 12.2.0-14+deb12u1) 12.2.0",
            12,
            true,
        ),
        (
            "6.18.52-production+truenas",
            "27.0.0-RC.1",
            "gcc (Debian 14.2.0-19) 14.2.0",
            14,
            false,
        ),
    ] {
        host.clear_presets_and_commands();
        common::fake_host::prime_kernel_scene(&host, kernel, truenas, compiler);
        assert_eq!(facts::kernel_gcc_major(&host, kernel), Some(major));
        assert_eq!(
            facts::kernel(&host).value().map(String::as_str),
            Some(kernel)
        );
        let users = list(&host, Duration::from_secs(2));
        assert!(users.errors.is_empty(), "{users:?}");
        let commands = host.commands();
        let called = |method: &str| {
            commands
                .iter()
                .any(|c| c.program == "midclt" && c.args == ["call", method])
        };
        assert!(called("container.query"));
        assert_eq!(called("virt.instance.query"), fallback);
    }
}

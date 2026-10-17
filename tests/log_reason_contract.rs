//! (`docker logs`) field and reason contracts: the failure events of the real startup and request
//! paths carry the `{key, params}` reason object (plus the external raw of the same object), the
//! setting_invalid line carries its mandated `rule` field, and the one string-reason exception
//! (`upload_rejected`) is valid. Every scenario runs the real `web::serve_with` or the real router
//! with the fake host; the process-global log sink is serialized by `LOCK` because each test binary
//! starts its tests in parallel threads.

mod common;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};
use truenas_nvidia_driver_manager::codes::CATALOG_URL;
use truenas_nvidia_driver_manager::settings::{
    self, Settings, VAR_BUILD_TIMEOUT, VAR_PASSWORD, VAR_PORT, VAR_USERNAME,
};
use truenas_nvidia_driver_manager::{log, web};

fn sink_lines(sink: &SharedBuf) -> Vec<Value> {
    let raw = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(raw.is_ascii(), "the log lines are ASCII: {raw}");
    raw.lines()
        .map(|line| serde_json::from_str(line).expect("a log line is JSON"))
        .collect()
}

fn event<'a>(lines: &'a [Value], name: &str) -> &'a Value {
    lines
        .iter()
        .find(|line| line["event"] == name)
        .unwrap_or_else(|| panic!("no {name} line in {lines:?}"))
}

/// The tests share one process-global sink; run one at a time. A failure in one test must not
/// poison the lock for the others. This is an async mutex because the guard is held across the
/// serve awaits.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn lock() -> tokio::sync::MutexGuard<'static, ()> {
    LOCK.lock().await
}

/// A complete log capture of one real serve call. The port is 0 unless a test binds a specific one;
/// the listener never conflicts with a later scenario.
async fn serve_lines(host: FakeHost, settings: Settings) -> Vec<Value> {
    let root = std::env::temp_dir().join(format!(
        "nvm-log-reason-{}",
        truenas_nvidia_driver_manager::store::random_hex(8)
    ));
    std::fs::create_dir(&root).unwrap();
    let sink = SharedBuf::default();
    log::set_sink(Box::new(sink.clone()));
    let task = tokio::spawn(web::serve_with(
        settings,
        Arc::new(host.clone()),
        root.clone(),
    ));
    for _ in 0..600 {
        let text = String::from_utf8_lossy(&sink.0.lock().unwrap()).into_owned();
        if text.contains("\"event\":\"listening\"") || task.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let lines = sink_lines(&sink);
    task.abort();
    let _ = task.await;
    log::set_sink(Box::new(std::io::sink()));
    let _ = std::fs::remove_dir_all(&root);
    host.assert_red_lines();
    lines
}

fn healthy() -> FakeHost {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    host.set_docker_inspect(&inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]));
    host
}

/// The three kernel-header readings and the version index versions needs.
fn catalog_ready(host: &FakeHost) {
    let base = "/usr/src/linux-headers-truenas-production-amd64/include";
    host.put_text(
        &format!("{base}/linux/mm.h"),
        "int follow_pfnmap_start(void);\n",
    );
    host.put_text(
        &format!("{base}/drm/drm_mode_config.h"),
        "struct drm_mode_config_funcs { void (*atomic_state_free)(void); };\n",
    );
    host.put_text(
        &format!("{base}/linux/pci.h"),
        "int pci_resize_resource(struct pci_dev *dev, int resno, int size, u32 exclude_bars);\n",
    );
    host.set_curl(
        CATALOG_URL,
        200,
        b"<a href=\"580.178.04/\">580.178.04</a>\n",
    );
}

fn assert_reason_object(line: &Value, context: &str) {
    let reason = &line["reason"];
    assert!(
        reason.is_object() && reason["key"].is_string() && reason["params"].is_object(),
        "{context}: the reason must be a {{key, params}} object: {line}"
    );
}

#[tokio::test]
async fn non_utf8_setting_invalid_carries_the_mandated_rule_field() {
    let _guard = lock().await;
    for var in [VAR_USERNAME, VAR_PASSWORD, VAR_PORT, VAR_BUILD_TIMEOUT] {
        let mut vars = vec![(OsString::from(VAR_PASSWORD), OsString::from("hunter12"))];
        vars.retain(|(key, _)| key != var);
        vars.push((OsString::from(var), OsString::from_vec(vec![0xff, 0xfe])));
        let mut set = settings::load(vars.into_iter());
        set.port = 0;
        let lines = serve_lines(healthy(), set).await;
        let line = event(&lines, "setting_invalid");
        assert_eq!(line["variable"], var, "{line}");
        // Invalid settings carry a `rule` field; non-UTF-8 values use `setting.invalid_utf8`.
        assert_eq!(
            line["rule"], "setting.invalid_utf8",
            "the setting reason requires the rule field for {var}: {line}"
        );
        // The entered value itself is never part of the line.
        assert!(line.get("value").is_none(), "{line}");
    }
    // A legal UTF-8 value with an unusable port keeps its real rule (control).
    let mut set = settings::load(
        vec![
            (OsString::from(VAR_PORT), OsString::from("abc")),
            (OsString::from(VAR_PASSWORD), OsString::from("hunter12")),
        ]
        .into_iter(),
    );
    set.port = 0;
    let lines = serve_lines(healthy(), set).await;
    let line = event(&lines, "setting_invalid");
    assert_eq!(line["rule"], "setting.rule.port", "{line}");
    assert!(
        event(&lines, "port_fallback")["port"].is_number(),
        "the invalid port still falls back"
    );
}

#[tokio::test]
async fn startup_check_failed_reasons_are_objects() {
    let _guard = lock().await;
    for case in ["pid_host", "runtime", "data_mapping"] {
        let host = healthy();
        match case {
            "pid_host" => host.set_same_namespace(true),
            "runtime" => {
                host.set_docker_inspect(&inspect_json("nvidia", &[(HOST_DATA_DIR, "/var/lib/nvm")]))
            }
            "data_mapping" => host.set_docker_inspect(&inspect_json("runc", &[])),
            _ => unreachable!(),
        }
        let mut set = settings("hunter12");
        set.port = 0;
        let lines = serve_lines(host, set).await;
        let matching: Vec<&Value> = lines
            .iter()
            .filter(|line| line["event"] == "startup_check_failed" && line["check"] == case)
            .collect();
        assert!(!matching.is_empty(), "{case}: the fault must be reached");
        for line in matching {
            assert_reason_object(line, case);
        }
    }
}

#[tokio::test]
async fn startup_command_signal_log_keeps_the_real_signal() {
    use truenas_nvidia_driver_manager::host::Exit;

    let _guard = lock().await;
    for (program, args, mapping_key) in [
        ("docker", &["inspect"][..], "startup.mapping.inspect_failed"),
        ("cat", &[][..], "startup.mapping.check_read_failed"),
    ] {
        let host = healthy();
        host.preset(
            program,
            args,
            FakeOutput {
                exit: Exit::Signal(15),
                stdout: Vec::new(),
                stderr: b"command: killed".to_vec(),
                delay: Duration::ZERO,
            },
        );
        let mut set = settings("hunter12");
        set.port = 0;
        let lines = serve_lines(host, set).await;
        let line = event(&lines, "startup_check_failed");
        let mapping = &line["reason"]["params"]["mapping_reason"];
        assert_eq!(line["reason"]["key"], "startup.data_mapping", "{line}");
        assert_eq!(mapping["key"], mapping_key, "{line}");
        assert_eq!(
            mapping["params"]["read_reason"]["key"], "reason.signal",
            "{line}"
        );
        assert_eq!(
            mapping["params"]["read_reason"]["params"],
            serde_json::json!({"signal": 15}),
            "{line}"
        );
        assert_eq!(line["reason"]["raw"], "command: killed", "{line}");
    }
}

#[tokio::test]
async fn unit_query_signal_log_uses_the_same_general_reason() {
    use truenas_nvidia_driver_manager::host::Exit;

    let _guard = lock().await;
    let host = healthy();
    host.preset(
        "systemctl",
        &["list-units"],
        FakeOutput {
            exit: Exit::Signal(15),
            stdout: Vec::new(),
            stderr: b"systemctl: killed".to_vec(),
            delay: Duration::ZERO,
        },
    );
    let mut set = settings("hunter12");
    set.port = 0;
    let lines = serve_lines(host, set).await;
    let line = event(&lines, "unit_query_failed");
    assert_eq!(
        line["reason"],
        serde_json::json!({"key": "reason.signal", "params": {"signal": 15}})
    );
}

#[tokio::test]
async fn credential_and_data_directory_failure_reasons_are_objects() {
    let _guard = lock().await;

    let host = healthy();
    host.put_text(
        "/var/lib/nvm/manager/secrets/web-credentials.json",
        "not-json",
    );
    let mut set = settings("hunter12");
    set.port = 0;
    let lines = serve_lines(host, set).await;
    let line = event(&lines, "credentials_unreadable");
    assert_reason_object(line, "credentials_unreadable");
    assert_eq!(
        line["reason"]["key"], "credentials.reason.not_json",
        "{line}"
    );

    let host = healthy();
    host.fail_write("/var/lib/nvm/manager/secrets/");
    let mut set = settings("hunter12");
    set.port = 0;
    let lines = serve_lines(host, set).await;
    let line = event(&lines, "credentials_unsaved");
    assert_reason_object(line, "credentials_unsaved");
    assert_eq!(
        line["reason"]["key"], "credentials.unsaved_reason.directory",
        "{line}"
    );

    let host = healthy();
    host.remove_tree("/var/lib/nvm");
    host.put_text("/var/lib/nvm", "own sandbox blocking file");
    let mut set = settings("hunter12");
    set.port = 0;
    let lines = serve_lines(host, set).await;
    let line = event(&lines, "data_dirs_failed");
    assert_reason_object(line, "data_dirs_failed");
    assert_eq!(line["path"], "/var/lib/nvm");
}

#[tokio::test]
async fn catalog_failure_reasons_are_objects_and_the_healthy_path_stays_quiet() {
    use truenas_nvidia_driver_manager::host::Exit;

    let _guard = lock().await;
    for case in [
        "curl-exit",
        "curl-signal",
        "http-status",
        "index-format",
        "kernel_headers",
        "gpus",
        "catalog-control",
    ] {
        let host = healthy();
        catalog_ready(&host);
        match case {
            "curl-exit" => host.set_curl_failure(CATALOG_URL, 7, "independent curl error"),
            "curl-signal" => host.preset(
                "curl",
                &[],
                FakeOutput {
                    exit: Exit::Signal(15),
                    stdout: Vec::new(),
                    stderr: b"curl: killed".to_vec(),
                    delay: Duration::ZERO,
                },
            ),
            "http-status" => host.set_curl(CATALOG_URL, 404, b""),
            "index-format" => host.set_curl(CATALOG_URL, 200, b"not an index"),
            "kernel_headers" => {
                host.fail_read("/usr/src/linux-headers-truenas-production-amd64/include/linux/mm.h")
            }
            "gpus" => {
                let base = "/sys/bus/pci/devices/0000:01:00.0";
                host.put_text(&format!("{base}/vendor"), "0x10de\n");
                host.put_text(&format!("{base}/class"), "0x030000\n");
                host.put_text(&format!("{base}/device"), "0x2684\n");
                host.fail_read(&format!("{base}/vendor"));
            }
            _ => {}
        }
        let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
        let cookie = login_changed(&app, "hunter12", "changed123").await;
        let sink = SharedBuf::default();
        log::set_sink(Box::new(sink.clone()));
        let (_status, body) = get(&app, "/api/versions", Some(&cookie)).await;
        let lines = sink_lines(&sink);
        log::set_sink(Box::new(std::io::sink()));
        host.assert_red_lines();

        let log_event = match case {
            "kernel_headers" => "kernel_headers_unreadable",
            "gpus" => "gpus_unreadable",
            _ => "catalog_query_failed",
        };
        let matching: Vec<&Value> = lines
            .iter()
            .filter(|line| line["event"] == log_event)
            .collect();
        if case == "catalog-control" {
            assert!(
                matching.is_empty(),
                "a healthy catalog writes no failure event: {lines:?}"
            );
            assert_eq!(body["unreadable"].as_array().unwrap().len(), 0, "{body}");
        } else {
            assert!(!matching.is_empty(), "{case}: the fault must be reached");
            for line in matching {
                assert_reason_object(line, case);
            }
            // The curl failure carries the external stderr as its own raw and the key of the real
            // curl classification.
            if case == "curl-exit" {
                let line = event(&lines, "catalog_query_failed");
                assert_eq!(line["reason"]["key"], "reason.connect_failed", "{line}");
                assert_eq!(line["reason"]["raw"], "independent curl error", "{line}");
            }
            if case == "curl-signal" {
                let line = event(&lines, "catalog_query_failed");
                assert_eq!(line["reason"]["key"], "reason.signal", "{line}");
                assert_eq!(
                    line["reason"]["params"],
                    serde_json::json!({"signal": 15}),
                    "{line}"
                );
                assert_eq!(line["reason"]["raw"], "curl: killed", "{line}");
                assert_eq!(body["unreadable"][0]["reason"]["key"], "reason.signal");
                assert_eq!(body["unreadable"][0]["reason"]["params"]["signal"], 15);
            }
        }
    }
}

#[tokio::test]
async fn listen_failed_reason_is_an_object() {
    let _guard = lock().await;
    let held = tokio::net::TcpListener::bind(("0.0.0.0", 0)).await.unwrap();
    let port = held.local_addr().unwrap().port();
    let mut set = settings("hunter12");
    set.port = port;
    let lines = serve_lines(healthy(), set).await;
    let line = event(&lines, "listen_failed");
    assert_reason_object(line, "listen_failed");
    assert_eq!(line["address"], format!("0.0.0.0:{port}"), "{line}");
    drop(held);
}

/// Every caller of `tasks::cleanup` passes the command's real `Exit`, so the logged reason keeps
/// that classification: the timeout carries its seconds, a spawn failure its own key, a signal uses
/// `reason.signal`, and only an exit code uses `reason.exit_status`. The stderr stays the raw of
/// the same object and numeric fields are never null.
#[test]
fn cleanup_command_failures_keep_their_exit_classification() {
    use truenas_nvidia_driver_manager::codes::{
        CleanupStep, HOST_CMD_TIMEOUT, REASON_EXIT_STATUS, REASON_SIGNAL, REASON_SPAWN_FAILED,
        REASON_TIMED_OUT, RW_MOUNT, RW_TMP_PATH,
    };
    use truenas_nvidia_driver_manager::host::Exit;
    use truenas_nvidia_driver_manager::task_state::Rootfs;

    struct Case {
        label: &'static str,
        step: CleanupStep,
        program: &'static str,
        args: Vec<&'static str>,
        exit: Exit,
        stderr: &'static str,
        wrapped_mount: bool,
        rootfs: Option<Rootfs>,
        key: &'static str,
        params: Value,
    }

    fn failed(exit: Exit, stderr: &str) -> FakeOutput {
        FakeOutput {
            exit,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
            delay: std::time::Duration::ZERO,
        }
    }

    let id = "20260928T101530Z-3f9a0c12";
    let dataset = "tank/ROOT/env/usr";
    let rootfs = || Rootfs {
        dataset: dataset.to_string(),
        readonly_before: "on".to_string(),
    };

    let cases = vec![
        Case {
            label: "timeout",
            step: CleanupStep::UnmountWritableMount,
            program: "umount",
            args: vec![RW_MOUNT],
            exit: Exit::TimedOut,
            stderr: "umount: timed out",
            wrapped_mount: true,
            rootfs: None,
            key: REASON_TIMED_OUT,
            params: serde_json::json!({"seconds": HOST_CMD_TIMEOUT.as_secs()}),
        },
        Case {
            label: "spawn-failed",
            step: CleanupStep::RestoreReadonly,
            program: "zfs",
            args: vec!["get", "-H", "-o", "value", "readonly", dataset],
            exit: Exit::SpawnFailed,
            stderr: "zfs: could not be started",
            wrapped_mount: false,
            rootfs: Some(rootfs()),
            key: REASON_SPAWN_FAILED,
            params: serde_json::json!({}),
        },
        Case {
            label: "signal",
            step: CleanupStep::RemoveBuildContainer,
            program: "docker",
            args: vec!["rm", "-f", "nvm-build-20260928T101530Z-3f9a0c12"],
            exit: Exit::Signal(15),
            stderr: "docker: killed",
            wrapped_mount: false,
            rootfs: None,
            key: REASON_SIGNAL,
            params: serde_json::json!({"signal": 15}),
        },
        Case {
            label: "exit-code",
            step: CleanupStep::UnmountWritableMount,
            program: "umount",
            args: vec![RW_MOUNT],
            exit: Exit::Code(1),
            stderr: "umount: target is busy",
            wrapped_mount: true,
            rootfs: None,
            key: REASON_EXIT_STATUS,
            params: serde_json::json!({"code": 1}),
        },
    ];

    for case in &cases {
        let host = default_host();
        let paths = truenas_nvidia_driver_manager::store::Paths::new(HOST_DATA_DIR);
        if case.wrapped_mount {
            host.add_mount(DATASET, RW_MOUNT, "zfs", "rw");
            host.put_text(RW_TMP_PATH, "half-written");
        }
        if case.program == "docker" {
            // The cleanup only issues `docker rm` for a container the exact listing really reports.
            host.add_container(&format!("nvm-build-{id}"), false);
        }
        host.preset(case.program, &case.args, failed(case.exit, case.stderr));

        let results =
            truenas_nvidia_driver_manager::tasks::cleanup(&host, &paths, id, case.rootfs.as_ref());
        let result = results
            .iter()
            .find(|r| r.step == case.step)
            .unwrap_or_else(|| panic!("{}: the step ran", case.label));
        assert!(!result.ok, "{}: the step must fail: {result:?}", case.label);
        assert_eq!(
            result.reason.as_deref(),
            Some(case.stderr),
            "{}: the raw command text stays the reason text",
            case.label
        );
        let reason = result
            .log_reason
            .clone()
            .unwrap_or_else(|| panic!("{}: a failed command step writes a log reason", case.label));
        assert_eq!(reason["key"], case.key, "{}: {reason}", case.label);
        assert_eq!(reason["params"], case.params, "{}: {reason}", case.label);
        assert_eq!(reason["raw"], case.stderr, "{}: {reason}", case.label);
        assert_ne!(
            reason["params"].get("code"),
            Some(&Value::Null),
            "{}: code is never null: {reason}",
            case.label
        );
    }
}

/// The reconcile cleanup step failure (the `docker logs` line written by `tasks.rs`, not by the web
/// process) carries the same object.
#[tokio::test]
async fn cleanup_step_failure_reason_is_an_object() {
    let _guard = lock().await;
    let host = default_host();
    let paths = truenas_nvidia_driver_manager::store::Paths::new(HOST_DATA_DIR);
    let id = "20260928T101530Z-3f9a0c12";
    // The cleanup only runs for an id whose driver responsibility is settled: a normally closed
    // record with its own passing update check and the positive readback of the image it wrote.
    {
        use truenas_nvidia_driver_manager::task_state::{
            new_state, CheckRecord, OriginalChoice, Pass, Phase, PlanSource, ReadingJson,
            ReloadedRecord, ReplaceRecord, Status,
        };
        let version = "580.178.04";
        let mut state = new_state(
            id,
            1,
            sample_plan(PlanSource::Official),
            Phase::ReadLoadedVersion,
        );
        state.plan.as_mut().unwrap().version = version.to_string();
        state.plan.as_mut().unwrap().original = OriginalChoice::Skip;
        state.status = Status::Succeeded;
        state.phase = Some(Phase::ReadLoadedVersionEnd);
        state.replace.push(ReplaceRecord {
            pass: Pass::Update,
            unloaded: vec!["nvidia".to_string()],
            reloaded: vec![ReloadedRecord {
                module: "nvidia".to_string(),
                ok: true,
                reason: None,
            }],
            refresh_exit: Some(serde_json::json!({"code": 0})),
            ldconfig_exit: Some(serde_json::json!({"code": 0})),
            readback_ok: Some(true),
            readonly_after: None,
        });
        state.checks.push(CheckRecord {
            pass: Pass::Update,
            since_before: ReadingJson::value(1),
            since_after: ReadingJson::value(2),
            refresh_ok: true,
            nvidia_smi: ReadingJson::value(version.to_string()),
            proc_version: ReadingJson::value(version.to_string()),
            expected: version.to_string(),
            version_ok: true,
            passed: true,
        });
        truenas_nvidia_driver_manager::task_state::write(
            &host,
            &truenas_nvidia_driver_manager::store::Paths::new(HOST_DATA_DIR),
            &state,
        )
        .unwrap();
    }
    host.put_text(&format!("{HOST_DATA_DIR}/manager/tasks/{id}/keep.txt"), "x");
    let temp = format!("/var/lib/nvm/manager/state/.{id}.json.tmp-aaaaaaaaaaaaaaaa");
    host.put_text(&temp, "half-written");
    host.fail_remove(&temp);
    let container = format!("nvm-build-{id}");
    // The exact listing is the judgement, so the removal is only issued for a container the model
    // really reports.
    host.add_container(&container, false);
    host.preset(
        "docker",
        &["rm", "-f", &container],
        FakeOutput {
            exit: truenas_nvidia_driver_manager::host::Exit::Signal(15),
            stdout: Vec::new(),
            stderr: b"docker: killed".to_vec(),
            delay: Duration::ZERO,
        },
    );

    let sink = SharedBuf::default();
    log::set_sink(Box::new(sink.clone()));
    let _ = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths);
    log::set_sink(Box::new(std::io::sink()));
    let lines = sink_lines(&sink);

    let line = lines
        .iter()
        .find(|line| {
            line["event"] == "task_cleanup_step_failed" && line["step"] == "delete_state_temps"
        })
        .expect("the failed state temp cleanup is logged");
    assert_eq!(line["task_id"], id, "{line}");
    assert_eq!(line["step"], "delete_state_temps", "{line}");
    assert_reason_object(line, "task_cleanup_step_failed");
    assert_eq!(line["reason"]["key"], "reason.permission_denied", "{line}");
    let container_line = lines
        .iter()
        .find(|line| {
            line["event"] == "task_cleanup_step_failed" && line["step"] == "remove_build_container"
        })
        .expect("the failed container cleanup is logged");
    assert_eq!(container_line["reason"]["key"], "reason.signal");
    assert_eq!(
        container_line["reason"]["params"],
        serde_json::json!({"signal": 15})
    );
    assert_eq!(container_line["reason"]["raw"], "docker: killed");
    assert!(!lines.iter().any(|line| line["event"] == "task_cleanup"));
    assert!(host.exists(paths.task_dir(id).to_str().unwrap()));
}

fn capture_cleanup(action: impl FnOnce()) -> Vec<Value> {
    let sink = SharedBuf::default();
    log::set_sink(Box::new(sink.clone()));
    action();
    log::set_sink(Box::new(std::io::sink()));
    sink_lines(&sink)
}

fn task_events<'a>(lines: &'a [Value], id: &str, name: &str) -> Vec<&'a Value> {
    lines
        .iter()
        .filter(|line| line["task_id"] == id && line["event"] == name)
        .collect()
}

#[tokio::test]
async fn cleanup_log_follows_release_once_and_stays_quiet_when_repeated() {
    use truenas_nvidia_driver_manager::{store::Paths, tasks};

    let _guard = lock().await;
    for explicit in [false, true] {
        let host = default_host();
        let paths = Paths::new(truenas_nvidia_driver_manager::codes::CONTAINER_DATA_DIR);
        release_scene(&host);
        let lines = capture_cleanup(|| {
            if explicit {
                tasks::release_recovery(&host, &paths, RELEASE_X).unwrap();
            } else {
                tasks::reconcile(&host, &paths);
            }
        });
        assert!(!host.exists(paths.task_dir(RELEASE_X).to_str().unwrap()));
        let events: Vec<&str> = lines
            .iter()
            .filter(|line| line["task_id"] == RELEASE_X)
            .map(|line| line["event"].as_str().unwrap())
            .collect();
        assert_eq!(events, ["task_released", "task_cleanup"], "{lines:?}");

        let again = capture_cleanup(|| {
            tasks::reconcile(&host, &paths);
            tasks::release_recovery(&host, &paths, RELEASE_X).unwrap();
        });
        assert!(task_events(&again, RELEASE_X, "task_cleanup").is_empty());
        assert!(task_events(&again, RELEASE_X, "host_reset").is_empty());
        host.assert_red_lines();
    }
}

#[tokio::test]
async fn cleanup_log_is_quiet_for_three_retained_reconciliations() {
    use truenas_nvidia_driver_manager::{store::Paths, tasks};

    let _guard = lock().await;
    let host = default_host();
    let paths = Paths::new(truenas_nvidia_driver_manager::codes::CONTAINER_DATA_DIR);
    release_scene(&host);
    host.remove(paths.state(RELEASE_L).to_str().unwrap());
    let lines = capture_cleanup(|| {
        for _ in 0..3 {
            tasks::reconcile(&host, &paths);
            assert!(host.exists(paths.task_dir(RELEASE_X).to_str().unwrap()));
        }
    });
    assert!(task_events(&lines, RELEASE_X, "task_cleanup").is_empty());
    assert!(task_events(&lines, RELEASE_X, "host_reset").is_empty());
    host.assert_red_lines();
}

#[tokio::test]
async fn cleanup_log_reports_only_observed_host_changes() {
    use truenas_nvidia_driver_manager::codes::{
        CONTAINER_DATA_DIR, HOST_CMD_TIMEOUT, RW_MOUNT, RW_TMP_PATH,
    };
    use truenas_nvidia_driver_manager::host::{At, Cmd, Host};
    use truenas_nvidia_driver_manager::{store::Paths, tasks};

    let _guard = lock().await;
    for change in [
        "unmount_writable_mount",
        "restore_readonly",
        "delete_temp_file",
    ] {
        let host = default_host();
        let paths = Paths::new(CONTAINER_DATA_DIR);
        release_scene(&host);
        host.remove(paths.state(RELEASE_L).to_str().unwrap());
        if change == "restore_readonly" {
            host.run(&Cmd::new(At::Host, "zfs", HOST_CMD_TIMEOUT).args([
                "set",
                "readonly=off",
                RELEASE_DATASET,
            ]));
            assert_eq!(host.readonly(), "off");
        } else {
            host.add_mount(RELEASE_DATASET, RW_MOUNT, "zfs", "rw");
            if change == "delete_temp_file" {
                host.put_text(RW_TMP_PATH, "partial image");
            }
        }
        let lines = capture_cleanup(|| {
            tasks::reconcile(&host, &paths);
        });
        let resets = task_events(&lines, RELEASE_X, "host_reset");
        let steps: Vec<&str> = resets
            .iter()
            .map(|line| {
                assert_eq!(line["level"], "INFO");
                assert_eq!(line["msg"], "host reset step applied");
                line["step"].as_str().unwrap()
            })
            .collect();
        let expected = if change == "delete_temp_file" {
            vec!["delete_temp_file", "unmount_writable_mount"]
        } else {
            vec![change]
        };
        assert_eq!(steps, expected, "{lines:?}");
        assert_eq!(host.readonly(), "on");
        assert!(!host
            .mount_table()
            .unwrap()
            .iter()
            .any(|m| m.mount_point == RW_MOUNT));
        assert!(!host.exists(RW_TMP_PATH));
        assert!(host.exists(paths.task_dir(RELEASE_X).to_str().unwrap()));
        assert!(task_events(&lines, RELEASE_X, "task_cleanup").is_empty());
        let again = capture_cleanup(|| {
            tasks::reconcile(&host, &paths);
        });
        assert!(task_events(&again, RELEASE_X, "host_reset").is_empty());
        host.assert_red_lines();
    }
}

#[tokio::test]
async fn cleanup_log_retains_failed_container_removal_for_retry() {
    use truenas_nvidia_driver_manager::{store::Paths, tasks};

    let _guard = lock().await;
    let host = default_host();
    let paths = Paths::new(truenas_nvidia_driver_manager::codes::CONTAINER_DATA_DIR);
    release_scene(&host);
    let container = format!("nvm-build-{RELEASE_X}");
    host.add_container(&container, false);
    host.set_docker_rm_fails(true);
    let lines = capture_cleanup(|| {
        tasks::reconcile(&host, &paths);
    });
    assert!(host.exists(paths.task_dir(RELEASE_X).to_str().unwrap()));
    assert!(host.container_names().contains(&container));
    assert!(task_events(&lines, RELEASE_X, "task_cleanup").is_empty());
    let failures = task_events(&lines, RELEASE_X, "task_cleanup_step_failed");
    let steps: Vec<&str> = failures
        .iter()
        .map(|line| {
            assert_eq!(line["level"], "ERROR");
            assert_reason_object(line, "cleanup failure");
            line["step"].as_str().unwrap()
        })
        .collect();
    assert_eq!(steps, ["remove_build_container", "delete_task_dir"]);
    host.set_docker_rm_fails(false);
    let retry = capture_cleanup(|| {
        tasks::reconcile(&host, &paths);
    });
    assert_eq!(task_events(&retry, RELEASE_X, "task_cleanup").len(), 1);
    assert!(!host.exists(paths.task_dir(RELEASE_X).to_str().unwrap()));
    host.assert_red_lines();
}

#[tokio::test]
async fn cleanup_log_does_not_invent_host_changes_on_failure_or_unknown_readings() {
    use truenas_nvidia_driver_manager::codes::{CONTAINER_DATA_DIR, RW_MOUNT, RW_TMP_PATH};
    use truenas_nvidia_driver_manager::{store::Paths, tasks};

    let _guard = lock().await;
    for case in [
        "unmount",
        "readonly",
        "before_unreadable",
        "before_invalid",
        "temp_sync",
    ] {
        let host = default_host();
        let paths = Paths::new(CONTAINER_DATA_DIR);
        release_scene(&host);
        host.remove(paths.state(RELEASE_L).to_str().unwrap());
        let query = ["get", "-H", "-o", "value", "readonly", RELEASE_DATASET];
        match case {
            "unmount" => {
                host.add_mount(RELEASE_DATASET, RW_MOUNT, "zfs", "rw");
                host.preset("umount", &[RW_MOUNT], FakeOutput::code(1));
            }
            "readonly" => host.preset("zfs", &query, FakeOutput::stdout("off\n")),
            "before_unreadable" => host.preset_once("zfs", &query, FakeOutput::code(1)),
            "before_invalid" => host.preset_once("zfs", &query, FakeOutput::stdout("unknown\n")),
            "temp_sync" => {
                host.add_mount(RELEASE_DATASET, RW_MOUNT, "zfs", "rw");
                host.put_text(RW_TMP_PATH, "partial image");
                host.fail_fsync_dir(
                    std::path::Path::new(RW_TMP_PATH)
                        .parent()
                        .unwrap()
                        .to_str()
                        .unwrap(),
                );
            }
            _ => unreachable!(),
        }
        let lines = capture_cleanup(|| {
            tasks::reconcile(&host, &paths);
        });
        let resets = task_events(&lines, RELEASE_X, "host_reset");
        if case == "temp_sync" {
            assert_eq!(resets.len(), 1, "{lines:?}");
            assert_eq!(resets[0]["step"], "delete_temp_file");
            assert!(!host.exists(RW_TMP_PATH));
        } else {
            assert!(resets.is_empty(), "{case}: {lines:?}");
        }
        if let Some(step) = match case {
            "unmount" => Some("unmount_writable_mount"),
            "readonly" => Some("restore_readonly"),
            "temp_sync" => Some("delete_temp_file"),
            _ => None,
        } {
            assert!(
                task_events(&lines, RELEASE_X, "task_cleanup_step_failed")
                    .iter()
                    .any(|line| line["step"] == step),
                "{case}: {lines:?}"
            );
        }
        assert!(task_events(&lines, RELEASE_X, "task_cleanup").is_empty());
        assert!(host.exists(paths.task_dir(RELEASE_X).to_str().unwrap()));
        host.assert_red_lines();
    }
}

/// The planning GPU reading failure is a `plan.rs` log caller: its reason is the same object.
#[tokio::test]
async fn plan_gpu_failure_reason_is_an_object() {
    let _guard = lock().await;
    let host = healthy();
    catalog_ready(&host);
    let base = "/sys/bus/pci/devices/0000:01:00.0";
    host.put_text(&format!("{base}/vendor"), "0x10de\n");
    host.put_text(&format!("{base}/class"), "0x030000\n");
    host.put_text(&format!("{base}/device"), "0x2684\n");
    host.fail_read(&format!("{base}/vendor"));
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    let cookie = login_changed(&app, "hunter12", "changed123").await;

    let sink = SharedBuf::default();
    log::set_sink(Box::new(sink.clone()));
    let (_status, _body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "580.178.04", "gpus": []}),
    )
    .await;
    log::set_sink(Box::new(std::io::sink()));
    let lines = sink_lines(&sink);

    let line = event(&lines, "gpus_unreadable");
    assert_reason_object(line, "plan gpus_unreadable");
    assert_eq!(line["reason"]["key"], "reason.permission_denied", "{line}");
    host.assert_red_lines();
}

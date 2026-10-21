mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use std::time::Duration;
use truenas_nvidia_driver_manager::codes::HOST_QUERY_TIMEOUT;
use truenas_nvidia_driver_manager::gpu_users;

fn usage_scene() -> FakeHost {
    let h = default_host();
    let ids: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.repeat(64)).collect();
    h.set_docker_ps(&ids.iter().map(String::as_str).collect::<Vec<_>>());
    h.set_docker_inspect(
        &json!([
            {"Id":ids[0],"Name":"/only-requested","Config":{"Env":["NVIDIA_VISIBLE_DEVICES=all"]}},
            {"Id":ids[1],"Name":"/using","Config":{"Env":["NVIDIA_VISIBLE_DEVICES=all"]}},
            {"Id":ids[2],"Name":"/unreadable","Config":{"Env":["NVIDIA_VISIBLE_DEVICES=all"]}},
            {"Id":ids[3],"Name":"/outside-list","Config":{"Env":[]}}
        ])
        .to_string(),
    );
    for (id, pid) in ids.iter().zip([101, 202, 303, 404]) {
        h.preset(
            "docker",
            &["top", id, "-eo", "pid"],
            FakeOutput::stdout(&format!("PID\n{pid}\n")),
        );
    }
    h.put_link("/proc/101/fd/0", "/dev/null");
    h.put_link("/proc/202/fd/8", "/dev/nvidia0");
    h.put_link("/proc/404/fd/8", "/dev/nvidia0");
    h.fail_read_dir("/proc/303/fd");
    h
}

#[test]
fn usage_is_scoped_to_container_pids_and_sorted() {
    let h = usage_scene();
    let result = gpu_users::list(&h, HOST_QUERY_TIMEOUT);
    let users = result
        .users
        .iter()
        .map(|u| serde_json::to_value(gpu_users::GpuUserJson::from(u)).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        users,
        json!([
            {"id":format!("docker:{}", "b".repeat(64)),"name":"using","type":"docker","usage":"in_use"},
            {"id":format!("docker:{}", "c".repeat(64)),"name":"unreadable","type":"docker","usage":"unknown"},
            {"id":format!("docker:{}", "a".repeat(64)),"name":"only-requested","type":"docker","usage":"requested"}
        ])
        .as_array()
        .unwrap()
        .clone()
    );
    for op in h.ops() {
        assert!(!op.path.contains("404"), "{op:?}");
        assert!(
            !op.path.contains("cmdline") && !op.path.contains("environ"),
            "{op:?}"
        );
    }
    assert!(!h.argv_log().iter().any(
        |args| args.get(1).is_some_and(|s| s == "top") && args.get(2) == Some(&"d".repeat(64))
    ));
}

#[tokio::test]
async fn snapshot_is_authenticated_cached_and_independent_of_process_lock() {
    let (app, cookie) = signed_in_app(usage_scene()).await;
    let (status, _) = get(&app, "/api/gpu-users", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let before = app.host.count("docker");
    let _held = app.ctx.process_lock.lock().await;
    let requests = async {
        tokio::join!(
            get(&app, "/api/gpu-users", Some(&cookie)),
            get(&app, "/api/gpu-users", Some(&cookie))
        )
    };
    let (a, b) = tokio::time::timeout(Duration::from_secs(2), requests)
        .await
        .expect("snapshot waited on the plan lock");
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(a.1, b.1);
    assert_eq!(a.1["state"], "idle");
    assert!(a.1["checked_at"].is_u64());
    assert_eq!(a.1["users"][0]["usage"], "in_use");
    assert_eq!(app.host.count("docker") - before, 5);
}

#[test]
fn usage_reads_all_members_and_drm_driver_and_uses_the_host_view() {
    for (device, driver, expected) in [
        ("/dev/nvidiactl", Some("nvidia"), "in_use"),
        ("/dev/nvidia-uvm-tools", Some("nvidia"), "in_use"),
        ("/dev/nvidia-modeset", Some("nvidia"), "in_use"),
        ("/dev/dri/renderD128", Some("nvidia"), "in_use"),
        ("/dev/dri/renderD128", Some("i915"), "requested"),
        ("/dev/dri/renderD128", None, "unknown"),
    ] {
        let h = default_host();
        let id = "e".repeat(64);
        h.put_link("/proc/1/root", "/");
        h.set_view_prefix(Some("/proc/1/root"));
        h.set_docker_ps(&[&id]);
        h.set_docker_inspect(
            &json!([{"Id":id,"Name":"/test","Config":{"Env":["NVIDIA_VISIBLE_DEVICES=all"]}}])
                .to_string(),
        );
        h.preset(
            "docker",
            &["top", &id, "-eo", "pid"],
            FakeOutput::stdout("PID\n101\n202\n"),
        );
        h.put_link("/proc/101/fd/0", "/dev/null");
        h.put_link("/proc/202/fd/3", device);
        if let Some(driver) = driver {
            h.add_drm_node("renderD128", driver);
        }
        let result = gpu_users::list(&h, HOST_QUERY_TIMEOUT);
        assert_eq!(
            serde_json::to_value(&result.users[0]).unwrap()["usage"],
            expected,
            "{device} {driver:?}"
        );
        assert!(h
            .ops()
            .iter()
            .any(|op| op.kind == "readlink" && op.path == "/proc/1/root/proc/202/fd/3"));
    }
}

#[test]
fn partial_fd_reads_and_untrusted_or_unavailable_process_lists_are_unknown() {
    let h = usage_scene();
    h.mkdir("/proc/202/fd/9"); // readlink fails even though fd 8 already proves use.
    let result = gpu_users::list(&h, HOST_QUERY_TIMEOUT);
    assert!(result
        .users
        .iter()
        .all(|u| u.name != "using" || serde_json::to_value(u).unwrap()["usage"] == "unknown"));
    for output in [
        FakeOutput::code(1),
        FakeOutput::stdout("PID\n"),
        FakeOutput::stdout("PID\n0\n"),
        FakeOutput::stdout("PID\n../404\n"),
        FakeOutput::stdout("PID CMD\n404 hidden\n"),
    ] {
        let h = usage_scene();
        h.preset("docker", &["top", &"b".repeat(64), "-eo", "pid"], output);
        let users = gpu_users::list(&h, HOST_QUERY_TIMEOUT).users;
        assert_eq!(
            serde_json::to_value(users.iter().find(|u| u.name == "using").unwrap()).unwrap()
                ["usage"],
            "unknown"
        );
        assert!(!h.ops().iter().any(|op| op.path.contains("/404/")));
    }
    let h = usage_scene();
    h.set_docker_inspect(
        &json!([{"Id":"not-in-ps","Name":"/test","Config":{"Env":["NVIDIA_VISIBLE_DEVICES=all"]}}])
            .to_string(),
    );
    assert_eq!(
        serde_json::to_value(&gpu_users::list(&h, HOST_QUERY_TIMEOUT).users[0]).unwrap()["usage"],
        "unknown"
    );
    assert!(!h
        .argv_log()
        .iter()
        .any(|args| args.get(1).is_some_and(|s| s == "top")));
}

#[tokio::test]
async fn cache_coalesces_an_in_flight_query_and_expires_after_completion() {
    let host = default_host();
    let gate = host.set_gate_output(
        "docker",
        &["ps", "-q", "--no-trunc"],
        FakeOutput::stdout(""),
    );
    let (app, cookie) = signed_in_app(host).await;
    let first_app = app.clone();
    let first_cookie = cookie.clone();
    let first =
        tokio::spawn(async move { get(&first_app, "/api/gpu-users", Some(&first_cookie)).await });
    for _ in 0..1000 {
        if gate.blocked() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(gate.blocked());
    let second_app = app.clone();
    let second_cookie = cookie.clone();
    let second =
        tokio::spawn(async move { get(&second_app, "/api/gpu-users", Some(&second_cookie)).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(app.host.count("docker"), 1);
    gate.release();
    assert_eq!(first.await.unwrap().1, second.await.unwrap().1);
    assert_eq!(app.host.count("docker"), 1);
    tokio::time::sleep(Duration::from_millis(1550)).await;
    app.host.load_module("nvidia", 1, &[]);
    let (_, body) = get(&app, "/api/gpu-users", Some(&cookie)).await;
    assert_eq!(body["state"], "busy");
    assert_eq!(app.host.count("docker"), 2);
}

#[tokio::test]
async fn snapshot_preserves_unreadable_occupancy_and_category_errors() {
    let h = default_host();
    h.fail_read("/proc/modules");
    h.set_midclt("call container.query", FakeOutput::code(7));
    let (app, cookie) = signed_in_app(h).await;
    let (_, body) = get(&app, "/api/gpu-users", Some(&cookie)).await;
    assert_eq!(body["state"], "unreadable");
    assert_eq!(body["read_reason"]["key"], "reason.permission_denied");
    assert_eq!(body["errors"][0]["type"], "incus");
    assert_eq!(body["errors"][0]["reason"]["params"]["code"], 7);
    let old: truenas_nvidia_driver_manager::task_state::GpuUserRecord =
        serde_json::from_value(json!({"name":"older","type":"docker"})).unwrap();
    assert!(old.usage.is_none());
}

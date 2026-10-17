//! The version index, kernel headers, runtime conditions and the source checks of
//! versions/planning.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::catalog::KernelModuleType;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::host::Exit;

const INDEX: &str = r#"<a href="../">..</a>
<a href="latest.txt">latest</a>
<a href="615.71.09/">615.71.09</a>
<a href="610.57.04/">610.57.04</a>
<a href="595.104.02/">595.104.02</a>
<a href="590.48.01/">590.48.01</a>
<a href="580.178.04/">580.178.04</a>
<a href="575.64.05/">575.64.05</a>
<a href="570.211.01/">570.211.01</a>
<a href="565.77/">565.77</a>
<a href="560.35.03/">560.35.03</a>
<a href="555.58.02/">555.58.02</a>
<a href="550.163.01/">550.163.01</a>
<a href="545.29.06/">545.29.06</a>
<a href="535.309.01/">535.309.01</a>
<a href="470.256.02/">470.256.02</a>
"#;

async fn setup() -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    prime_kernel_headers(&host, KERNEL);
    host.set_curl(CATALOG_URL, 200, INDEX.as_bytes());
    signed_in_app(host).await
}

#[tokio::test]
async fn items_are_the_buildable_branches_newest_first() {
    let (app, cookie) = setup().await;
    let (status, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(status, 200, "{body}");
    let items: Vec<String> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        items,
        vec![
            "615.71.09",
            "610.57.04",
            "595.104.02",
            "590.48.01",
            "580.178.04",
            "575.64.05",
            "570.211.01",
            "565.77",
            "550.163.01",
            "470.256.02"
        ]
    );
    assert_eq!(
        body["open_unbuildable"],
        json!(["575.64.05", "570.211.01", "565.77", "550.163.01"])
    );
    assert!(body["items"].as_array().unwrap().iter().all(|item| {
        let obj = item.as_object().unwrap();
        obj.len() == 1 && obj["version"].is_string()
    }));
    assert!(body["unreadable"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_new_branch_appears_in_the_list() {
    let (app, cookie) = setup().await;
    let with_new = format!("{INDEX}<a href=\"620.10.01/\">620.10.01</a>");
    app.host.set_curl(CATALOG_URL, 200, with_new.as_bytes());
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(body["items"][0]["version"], "620.10.01");
}

#[tokio::test]
async fn a_non_href_attribute_does_not_create_a_catalog_version() {
    let (app, cookie) = setup().await;
    let index = format!("{INDEX}<a data-href=\"999.99/\">preview</a>");
    app.host.set_curl(CATALOG_URL, 200, index.as_bytes());
    let (status, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["version"], "615.71.09", "{body}");
    assert!(body["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["version"] != "999.99"));
}

#[tokio::test]
async fn catalog_failure_empties_the_list() {
    let (app, cookie) = setup().await;
    app.host
        .set_curl_failure(CATALOG_URL, 6, "curl: (6) Could not resolve host");
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["open_unbuildable"], json!([]));
    let unreadable = body["unreadable"].as_array().unwrap();
    assert_eq!(unreadable[0]["object"], "catalog");
    assert_eq!(unreadable[0]["reason"]["key"], REASON_CONNECT_FAILED);
}

#[tokio::test]
async fn catalog_timeout_reasons_keep_the_limit_that_expired() {
    for (exit, seconds) in [(Exit::Code(28), 15), (Exit::TimedOut, 20)] {
        let (app, cookie) = setup().await;
        app.host.preset(
            "curl",
            &[],
            FakeOutput {
                exit,
                stdout: Vec::new(),
                stderr: b"curl timed out".to_vec(),
                delay: std::time::Duration::ZERO,
            },
        );
        let (status, body) = get(&app, "/api/versions", Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"], json!([]));
        assert_eq!(body["open_unbuildable"], json!([]));
        assert_eq!(
            body["unreadable"][0]["reason"],
            json!({"key": REASON_TIMED_OUT, "params": {"seconds": seconds}}),
            "{body}"
        );
        assert_eq!(body["unreadable"][0]["raw"], "curl timed out");
    }
}

#[tokio::test]
async fn http_status_failure_is_reported() {
    let (app, cookie) = setup().await;
    app.host.set_curl(CATALOG_URL, 404, b"not found");
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(body["unreadable"][0]["reason"]["key"], REASON_HTTP_STATUS);
    assert_eq!(body["unreadable"][0]["reason"]["params"]["status"], 404);
}

#[tokio::test]
async fn unreadable_kernel_headers_empty_the_list() {
    let (app, cookie) = setup().await;
    app.host
        .remove("/usr/src/linux-headers-truenas-production-amd64/include/drm/drm_mode_config.h");
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["open_unbuildable"], json!([]));
    let unreadable = body["unreadable"].as_array().unwrap();
    assert!(unreadable.iter().any(|u| u["object"] == "kernel_headers"
        && u["reason"]["key"] == REASON_FILE_MISSING
        && u["raw"]
            .as_str()
            .unwrap()
            .ends_with("drm/drm_mode_config.h")));
}

#[tokio::test]
async fn unreadable_gpus_empty_the_list_and_gpus() {
    let (app, cookie) = setup().await;
    app.host.mkdir("/sys/bus/pci/devices/0000:01:00.0");
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["open_unbuildable"], json!([]));
    assert_eq!(body["gpus"].as_array().unwrap().len(), 0);
    assert!(body["unreadable"]
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["object"] == "gpus"));
}

#[tokio::test]
async fn invalid_gpu_address_empties_the_list() {
    let (app, cookie) = setup().await;
    app.host.mkdir("/sys/bus/pci/devices/not-an-address");
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(
        body["unreadable"][0]["reason"]["key"],
        REASON_FORMAT_UNRECOGNIZED
    );
}

#[tokio::test]
async fn no_gpus_means_the_items_stay() {
    let (app, cookie) = setup().await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(body["gpus"].as_array().unwrap().len(), 0);
    assert_eq!(body["items"].as_array().unwrap().len(), 10);
}

#[tokio::test]
async fn official_470_follows_the_actual_kernel_without_erasing_other_branches() {
    let (app, cookie) = setup().await;
    for kernel in ["6.12.105-production+truenas", "6.12.96-production+truenas"] {
        app.host.set_uname(kernel);
        let (_, list) = get(&app, "/api/versions", Some(&cookie)).await;
        assert_eq!(
            list["items"].as_array().unwrap().len(),
            10,
            "{kernel}: {list}"
        );
        assert_eq!(list["items"][9]["version"], COMMUNITY_PATCH_VERSION);
        assert!(!list["open_unbuildable"]
            .as_array()
            .unwrap()
            .contains(&json!(COMMUNITY_PATCH_VERSION)));
        let (status, plan) = post_plan(
            &app,
            &cookie,
            json!({"kind": "official", "version": COMMUNITY_PATCH_VERSION, "gpus": []}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{kernel}: {plan}");
        assert_eq!(plan["dialog"]["kernel"], kernel);
    }
    for kernel in ["6.13.1-production+truenas", "6.120.1", "6.12evil.1", ""] {
        app.host.set_uname(kernel);
        let (_, list) = get(&app, "/api/versions", Some(&cookie)).await;
        assert_eq!(
            list["items"].as_array().unwrap().len(),
            9,
            "{kernel}: {list}"
        );
        assert!(!list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["version"] == COMMUNITY_PATCH_VERSION }));
        assert!(list["unreadable"].as_array().unwrap().is_empty(), "{list}");
    }
    let (status, result) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": COMMUNITY_PATCH_VERSION, "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["result"], "uncomputable");
    assert_eq!(result["items"][0]["object"], "current_kernel");
}

#[tokio::test]
async fn planning_rejects_an_unbuildable_branch_with_its_reasons() {
    let (app, cookie) = setup().await;
    app.host.set_uname("6.13.1-production+truenas");
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "470.256.02", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.source_invalid.branch_unbuildable");
    assert_eq!(body["params"]["branch"], 470);
    let reasons = body["params"]["reasons"].as_array().unwrap();
    assert!(reasons.contains(&json!({"key": BRANCH_FOLLOW_PFN_REMOVED, "params": {}})));
    assert!(reasons.contains(&json!({"key": BRANCH_OUTPUT_POLL_CHANGED_REMOVED, "params": {}})));
    app.host.set_uname("6.12.105-production+truenas");
    let (status, newer) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "470.256.03", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{newer}");
    assert_eq!(newer["error"], "plan.source_invalid.branch_unbuildable");
}

#[tokio::test]
async fn submission_rejects_a_qualified_470_plan_after_the_kernel_changes() {
    let (app, cookie) = setup().await;
    let (status, plan) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": COMMUNITY_PATCH_VERSION, "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    app.host.set_uname("6.13.1-production+truenas");
    let (status, submit) = post_task(&app, &cookie, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::CONFLICT, "{submit}");
    assert_eq!(submit["error"], "plan.changed");
    assert!(submit["params"]["changed"]
        .as_array()
        .unwrap()
        .contains(&json!("kernel")));
    assert_eq!(app.host.count("systemd-run"), 0);
}

#[tokio::test]
async fn planning_rejects_versions_below_470_and_bad_formats() {
    let (app, cookie) = setup().await;
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "460.106.00", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.source_invalid.version_below_470");
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "580", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.source_invalid.version_format");
}

#[tokio::test]
async fn planning_rejects_when_the_headers_are_unreadable() {
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x2684, Some("nvidia"))]).await;
    app.host
        .remove("/usr/src/linux-headers-truenas-production-amd64/include/linux/pci.h");
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "570.211.01", "gpus": ["0000:01:00.0"]}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "plan.source_invalid.branch_undecidable");
    assert!(body["params"]["unreadable"][0]["raw"]
        .as_str()
        .unwrap()
        .ends_with("linux/pci.h"));
}

#[tokio::test]
async fn planning_rejects_when_a_runtime_condition_holds() {
    let (app, cookie) = setup().await;
    // Kernel 6.9 headers make 470 buildable, but nvidia_drm is not loaded.
    prime_kernel_headers(&app.host, "6.9.0");
    app.host.set_uname("6.9.0-production+truenas");
    app.host.unload_module("nvidia_drm");
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "470.256.02", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.source_invalid.branch_unbuildable");
    assert!(body["params"]["reasons"]
        .as_array()
        .unwrap()
        .contains(&json!({"key": BRANCH_NVIDIA_DRM_NOT_LOADED, "params": {}})));
}

// ---- GPU scenes with the real generated table ---------------

async fn gpu_setup(gpus: &[(&str, u16, Option<&str>)]) -> (TestApp, String) {
    let (app, cookie) = setup().await;
    for (address, device, driver) in gpus {
        app.host.add_gpu(address, *device, *driver);
    }
    (app, cookie)
}

fn gpu_entry<'a>(body: &'a serde_json::Value, device: &str) -> &'a serde_json::Value {
    body["gpus"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["device"] == device)
        .unwrap_or_else(|| panic!("no gpu {device} in {body}"))
}

#[tokio::test]
async fn gtx_1080_scene() {
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x1b80, Some("nvidia"))]).await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    let gpu = gpu_entry(&body, "1b80");
    assert_eq!(gpu["name"], "NVIDIA GeForce GTX 1080");
    assert_eq!(gpu["modules"], "closed_only");
    assert_eq!(gpu["last_branch"], 580);
    assert_eq!(
        gpu["too_new"],
        json!(["615.71.09", "610.57.04", "595.104.02", "590.48.01"])
    );
    assert_eq!(gpu["too_old"], json!([]));
    // Planning rejects a newer version with the per-GPU reason payload.
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "590.48.01", "gpus": ["0000:01:00.0"]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.source_invalid.gpu_unsupported");
    assert_eq!(body["params"]["gpus"][0]["device"], "1b80");
    assert_eq!(body["params"]["gpus"][0]["last_branch"], 580);
    assert_eq!(body["params"]["conflict"], false);
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "580.178.04", "gpus": ["0000:01:00.0"]}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"], "ready");
}

#[tokio::test]
async fn kepler_scene_can_plan_the_qualified_470_version() {
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x1180, Some("nvidia"))]).await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    let gpu = gpu_entry(&body, "1180");
    assert_eq!(gpu["last_branch"], 470);
    assert_eq!(gpu["too_new"].as_array().unwrap().len(), 9);
    let (status, qualified) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": COMMUNITY_PATCH_VERSION, "gpus": ["0000:01:00.0"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{qualified}");
    assert_eq!(qualified["result"], "ready");
    assert_eq!(
        app.ctx
            .plan
            .lock()
            .unwrap()
            .0
            .as_ref()
            .unwrap()
            .kernel_module_type,
        None
    );
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "580.178.04", "gpus": ["0000:01:00.0"]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["params"]["gpus"][0]["last_branch"], 470);
    // The GT 640 that dropped out of the newest list keeps 470 as well.
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x0fc0, Some("nvidia"))]).await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    let gpu = gpu_entry(&body, "0fc0");
    assert_eq!(gpu["name"], "NVIDIA GeForce GT 640");
    assert_eq!(gpu["last_branch"], 470);
}

#[tokio::test]
async fn blackwell_scene_uses_open_modules() {
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x2b85, Some("nvidia"))]).await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    let gpu = gpu_entry(&body, "2b85");
    assert_eq!(gpu["modules"], "open_only");
    assert!(gpu.get("last_branch").is_none());
    assert_eq!(gpu["first_branch"], 570);
    assert_eq!(gpu["too_new"].as_array().unwrap().len(), 0);
    assert_eq!(
        gpu["too_old"],
        json!(["565.77", "550.163.01", "470.256.02"])
    );
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "580.178.04", "gpus": ["0000:01:00.0"]}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn qualified_470_respects_first_and_last_gpu_versions_in_multicard_plans() {
    type Case<'a> = (&'a str, &'a [(u16, &'a str)], bool);
    let cases: &[Case<'_>] = &[
        (
            "kepler plus pascal",
            &[(0x1180, "0000:01:00.0"), (0x1b80, "0000:02:00.0")],
            true,
        ),
        (
            "kepler plus first-470 either",
            &[(0x1180, "0000:01:00.0"), (0x1e02, "0000:02:00.0")],
            true,
        ),
        (
            "kepler plus first-520",
            &[(0x1180, "0000:01:00.0"), (0x2684, "0000:02:00.0")],
            false,
        ),
        ("pre-470", &[(0x06c0, "0000:01:00.0")], false),
        ("unlisted kepler", &[(0x101f, "0000:01:00.0")], true),
        ("unlisted either", &[(0x1e37, "0000:01:00.0")], true),
        ("unlisted open", &[(0x2c2c, "0000:01:00.0")], false),
    ];
    for (name, devices, accepted) in cases {
        let gpus: Vec<(&str, u16, Option<&str>)> = devices
            .iter()
            .map(|(device, address)| (*address, *device, Some("nvidia")))
            .collect();
        let (app, cookie) = gpu_setup(&gpus).await;
        let (_, list) = get(&app, "/api/versions", Some(&cookie)).await;
        assert_eq!(
            list["items"][9]["version"], COMMUNITY_PATCH_VERSION,
            "{name}"
        );
        if devices.iter().any(|(device, _)| *device == 0x2684) {
            assert_eq!(gpu_entry(&list, "2684")["first_branch"], 520, "{name}");
            assert!(gpu_entry(&list, "2684")["too_old"]
                .as_array()
                .unwrap()
                .contains(&json!(COMMUNITY_PATCH_VERSION)));
        }
        let selected: Vec<&str> = devices.iter().map(|(_, address)| *address).collect();
        let (status, result) = post_plan(
            &app,
            &cookie,
            json!({"kind": "official", "version": COMMUNITY_PATCH_VERSION, "gpus": selected}),
        )
        .await;
        if *accepted {
            assert_eq!(status, StatusCode::OK, "{name}: {result}");
            assert_eq!(result["result"], "ready", "{name}: {result}");
        } else {
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name}: {result}");
            assert_eq!(result["error"], "plan.source_invalid.gpu_unsupported");
        }
    }
}

#[tokio::test]
async fn a_mixed_selection_is_a_conflict() {
    let (app, cookie) = gpu_setup(&[
        ("0000:01:00.0", 0x1b80, Some("nvidia")),
        ("0000:02:00.0", 0x2b85, None),
    ])
    .await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(body["gpus"].as_array().unwrap().len(), 2);
    assert!(!body["gpus"][0]["address"].is_null());
    let (status, body) = post_plan(&app, &cookie, json!({"kind": "official", "version": "580.178.04", "gpus": ["0000:01:00.0", "0000:02:00.0"]}))
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.source_invalid.gpu_unsupported");
    assert_eq!(body["params"]["conflict"], true);
    assert_eq!(body["params"]["gpus"].as_array().unwrap().len(), 0);
    let (_, body) = post_plan(&app, &cookie, json!({"kind": "upload", "group_id": "0".repeat(32), "version": "580.178.04", "gpus": ["0000:01:00.0", "0000:02:00.0"]}))
    .await;
    // The upload source is validated first, so this is upload_group_gone.
    assert_eq!(body["error"], "plan.source_invalid.upload_group_gone");
}

#[tokio::test]
async fn an_uppercase_pci_directory_name_makes_the_gpu_reading_unknown() {
    // The directory regex is lowercase-only; one non-matching name makes the whole GPU reading
    // unreadable.
    let (app, cookie) = gpu_setup(&[("0000:0A:00.0", 0x1b80, Some("nvidia"))]).await;
    let (status, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["unreadable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["object"] == "gpus" && u["reason"]["key"] == REASON_FORMAT_UNRECOGNIZED),
        "{body}"
    );
    assert_eq!(body["gpus"], json!([]));
    assert_eq!(body["items"], json!([]));
}

#[tokio::test]
async fn a_fresh_gpu_reading_after_a_dialed_clock_uses_the_host_clock() {
    // A reading that starts on the host clock gets its own full budget even after the fake clock
    // was moved forward.
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x1b80, Some("nvidia"))]).await;
    app.host.advance(std::time::Duration::from_secs(31));
    let (status, body) = get(&app, "/api/versions", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["unreadable"].as_array().unwrap().is_empty(),
        "a fresh GPU reading budget was already expired by the dialed clock: {body}"
    );
    assert_eq!(body["gpus"].as_array().unwrap().len(), 1, "{body}");
}

#[tokio::test]
async fn a_vfio_bound_gpu_is_not_counted() {
    let (app, cookie) = gpu_setup(&[
        ("0000:01:00.0", 0x1b80, Some("vfio-pci")),
        ("0000:02:00.0", 0x2b85, Some("nvidia")),
    ])
    .await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    let gpus = body["gpus"].as_array().unwrap();
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0]["device"], "2b85");
}

#[tokio::test]
async fn a_selected_gpu_handed_to_vfio_is_reported_as_changed() {
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x1b80, Some("nvidia"))]).await;
    app.host.set_gpu_driver("0000:01:00.0", Some("vfio-pci"));
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "580.178.04", "gpus": ["0000:01:00.0"]}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "plan.changed");
    assert_eq!(body["params"]["changed"][0], "gpus");
}

#[tokio::test]
async fn devices_outside_the_table_use_the_id_sections() {
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x101f, Some("nvidia"))]).await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    let gpu = gpu_entry(&body, "101f");
    assert_eq!(gpu["modules"], "closed_only");
    assert_eq!(gpu["last_branch"], 470);
    assert!(gpu.get("name").is_none());
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x1b07, Some("nvidia"))]).await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    let gpu = gpu_entry(&body, "1b07");
    assert_eq!(gpu["modules"], "closed_only");
    assert_eq!(gpu["last_branch"], 580);
    assert_eq!(gpu["too_new"].as_array().unwrap().len(), 4);
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x1e37, Some("nvidia"))]).await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    let gpu = gpu_entry(&body, "1e37");
    assert_eq!(gpu["modules"], "either");
    assert_eq!(gpu["first_branch"], 470);
    assert_eq!(gpu["too_new"].as_array().unwrap().len(), 0);
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x2c2c, Some("nvidia"))]).await;
    let (_, body) = get(&app, "/api/versions", Some(&cookie)).await;
    let gpu = gpu_entry(&body, "2c2c");
    assert_eq!(gpu["modules"], "open_only");
    assert_eq!(gpu["first_branch"], 570);
}

#[tokio::test]
async fn module_type_scenes_share_catalog_candidates_and_plan_types() {
    const ITEMS: &[&str] = &[
        "615.71.09",
        "610.57.04",
        "595.104.02",
        "590.48.01",
        "580.178.04",
        "575.64.05",
        "570.211.01",
        "565.77",
        "550.163.01",
        "470.256.02",
    ];
    const OPEN: &[&str] = &[
        "615.71.09",
        "610.57.04",
        "595.104.02",
        "590.48.01",
        "580.178.04",
    ];
    const CLOSED: &[&str] = &[
        "580.178.04",
        "575.64.05",
        "570.211.01",
        "565.77",
        "550.163.01",
    ];
    const CLOSED_WITH_470: &[&str] = &[
        "580.178.04",
        "575.64.05",
        "570.211.01",
        "565.77",
        "550.163.01",
        "470.256.02",
    ];
    const EITHER: &[&str] = &[
        "615.71.09",
        "610.57.04",
        "595.104.02",
        "590.48.01",
        "580.178.04",
        "550.163.01",
    ];
    const EITHER_WITH_470: &[&str] = &[
        "615.71.09",
        "610.57.04",
        "595.104.02",
        "590.48.01",
        "580.178.04",
        "550.163.01",
        "470.256.02",
    ];
    type Case<'a> = (&'a str, &'a [(u16, Option<&'a str>, bool)], &'a [&'a str]);
    let cases: &[Case<'_>] = &[
        ("no counted GPUs", &[], ITEMS),
        ("all vfio", &[(0x1b80, Some("vfio-pci"), false)], ITEMS),
        ("1b80", &[(0x1b80, Some("nvidia"), true)], CLOSED_WITH_470),
        ("1b07", &[(0x1b07, Some("nvidia"), true)], CLOSED_WITH_470),
        ("2684", &[(0x2684, Some("nvidia"), true)], EITHER),
        ("2b85", &[(0x2b85, Some("nvidia"), true)], OPEN),
        ("2c33", &[(0x2c33, Some("nvidia"), true)], OPEN),
        ("1e37", &[(0x1e37, Some("nvidia"), true)], EITHER_WITH_470),
        ("2c2c", &[(0x2c2c, Some("nvidia"), true)], OPEN),
        (
            "Closed + Either",
            &[(0x1b80, Some("nvidia"), true), (0x2684, None, true)],
            CLOSED,
        ),
        (
            "Closed + Open",
            &[(0x1b80, Some("nvidia"), true), (0x2b85, None, true)],
            &[],
        ),
        (
            "Closed + unlisted Open",
            &[(0x1b80, Some("nvidia"), true), (0x2c2c, None, true)],
            &[],
        ),
        (
            "Either + Open",
            &[(0x2684, Some("nvidia"), true), (0x2b85, None, true)],
            OPEN,
        ),
        (
            "unlisted Closed + Closed",
            &[(0x1b07, Some("nvidia"), true), (0x1b80, None, true)],
            CLOSED_WITH_470,
        ),
        (
            "unlisted Closed + Either",
            &[(0x1b07, Some("nvidia"), true), (0x2684, None, true)],
            CLOSED,
        ),
        (
            "unlisted Closed + Open",
            &[(0x1b07, Some("nvidia"), true), (0x2b85, None, true)],
            &[],
        ),
        (
            "Open + unlisted Either, Closed vfio",
            &[
                (0x2b85, Some("nvidia"), true),
                (0x1e37, None, true),
                (0x1b80, Some("vfio-pci"), false),
            ],
            OPEN,
        ),
        (
            "two Open, Closed vfio",
            &[
                (0x2b85, Some("nvidia"), true),
                (0x2c2c, None, true),
                (0x1b80, Some("vfio-pci"), false),
            ],
            OPEN,
        ),
        (
            "Open, Closed vfio",
            &[
                (0x2b85, Some("nvidia"), true),
                (0x1b80, Some("vfio-pci"), false),
            ],
            OPEN,
        ),
        (
            "two counted, neither selected",
            &[(0x1b80, Some("nvidia"), false), (0x2b85, None, false)],
            &[],
        ),
        (
            "only Closed selected",
            &[(0x1b80, Some("nvidia"), true), (0x2b85, None, false)],
            CLOSED_WITH_470,
        ),
        (
            "only Open selected",
            &[(0x1b80, Some("nvidia"), false), (0x2b85, None, true)],
            OPEN,
        ),
        (
            "only Either selected",
            &[(0x1b07, Some("nvidia"), false), (0x2684, None, true)],
            EITHER,
        ),
    ];
    let (app, cookie) = setup().await;
    for (name, devices, accepted) in cases {
        app.host.remove_tree(PCI_DEVICES_PATH);
        app.host.mkdir(PCI_DEVICES_PATH);
        let mut addresses = Vec::new();
        let mut selected_devices = Vec::new();
        for (i, (device, driver, selected)) in devices.iter().enumerate() {
            let address = format!("0000:{:02x}:00.0", i + 1);
            app.host.add_gpu(&address, *device, *driver);
            if *selected {
                addresses.push(address);
                selected_devices.push(*device);
            }
        }
        let (status, body) = get(&app, "/api/versions", Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
        let listed: Vec<&str> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["version"].as_str().unwrap())
            .collect();
        assert_eq!(listed, ITEMS, "{name}");
        assert_eq!(
            body["open_unbuildable"],
            json!(["575.64.05", "570.211.01", "565.77", "550.163.01"]),
            "{name}"
        );
        assert!(
            body["unreadable"].as_array().unwrap().is_empty(),
            "{name}: {body}"
        );
        for version in ITEMS {
            let (status, result) = post_plan(
                &app,
                &cookie,
                json!({
                    "kind": "official", "version": version, "gpus": addresses,
                }),
            )
            .await;
            if accepted.contains(version) {
                assert_eq!(status, StatusCode::OK, "{name}/{version}: {result}");
                assert_eq!(result["result"], "ready", "{name}/{version}: {result}");
                let slot = app.ctx.plan.lock().unwrap();
                let plan = slot.0.as_ref().unwrap();
                let expected_type = if *version == "550.163.01"
                    || *version == COMMUNITY_PATCH_VERSION
                    || addresses.is_empty()
                {
                    None
                } else if selected_devices.iter().any(|d| *d < 0x1e00) {
                    Some(KernelModuleType::Proprietary)
                } else {
                    Some(KernelModuleType::Open)
                };
                assert_eq!(plan.kernel_module_type, expected_type, "{name}/{version}");
                assert_eq!(plan.version, *version);
                assert_eq!(
                    plan.gpus.iter().map(|g| &g.address).collect::<Vec<_>>(),
                    addresses.iter().collect::<Vec<_>>()
                );
            } else {
                assert_ne!(status, StatusCode::OK, "{name}/{version}: {result}");
                assert!(
                    result["error"].as_str().is_some(),
                    "{name}/{version}: {result}"
                );
            }
        }
    }
    assert_eq!(cases.len(), 23);
    assert_eq!(app.host.count("systemd-run"), 0);
}

#[tokio::test]
async fn planning_uses_pci_headers_only_for_explicit_open() {
    for devices in [&[][..], &[("0000:01:00.0", 0x1b80, Some("nvidia"))][..]] {
        let (app, cookie) = gpu_setup(devices).await;
        app.host
            .remove("/usr/src/linux-headers-truenas-production-amd64/include/linux/pci.h");
        let gpus: Vec<&str> = devices.iter().map(|(address, _, _)| *address).collect();
        let (status, result) = post_plan(
            &app,
            &cookie,
            json!({
                "kind": "official", "version": "570.211.01", "gpus": gpus,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["result"], "ready");
        let (_, list) = get(&app, "/api/versions", Some(&cookie)).await;
        assert_eq!(list["items"], json!([]));
        assert_eq!(list["open_unbuildable"], json!([]));
        assert_eq!(list["unreadable"][0]["object"], "kernel_headers");
    }
}

#[tokio::test]
async fn planning_checks_generic_rules_and_runtime_before_gpu_reading() {
    let (app, cookie) = setup().await;
    app.host.mkdir("/sys/bus/pci/devices/bad-address");
    let (_, result) = post_plan(
        &app,
        &cookie,
        json!({
            "kind": "official", "version": "560.35.03", "gpus": [],
        }),
    )
    .await;
    assert_eq!(result["error"], "plan.source_invalid.branch_unbuildable");
    assert_eq!(
        result["params"]["reasons"],
        json!([
            {"key": BRANCH_OUTPUT_POLL_CHANGED_REMOVED, "params": {}},
        ])
    );
    prime_kernel_headers(&app.host, "6.9.0");
    app.host.unload_module("nvidia");
    let (_, result) = post_plan(
        &app,
        &cookie,
        json!({
            "kind": "official", "version": "545.29.06", "gpus": [],
        }),
    )
    .await;
    assert_eq!(result["error"], "plan.source_invalid.branch_unbuildable");
    assert_eq!(
        result["params"]["reasons"],
        json!([
            {"key": BRANCH_NVIDIA_NOT_LOADED, "params": {}},
        ])
    );
}

#[tokio::test]
async fn planning_checks_fresh_gpu_addresses_before_pci_and_pci_before_gpu_fit() {
    let (app, cookie) = gpu_setup(&[("0000:01:00.0", 0x3001, Some("nvidia"))]).await;
    let (_, result) = post_plan(
        &app,
        &cookie,
        json!({
            "kind": "official", "version": "575.64.05", "gpus": ["0000:01:00.0"],
        }),
    )
    .await;
    assert_eq!(result["error"], "plan.source_invalid.branch_unbuildable");
    assert_eq!(
        result["params"]["reasons"],
        json!([
            {"key": BRANCH_PCI_RESIZE_RESOURCE_ARGS, "params": {}},
        ])
    );
    app.host.set_gpu_driver("0000:01:00.0", Some("vfio-pci"));
    let (_, result) = post_plan(
        &app,
        &cookie,
        json!({
            "kind": "official", "version": "575.64.05", "gpus": ["0000:01:00.0"],
        }),
    )
    .await;
    assert_eq!(result["error"], "plan.changed");
    app.host.mkdir("/sys/bus/pci/devices/bad-address");
    let (_, result) = post_plan(
        &app,
        &cookie,
        json!({
            "kind": "official", "version": "575.64.05", "gpus": ["0000:01:00.0"],
        }),
    )
    .await;
    assert_eq!(result["error"], "plan.source_invalid.gpu_undecidable");
}

#[tokio::test]
async fn submission_persists_the_single_selected_module_type() {
    for (device, version, expected_type) in [
        (0x1b80, "570.211.01", json!("proprietary")),
        (0x2684, "580.178.04", json!("open")),
        (0x2684, "550.163.01", json!(null)),
    ] {
        let (app, cookie) = gpu_setup(&[("0000:01:00.0", device, Some("nvidia"))]).await;
        let (status, plan) = post_plan(
            &app,
            &cookie,
            json!({
                "kind": "official", "version": version, "gpus": ["0000:01:00.0"],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{plan}");
        let (status, result) = post_task(&app, &cookie, &plan["plan_id"], "skip").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{result}");
        let id = result["task_id"].as_str().unwrap();
        let state: serde_json::Value = serde_json::from_slice(
            &app.host
                .file(&format!("/var/lib/nvm/manager/state/{id}.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(state["plan"]["kernel_module_type"], expected_type);
        assert_eq!(state["plan"]["version"], version);
        assert_eq!(
            state["plan"]["gpus"],
            json!([{"address": "0000:01:00.0", "device": format!("{device:04x}")} ])
        );
    }
}

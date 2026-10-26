//! Uploads, their validation, the group summary and the upload source.

mod common;

use axum::http::{header, Method, StatusCode};
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::host::{At, HOST_PATH};

const RUN_BODY: &[u8] = b"#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\nrest of the file";

async fn setup() -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    signed_in_app(host).await
}

async fn open_group(app: &TestApp, cookie: &str) -> String {
    let (status, body) = post(app, "/api/uploads", json!({}), Some(cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["group_id"].as_str().unwrap().to_string()
}

async fn upload(
    app: &TestApp,
    cookie: &str,
    group: &str,
    name: &str,
    bytes: &[u8],
) -> (StatusCode, serde_json::Value) {
    request_raw(
        app,
        Method::PUT,
        &format!("/api/uploads/{group}/files?name={name}"),
        bytes.to_vec(),
        "application/octet-stream",
        Some(cookie),
    )
    .await
}

#[tokio::test]
async fn run_is_accepted_and_the_version_read() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    let (status, body) = upload(
        &app,
        &cookie,
        &group,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        RUN_BODY,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["file"]["kind"], "run");
    assert_eq!(body["file"]["candidates"][0], "580.178.04");
    assert_eq!(body["file"]["size"], RUN_BODY.len() as u64);
    assert_eq!(body["file"]["sha256"], sha256_hex(RUN_BODY));
    assert_eq!(
        body["group"]["run"]["fingerprint"],
        sha256_hex(RUN_BODY)[..12].to_string()
    );
}

#[tokio::test]
async fn bad_run_headers_are_rejected() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    let (status, body) = upload(&app, &cookie, &group, "x.run", b"not a shell").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "upload.rejected");
    assert_eq!(body["params"]["reason"], "run_not_shebang");
    let (status, body) = upload(
        &app,
        &cookie,
        &group,
        "y.run",
        b"#!/bin/sh\nlabel=\"something 580.178.04\"\n",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["params"]["reason"], "run_not_linux_x86_64");
}

#[tokio::test]
async fn other_extensions_are_rejected() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    let (status, body) = upload(&app, &cookie, &group, "driver.txt", b"hello").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["params"]["reason"], "not_run_or_deb");
}

#[tokio::test]
async fn a_file_over_1_gib_is_rejected_from_the_content_length() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    let req = axum::http::Request::builder()
        .method(Method::PUT)
        .uri(format!("/api/uploads/{group}/files?name=x.run"))
        .header(header::HOST, "nas.example:8199")
        .header(header::ORIGIN, "http://nas.example:8199")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, (UPLOAD_FILE_LIMIT + 1).to_string())
        .header(header::COOKIE, format!("nvm_session={cookie}"))
        .header("x-nvm-token", token_for(&cookie).unwrap_or_default())
        .body(axum::body::Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(app.router.clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn deb_fields_are_checked() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    app.host.set_dpkg_default(
        "Package: libnvidia-container1\nVersion: 1.20.1-1\nArchitecture: amd64\n",
    );
    let (status, body) = upload(
        &app,
        &cookie,
        &group,
        "libnvidia-container1_1.20.1-1_amd64.deb",
        b"fake deb",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["file"]["package"], "libnvidia-container1");
    assert_eq!(body["file"]["version"], "1.20.1-1");
    // The fields are read on the host, with the host path of the received file: the web container
    // runs no program from its own image.
    let command = app
        .host
        .commands()
        .into_iter()
        .find(|c| c.program == "dpkg-deb")
        .expect("dpkg-deb ran");
    assert_eq!(command.at, At::Host);
    assert_eq!(
        command.args.first().map(String::as_str),
        Some("--field"),
        "{:?}",
        command.args
    );
    let file = command.args.get(1).expect("the file argument");
    assert!(
        file.starts_with(HOST_DATA_DIR),
        "the host path of the received file: {:?}",
        command.args
    );
    assert!(
        !file.starts_with(CONTAINER_DATA_DIR),
        "not the container path: {:?}",
        command.args
    );
    app.host
        .set_dpkg_default("Package: something-else\nVersion: 1.20.1-1\nArchitecture: amd64\n");
    let (status, body) = upload(&app, &cookie, &group, "other.deb", b"fake deb").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["params"]["reason"], "deb_package_unknown");
    app.host.set_dpkg_default(
        "Package: libnvidia-container1\nVersion: 1.20.1-1\nArchitecture: arm64\n",
    );
    let (status, body) = upload(&app, &cookie, &group, "arm.deb", b"fake deb").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["params"]["reason"], "deb_arch");
    app.host.set_dpkg_default(
        "Package: libnvidia-container1\nVersion: one\ntwo\nArchitecture: amd64\n",
    );
    let (status, body) = upload(&app, &cookie, &group, "bad.deb", b"fake deb").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["params"]["reason"], "deb_version_format");
}

/// A real `.deb` built with the host-command `dpkg-deb`, or `None` when the program is not on the
/// host-command PATH (the CI image carries it).
fn real_deb(package: &str, version: &str, arch: &str) -> Option<Vec<u8>> {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("nvm-upload-deb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let control = dir.join("pkg/DEBIAN");
    std::fs::create_dir_all(&control).ok()?;
    // A permissive umask can make dpkg-deb refuse the control directory.
    std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o755)).ok()?;
    std::fs::write(
        control.join("control"),
        format!(
            "Package: {package}\nVersion: {version}\nArchitecture: {arch}\n\
             Maintainer: t <t@example.com>\nDescription: t\n"
        ),
    )
    .ok()?;
    std::fs::set_permissions(
        control.join("control"),
        std::fs::Permissions::from_mode(0o644),
    )
    .ok()?;
    std::fs::write(dir.join("pkg/payload"), b"x").ok()?;
    let deb = dir.join(format!("{package}_{version}_{arch}.deb"));
    let built = std::process::Command::new("dpkg-deb")
        .env_clear()
        .env("LC_ALL", "C")
        .env("PATH", HOST_PATH)
        .arg("--build")
        .arg(dir.join("pkg"))
        .arg(&deb)
        .output()
        .ok()?;
    let bytes = if built.status.success() {
        std::fs::read(&deb).ok()
    } else {
        None
    };
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

#[tokio::test]
async fn a_deb_without_the_confirmed_host_mapping_is_rejected_as_unreadable() {
    // The start-up check did not confirm the data-directory mapping, so no host path can be
    // derived: the file is refused like an unreadable one and no host program runs at all.
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let mut startup = healthy_startup(&host);
    startup.host_data_dir = None;
    let app = app(host.clone(), settings(), startup);
    let _ = get(&app, "/api/login", None).await;
    let cookie = setup_app(&app, "longpass99").await;
    let group = open_group(&app, &cookie).await;
    app.host.set_dpkg_default(
        "Package: libnvidia-container1\nVersion: 1.20.1-1\nArchitecture: amd64\n",
    );
    let (status, body) = upload(
        &app,
        &cookie,
        &group,
        "libnvidia-container1_1.20.1-1_amd64.deb",
        b"fake deb",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["params"]["reason"], "deb_fields_unreadable");
    assert_eq!(
        app.host.count("dpkg-deb"),
        0,
        "a host program must not run without a host path"
    );
}

#[tokio::test]
async fn the_fields_of_a_real_deb_are_read_by_the_host_program() {
    let Some(bytes) = real_deb("libnvidia-container1", "1.20.1-1", "amd64") else {
        eprintln!(
            "dpkg-deb is not on the host-command PATH: the real-program path stays UNVERIFIED here"
        );
        return;
    };
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    host.set_dpkg_passthrough(true);
    let app = app(host.clone(), settings(), healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let cookie = setup_app(&app, "longpass99").await;
    let group = open_group(&app, &cookie).await;
    let (status, body) = upload(
        &app,
        &cookie,
        &group,
        "libnvidia-container1_1.20.1-1_amd64.deb",
        &bytes,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["file"]["package"], "libnvidia-container1");
    assert_eq!(body["file"]["version"], "1.20.1-1");
    // A file that only looks like a `.deb` is refused by the program itself.
    let (status, body) = upload(&app, &cookie, &group, "bogus.deb", b"not a deb at all").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["params"]["reason"], "deb_fields_unreadable");
}

async fn fill_group(app: &TestApp, cookie: &str, group: &str, toolkit_version: &str, driver: &str) {
    let run = format!(
        "#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 {driver}\"\n"
    );
    let (_s, _b) = upload(
        app,
        cookie,
        group,
        &format!("NVIDIA-Linux-x86_64-{driver}.run"),
        run.as_bytes(),
    )
    .await;
    for package in TOOLKIT_PACKAGES {
        app.host.set_dpkg_default(&format!(
            "Package: {package}\nVersion: {toolkit_version}-1\nArchitecture: amd64\n"
        ));
        let name = format!("{package}_{toolkit_version}-1_amd64.deb");
        let (_s, _b) = upload(app, cookie, group, &name, b"fake deb").await;
    }
}

#[tokio::test]
async fn group_summary_reports_the_toolkit_version() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let (status, body) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(status, 200);
    assert_eq!(body["toolkit_version"], "1.20.1");
    assert_eq!(body["missing"].as_array().unwrap().len(), 0);
    assert_eq!(body["below_security_floor"], false);
    assert_eq!(body["run"]["candidates"][0], "580.178.04");
}

#[tokio::test]
async fn upload_confirmation_warns_about_building_and_loading_the_installer() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "ready");
    assert_eq!(
        body["dialog"]["confirmation"]["warnings"],
        json!([
            {"key": "confirm.upload_trust", "params": {}},
            {"key": "original.skip_warning", "params": {}}
        ])
    );
}

#[tokio::test]
async fn upload_planning_requires_a_version_candidate_and_current_group() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.source_invalid.upload_version_required");
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "999.1.1", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["error"],
        "plan.source_invalid.upload_version_not_candidate"
    );
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": "0".repeat(32), "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "plan.source_invalid.upload_group_gone");
}

#[tokio::test]
async fn toolkit_floors_apply_to_upload_summaries_and_plans() {
    let (app, cookie) = setup().await;
    for (driver, toolkit, security_warning, too_old) in [
        ("614.99.99", "1.19.0", true, false),
        ("615.71.09", "1.19.0", true, true),
        ("615.71.09", "1.19.1", false, true),
        ("615.71.09", "1.20.0", false, true),
        ("615.71.09", "1.20.1", false, false),
        ("616.1.1", "1.21.0", false, false),
    ] {
        let group = open_group(&app, &cookie).await;
        fill_group(&app, &cookie, &group, toolkit, driver).await;
        let (status, summary) = get(&app, "/api/uploads/current", Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "{driver} / {toolkit}: {summary}");
        assert_eq!(
            summary["below_security_floor"], security_warning,
            "{driver} / {toolkit}: {summary}"
        );
        let group_problem = summary["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["key"] == "upload.group.toolkit_too_old");
        assert_eq!(group_problem, too_old, "{driver} / {toolkit}: {summary}");
        let (status, body) = post_plan(
            &app,
            &cookie,
            json!({"kind": "upload", "group_id": group, "version": driver, "gpus": []}),
        )
        .await;
        if too_old {
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert_eq!(body["error"], "plan.source_invalid.toolkit_too_old");
        } else {
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["result"], "ready");
            let id = body["plan_id"].as_str().unwrap();
            let (status, _) = post(
                &app,
                &format!("/api/plans/{id}/cancel"),
                json!({}),
                Some(&cookie),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
    }
}

#[tokio::test]
async fn upload_planning_checks_the_selected_candidate_from_a_multiple_version_summary() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.0", "610.57.04").await;
    let run = b"#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 610.57.04 / 615.71.09\"\n";
    let (status, _) = upload(&app, &cookie, &group, "multi.run", run).await;
    assert_eq!(status, StatusCode::OK);
    let (_, summary) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(summary["run"]["candidates"].as_array().unwrap().len(), 2);
    assert!(!summary["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["key"] == "upload.group.toolkit_too_old"));
    for (driver, reject) in [("610.57.04", false), ("615.71.09", true)] {
        let (status, body) = post_plan(
            &app,
            &cookie,
            json!({"kind": "upload", "group_id": group, "version": driver, "gpus": []}),
        )
        .await;
        if reject {
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert_eq!(body["error"], "plan.source_invalid.toolkit_too_old");
        } else {
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["result"], "ready");
        }
    }
}

#[tokio::test]
async fn a_new_group_replaces_the_old_unsubmitted_one() {
    let (app, cookie) = setup().await;
    let first = open_group(&app, &cookie).await;
    let _ = upload(&app, &cookie, &first, "a.run", RUN_BODY).await;
    let second = open_group(&app, &cookie).await;
    assert_ne!(first, second);
    let (status, body) = upload(&app, &cookie, &first, "a.run", RUN_BODY).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "upload.group_replaced");
    let (_, summary) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(summary["group_id"], second);
}

#[tokio::test]
async fn changing_the_uploaded_file_after_the_confirm_is_rejected() {
    // Submission recomputes all five file checksums and refuses a plan whose input changed.
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "ready");
    let plan_id = body["plan_id"].as_str().unwrap().to_string();
    // Change the installer bytes after planning.
    let replacement =
        "#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\nreplaced";
    let (status, body) = upload(
        &app,
        &cookie,
        &group,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        replacement.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "upload.busy");
    // Host-side modification bypasses HTTP locking; submission must still hash the actual bytes.
    app.host.put_file(
        &format!("/var/lib/nvm/manager/upload/{group}/driver.run"),
        replacement.as_bytes(),
    );
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "plan.changed");
    assert!(
        body["params"]["changed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "source"),
        "{body}"
    );
    let (status, summary) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["group_id"], group);
}

#[tokio::test]
async fn a_rejected_replacement_keeps_the_old_slot() {
    // A failed group.json rewrite must not destroy the accepted file.
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let run_path = format!("/var/lib/nvm/manager/upload/{group}/driver.run");
    let old_bytes = app.host.file(&run_path).unwrap();
    let (_, before) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    let old_sha = before["run"]["sha256"].as_str().unwrap().to_string();
    app.host.fail_write(&format!(
        "/var/lib/nvm/manager/upload/{group}/.group.json.tmp-"
    ));
    let replacement =
        "#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\nreplacement";
    let (status, body) = upload(
        &app,
        &cookie,
        &group,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        replacement.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "upload.rejected");
    assert_eq!(body["params"]["reason"], "write_failed");
    assert_eq!(app.host.file(&run_path).unwrap(), old_bytes);
    let (_, after) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(after["run"]["sha256"], old_sha);
}

#[tokio::test]
async fn a_failed_move_back_keeps_the_upload_inputs() {
    // When the unit never ran and the group cannot be renamed back, the five files must survive
    // instead of being deleted with the task dir.
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let plan_id = body["plan_id"].as_str().unwrap().to_string();
    app.host.preset(
        "systemd-run",
        &[],
        FakeOutput::code(1).with_stderr("systemd-run: failed to create unit"),
    );
    // Only a native EXEC failure proves the child never ran, so the unit must report
    // `Result=exit-code` with `ExecMainStatus=203`.
    app.host.preset(
        "systemctl",
        &["show"],
        FakeOutput::stdout(
            "LoadState=loaded\nActiveState=failed\nSubState=failed\nResult=exit-code\nExecMainStatus=203\n",
        ),
    );
    // The move-back rename is `tasks/<id>/in` -> `upload/<group>`.
    app.host.fail_rename_suffix("/in");
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "submit.failed");
    let (status, summary) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    assert_eq!(summary["group_id"], group);
    assert!(app
        .host
        .exists(&format!("/var/lib/nvm/manager/upload/{group}/driver.run")));
    assert!(!app
        .host
        .exists(&format!("/var/lib/nvm/manager/tasks/{group}")));
}

#[tokio::test]
async fn a_streamed_body_over_the_limit_is_rejected_without_leftovers() {
    // No Content-Length: the frame count is what the stream decides. One 1 GiB + 1 byte frame
    // crosses the limit before it is written.
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    let mut big = vec![0u8; (UPLOAD_FILE_LIMIT + 1) as usize];
    big[0] = b'#';
    let req = axum::http::Request::builder()
        .method(Method::PUT)
        .uri(format!("/api/uploads/{group}/files?name=big.run"))
        .header(header::HOST, "nas.example:8199")
        .header(header::ORIGIN, "http://nas.example:8199")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::COOKIE, format!("nvm_session={cookie}"))
        .header("x-nvm-token", token_for(&cookie).unwrap_or_default())
        .body(axum::body::Body::from(big))
        .unwrap();
    let response = tower::ServiceExt::oneshot(app.router.clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "upload.rejected");
    assert_eq!(body["params"]["reason"], "too_large");
    // No half file is left behind and the group still answers.
    let names = app
        .host
        .children(&format!("/var/lib/nvm/manager/upload/{group}"));
    assert!(
        !names.iter().any(|n| n.starts_with(".incoming-")),
        "{names:?}"
    );
    let (status, _) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn while_a_receive_is_open_the_group_and_submit_are_busy() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let plan_id = body["plan_id"].as_str().unwrap().to_string();
    // A file body is being received: the product's own state flag.
    app.ctx.upload.inner.lock().unwrap().receiving = true;
    let (status, body) = post(&app, "/api/uploads", json!({}), Some(&cookie)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "upload.busy");
    let (status, body) = upload(
        &app,
        &cookie,
        &group,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        RUN_BODY,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "upload.busy");
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "upload.busy");
    assert_eq!(app.host.count("systemd-run"), 0);
    app.ctx.upload.inner.lock().unwrap().receiving = false;
    let (status, _) = upload(
        &app,
        &cookie,
        &group,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        RUN_BODY,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!app.ctx.upload.receiving());
}

#[tokio::test]
async fn an_incomplete_group_lists_its_missing_packages() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    let run =
        "#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\n";
    let (status, _) = upload(
        &app,
        &cookie,
        &group,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        run.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    app.host.set_dpkg_default(
        "Package: libnvidia-container1\nVersion: 1.20.1-1\nArchitecture: amd64\n",
    );
    let (status, _) = upload(
        &app,
        &cookie,
        &group,
        "libnvidia-container1_1.20.1-1_amd64.deb",
        b"fake deb",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "plan.source_invalid.upload_incomplete");
    let missing: Vec<String> = body["params"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(missing.len(), 3, "{body}");
    assert!(!missing.contains(&"libnvidia-container1".to_string()));
}

#[tokio::test]
async fn a_group_with_mixed_toolkit_versions_is_a_problem() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    let run =
        "#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\n";
    let (status, _) = upload(
        &app,
        &cookie,
        &group,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        run.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for (i, package) in TOOLKIT_PACKAGES.iter().enumerate() {
        let version = if i == 0 { "1.20.1-1" } else { "1.20.0-1" };
        app.host.set_dpkg_default(&format!(
            "Package: {package}\nVersion: {version}\nArchitecture: amd64\n"
        ));
        let (status, _) = upload(
            &app,
            &cookie,
            &group,
            &format!("{package}_{version}_amd64.deb"),
            b"fake deb",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (_, summary) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert!(summary["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["key"] == "upload.group.versions_differ"));
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "plan.source_invalid.upload_problem");
}

#[tokio::test]
async fn a_label_without_a_version_takes_the_manual_one() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    let run = "#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64\"\n";
    let (status, body) = upload(&app, &cookie, &group, "manual.run", run.as_bytes()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["file"]["candidates"].as_array().unwrap().len(), 0);
    for package in TOOLKIT_PACKAGES {
        app.host.set_dpkg_default(&format!(
            "Package: {package}\nVersion: 1.20.1-1\nArchitecture: amd64\n"
        ));
        let (status, _) = upload(
            &app,
            &cookie,
            &group,
            &format!("{package}_1.20.1-1_amd64.deb"),
            b"fake deb",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "5.x", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "plan.source_invalid.version_format");
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "550.163.01", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "ready");
    assert_eq!(body["dialog"]["version"], "550.163.01");
    assert_eq!(
        body["dialog"]["confirmation"]["how"]["key"],
        "confirm.how.upload"
    );
}

#[tokio::test]
async fn two_contents_of_the_same_version_become_two_rows() {
    // The same version from two different uploads becomes two rows with different fingerprints; a
    // local plan can be started from either.
    let (app, cookie) = setup().await;
    let mut rows = Vec::new();
    for (i, artifact) in [b"first driver".as_slice(), b"second driver!".as_slice()]
        .iter()
        .enumerate()
    {
        let group = open_group(&app, &cookie).await;
        let run = format!(
            "#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\ncontent {i}"
        );
        let (status, _) = upload(
            &app,
            &cookie,
            &group,
            "NVIDIA-Linux-x86_64-580.178.04.run",
            run.as_bytes(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        for package in TOOLKIT_PACKAGES {
            app.host.set_dpkg_default(&format!(
                "Package: {package}\nVersion: 1.20.1-1\nArchitecture: amd64\n"
            ));
            let (status, _) = upload(
                &app,
                &cookie,
                &group,
                &format!("{package}_1.20.1-1_amd64.deb"),
                b"fake deb",
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, body) = post_plan(
            &app,
            &cookie,
            json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let fingerprint = body["dialog"]["source"]["fingerprint"]
            .as_str()
            .unwrap()
            .to_string();
        // Hold the build container until the preset output is in place. An upload-source task
        // prepares the build environment too; the fake host carries an already prepared one,
        // because the pinned official image and its packages have no real bytes inside it.
        app.host.prepare_build_env();
        let gate = app.host.set_gate("docker", &["run"]);
        let (status, task) = post_task(&app, &cookie, &body["plan_id"], "skip").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        let id = task["task_id"].as_str().unwrap().to_string();
        // Place the preset build output while the runner waits, so the row is really delivered.
        put_build_output(&app.host, &id, "580.178.04", artifact);
        gate.release();
        app.host.wait_for_units(std::time::Duration::from_secs(5));
        // An upload-source task never downloads its own inputs — its driver and toolkit packages
        // are the uploaded ones. Its only outbound work would be the pinned build environment, and
        // this host hands that over already prepared, so no curl runs at all.
        assert_eq!(app.host.count("curl"), 0);
        let record: serde_json::Value = serde_json::from_str(
            &app.host
                .file_text(&format!("/var/lib/nvm/drivers/{id}-build/record.json"))
                .unwrap_or_else(|| panic!("row for {id}")),
        )
        .unwrap();
        assert_eq!(record["kind"], "upload");
        assert_eq!(record["fingerprint"], fingerprint.as_str());
        assert_eq!(record["sha256"], sha256_hex(artifact));
        rows.push((id, fingerprint));
    }
    assert_ne!(rows[0].1, rows[1].1);
    for (id, _) in &rows {
        let (status, body) = post_plan(
            &app,
            &cookie,
            json!({"kind": "local", "row_id": format!("{id}-build")}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["dialog"]["source"]["kind"], "local");
        assert!(!body["dialog"]["confirmation"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["key"] == "confirm.upload_trust"));
        let (status, task) = post_task(&app, &cookie, &body["plan_id"], "skip").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        app.host.wait_for_units(std::time::Duration::from_secs(5));
    }
}

#[tokio::test]
async fn an_exdev_on_the_task_directory_keeps_the_group() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    app.host.fail_rename_exdev(&group);
    let (status, body) = post_task(&app, &cookie, &body["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "submit.failed");
    assert_eq!(body["params"]["reason"]["key"], "submit.task_dir_failed");
    assert!(app.host.children("/var/lib/nvm/manager/state").is_empty());
    assert!(app.host.children("/var/lib/nvm/manager/tasks").is_empty());
    assert!(app
        .host
        .exists(&format!("/var/lib/nvm/manager/upload/{group}/driver.run")));
    let (_, summary) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(summary["group_id"], group);
    assert_eq!(app.host.count("systemd-run"), 0);
}

#[tokio::test]
async fn a_staged_group_survives_a_web_process_restart() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let paths = truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR);
    let found = truenas_nvidia_driver_manager::upload::staged_group(&app.host, &paths);
    assert_eq!(found.as_deref(), Some(group.as_str()));
    let state = truenas_nvidia_driver_manager::upload::UploadState::with_current(found);
    assert_eq!(state.current().as_deref(), Some(group.as_str()));
    assert!(!state.receiving());
}

#[tokio::test]
async fn a_successful_replacement_retires_the_old_protected_copy() {
    // A successful replacement must not leave protected recovery material behind; the next cleanup
    // would otherwise put the old file back over the accepted one. A copy that can be neither
    // unlinked nor renamed out of the recovery namespace is disarmed with the committed bytes.
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let dir = format!("/var/lib/nvm/manager/upload/{group}");
    let run_path = format!("{dir}/driver.run");
    app.host
        .fail_remove(&format!("{dir}/.driver.run.recovery-"));
    app.host.fail_rename_contains("/.driver.run.recovery-");
    let replacement =
        "#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\nreplacement";
    let (status, body) = upload(
        &app,
        &cookie,
        &group,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        replacement.as_bytes(),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    app.host.clear_failures();
    // The next cleanup (startup does the same) must not roll the file back.
    let paths = truenas_nvidia_driver_manager::store::Paths::new(CONTAINER_DATA_DIR);
    truenas_nvidia_driver_manager::store::clean_leftovers(&app.host, &paths).unwrap();
    assert_eq!(app.host.file(&run_path).unwrap(), replacement.as_bytes());
    let (_, summary) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(
        summary["run"]["sha256"],
        sha256_hex(replacement.as_bytes()),
        "{summary}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_upload_open_does_nothing_after_logout() {
    let (app, cookie) = setup().await;
    let token = token_for(&cookie).unwrap();
    let guard = app.ctx.process_lock.lock().await;
    let router = app.router.clone();
    let cookie_for_task = cookie.clone();
    let pending = tokio::spawn(async move {
        let request = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/api/uploads")
            .header(header::HOST, "nas.example:8199")
            .header(header::ORIGIN, "http://nas.example:8199")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, format!("nvm_session={cookie_for_task}"))
            .header("x-nvm-token", token)
            .body(axum::body::Body::from("{}"))
            .unwrap();
        tower::ServiceExt::oneshot(router, request).await.unwrap()
    });
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(!pending.is_finished(), "the upload open did not wait");
    let (status, _) = post(&app, "/api/logout", json!({}), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    drop(guard);
    let response = pending.await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "auth.required");
    assert!(app.ctx.upload.current().is_none());
    assert!(app.host.children("/var/lib/nvm/manager/upload").is_empty());
}

#[tokio::test]
async fn confirmed_upload_is_locked_until_cancel_and_can_be_planned_again() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let source = json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []});
    let (status, body) = post_plan(&app, &cookie, source.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["plan_id"].as_str().unwrap();
    assert!(app.host.exists(
        app.ctx
            .paths
            .upload_group(&group)
            .join("driver.run")
            .to_str()
            .unwrap()
    ));
    let (status, _) = post(&app, "/api/uploads", json!({}), Some(&cookie)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = upload(&app, &cookie, &group, "replacement.run", RUN_BODY).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = post(
        &app,
        &format!("/api/plans/{id}/cancel"),
        json!({}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = upload(&app, &cookie, &group, "replacement.run", RUN_BODY).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_plan(&app, &cookie, source).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn packages_index(versions: &[(&str, usize)]) -> String {
    let mut index = String::new();
    for (version, count) in versions {
        for name in TOOLKIT_PACKAGES.iter().take(*count) {
            index.push_str(&format!("Package: {name}\nArchitecture: amd64\nVersion: {version}\nFilename: ./{name}_{version}_amd64.deb\nSize: 8\nSHA256: {}\n\n", "a".repeat(64)));
        }
    }
    index
}

#[tokio::test]
async fn toolkit_download_version_uses_the_newest_complete_official_version() {
    let (app, cookie) = setup().await;
    app.host.set_curl(
        TOOLKIT_INDEX_URL,
        200,
        packages_index(&[("1.21.0-1", 4), ("1.22.0-1", 3)]).as_bytes(),
    );
    let (status, body) = get(&app, "/api/uploads/toolkit-version", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"version": "1.21.0-1", "fallback": false}));
}

#[tokio::test]
async fn toolkit_download_version_falls_back_when_index_cannot_be_validated() {
    let (app, cookie) = setup().await;
    for (status, index) in [
        (503, String::new()),
        (200, "unreadable".to_string()),
        (302, packages_index(&[("1.22.0-1", 4)])),
    ] {
        app.host
            .set_curl(TOOLKIT_INDEX_URL, status, index.as_bytes());
        let (status, body) = get(&app, "/api/uploads/toolkit-version", Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"version": "1.20.1-1", "fallback": true}));
    }
}

#[test]
fn restart_restores_a_group_without_a_stale_confirmation_lock() {
    use truenas_nvidia_driver_manager::upload::UploadState;
    let group = "a".repeat(32);
    let state = UploadState::with_current(Some(group.clone()));
    assert_eq!(state.current(), Some(group));
    assert!(!state.receiving());
}

#[tokio::test]
async fn reopening_upload_recovers_an_abandoned_confirmation_without_losing_files() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let source = json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []});
    let (status, plan) = post_plan(&app, &cookie, source.clone()).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    // A new browser dialog no longer knows the abandoned plan id.
    let (status, _) = post(
        &app,
        "/api/uploads/current/cancel-plan",
        json!({}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, summary) = get(&app, "/api/uploads/current", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["group_id"], group);
    assert!(summary["missing"].as_array().unwrap().is_empty());
    let (status, body) = post_task(&app, &cookie, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "plan.used");
    let (status, next) = post_plan(&app, &cookie, source).await;
    assert_eq!(status, StatusCode::OK, "{next}");
    // An old dialog returning later must never unlock the newly confirmed bytes.
    let old_id = plan["plan_id"].as_str().unwrap();
    let (status, _) = post(
        &app,
        &format!("/api/plans/{old_id}/cancel"),
        json!({}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = upload(&app, &cookie, &group, "replacement.run", RUN_BODY).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn recovery_without_auth_cannot_unlock_a_confirmation() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let (status, plan) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let (status, _) = post(&app, "/api/uploads/current/cancel-plan", json!({}), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = upload(&app, &cookie, &group, "replacement.run", RUN_BODY).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn restart_recovers_the_original_group_json_and_all_five_files_unlocked() {
    use std::sync::Arc;
    use truenas_nvidia_driver_manager::upload::{self, UploadState};
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let group_path = app.ctx.paths.upload_group(&group);
    let old_record = app
        .host
        .file(group_path.join("group.json").to_str().unwrap())
        .unwrap();
    let original_hash = upload::hash_run(&app.host, &app.ctx.paths, &group).unwrap();
    let (status, body) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        app.host
            .file(group_path.join("group.json").to_str().unwrap()),
        Some(old_record)
    );
    // Startup reconstructs only this process-local state; the on-disk format is unchanged.
    let recovered = Arc::new(UploadState::with_current(upload::staged_group(
        &app.host,
        &app.ctx.paths,
    )));
    assert_eq!(recovered.current().as_deref(), Some(group.as_str()));
    let summary = upload::summary(&app.host, &app.ctx.paths, &group);
    assert!(summary.missing.is_empty());
    assert_eq!(
        upload::hash_debs(&app.host, &app.ctx.paths, &group)
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        upload::hash_run(&app.host, &app.ctx.paths, &group).unwrap(),
        original_hash
    );
    // Real replacement uses the reconstructed state, proving that no stale lock survived.
    let result = upload::receive(
        Arc::new(app.host.clone()),
        app.ctx.paths.clone(),
        app.ctx.startup.host_data_dir.clone(),
        group,
        "after-restart.run".into(),
        Some(RUN_BODY.len() as u64),
        axum::body::Body::from(RUN_BODY),
        recovered,
        &tokio::sync::Mutex::new(()),
        || true,
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn toolkit_index_download_uses_the_confirmed_host_data_mapping() {
    let (app, cookie) = setup().await;
    app.host.set_curl(
        TOOLKIT_INDEX_URL,
        200,
        packages_index(&[("1.21.0-1", 4)]).as_bytes(),
    );
    let (status, body) = get(&app, "/api/uploads/toolkit-version", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], "1.21.0-1");
    assert_eq!(body["fallback"], false);
    let commands = app.host.commands();
    let command = commands.iter().find(|cmd| cmd.program == "curl").unwrap();
    for flag in ["--output", "--dump-header"] {
        let path = command
            .args
            .windows(2)
            .find(|pair| pair[0] == flag)
            .unwrap()[1]
            .to_string();
        assert!(
            path.starts_with(&format!("{HOST_DATA_DIR}/manager/.toolkit-index-")),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_receive_in_progress_cannot_create_a_confirmation_lock() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    app.ctx.upload.inner.lock().unwrap().receiving = true;
    let source = json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []});
    let (status, body) = post_plan(&app, &cookie, source.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "upload.busy");
    assert!(app.ctx.plan.lock().unwrap().0.is_none());
    assert!(!app.ctx.upload.inner.lock().unwrap().locked);
    app.ctx.upload.inner.lock().unwrap().receiving = false;
    let (status, body) = post_plan(&app, &cookie, source).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "ready");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reading_upload_fields_keeps_confirmation_and_group_replacement_busy() {
    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let source = json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []});
    let gate = app.host.set_gate_output(
        "dpkg-deb",
        &["--field"],
        FakeOutput::stdout(
            "Package: libnvidia-container1\nVersion: 1.20.1-1\nArchitecture: amd64\n",
        ),
    );
    let _release = ReleaseGate(gate.clone());
    let pending = {
        let (app, cookie, group) = (app.clone(), cookie.clone(), group.clone());
        tokio::spawn(async move {
            upload(&app, &cookie, &group, "replacement.deb", b"replacement deb").await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !gate.blocked() {
            assert!(
                !pending.is_finished(),
                "upload never reached the field read"
            );
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("upload did not reach the field read");

    let (status, body) = post_plan(&app, &cookie, source.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "upload.busy");
    assert!(app.ctx.upload.receiving());
    assert!(app.ctx.plan.lock().unwrap().0.is_none());
    assert!(!app.ctx.upload.inner.lock().unwrap().locked);
    let (status, body) = post(&app, "/api/uploads", json!({}), Some(&cookie)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "upload.busy");
    assert_eq!(app.ctx.upload.current().as_deref(), Some(group.as_str()));

    gate.release();
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!app.ctx.upload.receiving());
    let (status, body) = post_plan(&app, &cookie, source).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admitted_upload_cannot_replace_files_locked_before_commit() {
    use truenas_nvidia_driver_manager::upload;

    let (app, cookie) = setup().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.178.04").await;
    let group_path = app.ctx.paths.upload_group(&group);
    let run_before = app
        .host
        .file(group_path.join("driver.run").to_str().unwrap());
    let record_before = app
        .host
        .file(group_path.join("group.json").to_str().unwrap());
    let run_hash_before = upload::hash_run(&app.host, &app.ctx.paths, &group).unwrap();
    let deb_hashes_before = upload::hash_debs(&app.host, &app.ctx.paths, &group).unwrap();
    let (status, plan) = post_plan(
        &app,
        &cookie,
        json!({"kind": "upload", "group_id": group, "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    // Admit the request before restoring the real plan's lock at the read barrier.
    // This isolates the commit check from both the entry check and receiving lifetime.
    {
        let _lock = app.ctx.process_lock.lock().await;
        app.ctx.upload.lock_group(None);
    }
    let gate = app.host.set_gate_output(
        "dpkg-deb",
        &["--field"],
        FakeOutput::stdout(
            "Package: libnvidia-container1\nVersion: 1.20.1-1\nArchitecture: amd64\n",
        ),
    );
    let _release = ReleaseGate(gate.clone());
    let pending = {
        let (app, cookie, group) = (app.clone(), cookie.clone(), group.clone());
        tokio::spawn(async move {
            upload(&app, &cookie, &group, "replacement.deb", b"replacement deb").await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !gate.blocked() {
            assert!(
                !pending.is_finished(),
                "upload never reached the field read"
            );
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("upload did not reach the field read");
    assert!(app
        .host
        .children(group_path.to_str().unwrap())
        .iter()
        .any(|name| name.starts_with(".incoming-")));
    {
        let _lock = app.ctx.process_lock.lock().await;
        app.ctx.upload.lock_group(Some(&group));
    }
    gate.release();
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "upload.busy");
    assert!(!app.ctx.upload.receiving());
    assert_eq!(body["params"]["file_name"], "replacement.deb");
    assert_eq!(body["params"]["kind"], "deb");
    assert_eq!(body["params"]["package"], "libnvidia-container1");
    assert_eq!(app.ctx.upload.current().as_deref(), Some(group.as_str()));
    assert!(app.ctx.upload.inner.lock().unwrap().locked);
    assert_eq!(
        app.host
            .file(group_path.join("driver.run").to_str().unwrap()),
        run_before
    );
    assert_eq!(
        app.host
            .file(group_path.join("group.json").to_str().unwrap()),
        record_before
    );
    assert_eq!(
        upload::hash_run(&app.host, &app.ctx.paths, &group).unwrap(),
        run_hash_before
    );
    assert_eq!(
        upload::hash_debs(&app.host, &app.ctx.paths, &group).unwrap(),
        deb_hashes_before
    );
    assert!(!app
        .host
        .children(group_path.to_str().unwrap())
        .iter()
        .any(|name| name.starts_with(".incoming-")));

    let (status, body) = post_task(&app, &cookie, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let task_id = body["task_id"].as_str().unwrap();
    assert_eq!(
        app.host.file(
            app.ctx
                .paths
                .task_dir(task_id)
                .join("in/driver.run")
                .to_str()
                .unwrap()
        ),
        run_before
    );
    assert_eq!(app.host.count("systemd-run"), 1);
}

#[tokio::test]
async fn an_old_580_open_upload_still_reaches_isolated_build_submission() {
    let (app, cookie) = setup().await;
    fake_host::prime_kernel_headers(&app.host, KERNEL);
    app.host.add_gpu("0000:01:00.0", 0x1e37, Some("nvidia"));
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group, "1.20.1", "580.119.02").await;
    let (status, plan) = post_plan(
        &app,
        &cookie,
        json!({
            "kind": "upload", "group_id": group, "version": "580.119.02", "gpus": ["0000:01:00.0"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["result"], "ready");
    let (status, task) = post_task(&app, &cookie, &plan["plan_id"], "skip").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id = task["task_id"].as_str().unwrap();
    let state: serde_json::Value = serde_json::from_slice(
        &app.host
            .file(&format!("/var/lib/nvm/manager/state/{id}.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(state["plan"]["kernel_module_type"], "open");
    assert_eq!(state["plan"]["source"]["kind"], "upload");
    assert_eq!(app.host.count("systemd-run"), 1);
}

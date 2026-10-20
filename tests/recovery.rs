//! Recovery after a failed publish or submission. A failure path can leave the previous bytes under
//! a temporary name that normal cleanup or the next start would delete. These tests run both the
//! negative lifecycle (the fault persists: the only copy must still be there after reconcile,
//! cleanup and a simulated restart) and the positive one (the fault clears: the bytes are actually
//! restored and read back).

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::CONTAINER_DATA_DIR;
use truenas_nvidia_driver_manager::store::{self, Paths, Validate};
use truenas_nvidia_driver_manager::task_state::{Phase, Status};
use truenas_nvidia_driver_manager::upload::UploadState;

const RUN_BODY: &[u8] =
    b"#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\nrest";

fn paths() -> Paths {
    Paths::new(CONTAINER_DATA_DIR)
}

fn names_with(host: &FakeHost, dir: &str, needle: &str) -> Vec<String> {
    host.children(dir)
        .into_iter()
        .filter(|n| n.contains(needle))
        .collect()
}

/// Positive helper: a state file that parses but does not validate, as the reconcile table's
/// `Invalid` row describes.
fn invalid_state(host: &FakeHost, id: &str) -> Vec<u8> {
    let p = paths();
    let mut state = truenas_nvidia_driver_manager::task_state::new_state(
        id,
        1,
        sample_plan(truenas_nvidia_driver_manager::task_state::PlanSource::Official),
        Phase::Write,
    );
    // SUCCEEDED without the success-check readings fails `validate`.
    state.status = Status::Succeeded;
    state.finished_at = Some(2);
    truenas_nvidia_driver_manager::task_state::write(host, &p, &state).unwrap();
    assert!(state.validate().is_err());
    host.file(&format!("/var/lib/nvm/manager/state/{id}.json"))
        .unwrap()
}

async fn setup_upload_app() -> (TestApp, String) {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    signed_in_app(host).await
}

async fn open_group(app: &TestApp, cookie: &str) -> String {
    let (status, body) = post(app, "/api/uploads", json!({}), Some(cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["group_id"].as_str().unwrap().to_string()
}

async fn fill_group(app: &TestApp, cookie: &str, group: &str) {
    let (status, body) = request_raw(
        app,
        axum::http::Method::PUT,
        &format!("/api/uploads/{group}/files?name=NVIDIA-Linux-x86_64-580.178.04.run"),
        RUN_BODY.to_vec(),
        "application/octet-stream",
        Some(cookie),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    for package in truenas_nvidia_driver_manager::codes::TOOLKIT_PACKAGES {
        app.host.set_dpkg_default(&format!(
            "Package: {package}\nVersion: 1.20.1-1\nArchitecture: amd64\n"
        ));
        let name = format!("{package}_1.20.1-1_amd64.deb");
        let (status, body) = request_raw(
            app,
            axum::http::Method::PUT,
            &format!("/api/uploads/{group}/files?name={name}"),
            b"fake deb".to_vec(),
            "application/octet-stream",
            Some(cookie),
        )
        .await;
        assert_eq!(status, 200, "{body}");
    }
}

/// A submission whose unit never ran and whose move-back fails must leave the only copy of the
/// upload in the kept task directory, and a reconcile must not delete it while the rollback cannot
/// be completed. Once the fault clears the same reconcile puts the five files back and removes the
/// task directory.
#[tokio::test]
async fn failed_move_back_input_survives_cleanup_and_is_recovered() {
    let (app, cookie) = setup_upload_app().await;
    let group = open_group(&app, &cookie).await;
    fill_group(&app, &cookie, &group).await;
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
    // The move-back rename (`…/in` -> the group) fails, and every write into the group directory is
    // refused, so the copy fallback fails too.
    app.host.fail_rename_suffix("/in");
    app.host
        .fail_write(&format!("/var/lib/nvm/manager/upload/{group}/"));
    let (status, body) = post_task(&app, &cookie, &plan_id, "skip").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "submit.failed");

    let task = app.host.children("/var/lib/nvm/manager/tasks")[0].clone();
    let input = format!("/var/lib/nvm/manager/tasks/{task}/in/driver.run");
    assert_eq!(app.host.file(&input).unwrap(), RUN_BODY);
    assert!(
        app.host.exists(&format!(
            "/var/lib/nvm/manager/tasks/{task}/.rollback-upload"
        )),
        "the destination of the pending rollback must be recorded"
    );

    // Negative lifecycle: the fault persists. Reconcile writes INTERRUPTED but must keep the only
    // copy of the upload, and the startup cleanup must not delete it either.
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&app.host, &paths());
    assert_eq!(
        reconciled.state_of(&task).unwrap().status,
        Some(Status::Interrupted)
    );
    assert_eq!(
        app.host.file(&input).unwrap(),
        RUN_BODY,
        "reconcile deleted the only upload input"
    );
    assert!(app
        .host
        .exists(&format!("/var/lib/nvm/manager/tasks/{task}")));
    let _ = store::clean_leftovers(&app.host, &paths());
    assert_eq!(
        app.host.file(&input).unwrap(),
        RUN_BODY,
        "the startup cleanup deleted the only upload input"
    );

    // A restart is a fresh process over the same data directory; the marker and the staged files
    // are all it has, and the group is still the current one.
    assert_eq!(
        truenas_nvidia_driver_manager::upload::staged_group(&app.host, &paths()).as_deref(),
        Some(group.as_str())
    );

    // Positive lifecycle: the fault clears, the same reconcile recovers the group and only then
    // deletes the task directory.
    app.host.clear_failures();
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&app.host, &paths());
    assert_eq!(
        reconciled.state_of(&task).unwrap().status,
        Some(Status::Interrupted)
    );
    assert_eq!(
        app.host
            .file(&format!("/var/lib/nvm/manager/upload/{group}/driver.run"))
            .unwrap(),
        RUN_BODY,
        "the staged upload was not put back into its group"
    );
    for package in truenas_nvidia_driver_manager::codes::TOOLKIT_PACKAGES {
        assert!(
            app.host.exists(&format!(
                "/var/lib/nvm/manager/upload/{group}/toolkit/{package}.deb"
            )),
            "the tool package {package} was not restored"
        );
    }
    assert!(
        !app.host
            .exists(&format!("/var/lib/nvm/manager/tasks/{task}")),
        "the task directory survived a completed rollback"
    );
}

#[test]
fn an_unreadable_state_whose_rename_is_refused_is_not_overwritten() {
    let host = default_host();
    let id = "20261003T010203Z-1234abcd";
    let before = invalid_state(&host, id);
    host.fail_rename_suffix(".json");
    host.fail_write("/var/lib/nvm/manager/state/");
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    let task = reconciled.state_of(id).unwrap();
    assert_eq!(
        task.status, None,
        "a state whose original could not be kept must stay UNKNOWN"
    );
    assert_eq!(task.state_read_error.as_deref(), Some("invalid"));
    assert_eq!(
        host.file(&format!("/var/lib/nvm/manager/state/{id}.json"))
            .unwrap(),
        before,
        "the only copy of the unreadable state was overwritten"
    );
    assert!(!host.exists(&format!("/var/lib/nvm/manager/state/{id}.json.unreadable")));
}

#[test]
fn a_refused_rename_keeps_the_original_through_a_verified_copy() {
    let host = default_host();
    let id = "20261003T010203Z-1234abcd";
    let before = invalid_state(&host, id);
    // The rename aside is refused; the verified copy to the sidecar name is allowed, so the
    // original bytes are kept before the fresh record.
    host.fail_rename_suffix(".json");
    let reconciled = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
    let task = reconciled.state_of(id).unwrap();
    assert_eq!(task.status, Some(Status::Interrupted));
    assert_eq!(
        host.file(&format!("/var/lib/nvm/manager/state/{id}.json.unreadable"))
            .unwrap(),
        before,
        "the sidecar copy does not hold the original bytes"
    );
    let note = task.state_note.as_ref().unwrap();
    assert_eq!(
        note.kept_as.as_deref(),
        Some("20261003T010203Z-1234abcd.json.unreadable")
    );
    let fresh = host
        .file(&format!("/var/lib/nvm/manager/state/{id}.json"))
        .unwrap();
    assert_ne!(fresh, before);
    assert!(String::from_utf8(fresh).unwrap().contains("INTERRUPTED"));
}

/// A publish whose directory sync and both rollback renames fail must keep the old bytes under the
/// protected recovery name; the startup cleanup must not delete them, and once the fault clears the
/// same cleanup puts them back.
#[test]
fn a_refused_publish_rollback_keeps_the_old_bytes_until_recovery() {
    let host = default_host();
    let p = paths();
    let cred = p.credentials().to_string_lossy().into_owned();
    store::write_atomic(&host, &p.credentials(), b"old-complete", 0o600).unwrap();
    host.fail_fsync_dir("/var/lib/nvm/manager/secrets");
    host.fail_rename_to_suffix_after("web-credentials.json", 1);
    assert!(store::write_atomic(&host, &p.credentials(), b"new-complete", 0o600).is_err());
    assert_eq!(host.file(&cred).unwrap(), b"new-complete");
    let protected = names_with(&host, "/var/lib/nvm/manager/secrets", ".recovery-");
    assert_eq!(protected.len(), 1, "{protected:?}");
    let recovery = format!("/var/lib/nvm/manager/secrets/{}", protected[0]);
    assert_eq!(host.file(&recovery).unwrap(), b"old-complete");
    assert!(
        names_with(&host, "/var/lib/nvm/manager/secrets", ".tmp-").is_empty(),
        "the recovery material must not stay under a name the cleanup deletes"
    );

    // Negative lifecycle: the fault persists. The cleanup reports the refusal and the material
    // survives.
    assert!(store::clean_leftovers(&host, &p).is_err());
    assert_eq!(host.file(&recovery).unwrap(), b"old-complete");

    // Positive lifecycle: the fault clears and the cleanup recovers.
    host.clear_failures();
    store::clean_leftovers(&host, &p).unwrap();
    assert_eq!(host.file(&cred).unwrap(), b"old-complete");
    assert!(names_with(&host, "/var/lib/nvm/manager/secrets", ".recovery-").is_empty());
}

/// The next publish of the same target must put the pending old bytes back before it snapshots
/// them, so a successful publish never leaves a stale recovery copy that a later startup would
/// restore over it.
#[test]
fn a_new_publish_flushes_pending_recovery_first() {
    let host = default_host();
    let p = paths();
    let cred = p.credentials().to_string_lossy().into_owned();
    store::write_atomic(&host, &p.credentials(), b"old-complete", 0o600).unwrap();
    host.fail_fsync_dir("/var/lib/nvm/manager/secrets");
    host.fail_rename_to_suffix_after("web-credentials.json", 1);
    assert!(store::write_atomic(&host, &p.credentials(), b"new-complete", 0o600).is_err());
    host.clear_failures();
    store::write_atomic(&host, &p.credentials(), b"third-complete", 0o600).unwrap();
    assert_eq!(host.file(&cred).unwrap(), b"third-complete");
    assert!(names_with(&host, "/var/lib/nvm/manager/secrets", ".recovery-").is_empty());
    store::clean_leftovers(&host, &p).unwrap();
    assert_eq!(host.file(&cred).unwrap(), b"third-complete");
}

async fn receive_run(
    host: &FakeHost,
    group: &str,
    st: &Arc<UploadState>,
    name: &str,
    bytes: &'static [u8],
) -> Result<(), String> {
    let lock = tokio::sync::Mutex::new(());
    truenas_nvidia_driver_manager::upload::receive(
        Arc::new(host.clone()),
        paths(),
        Some(HOST_DATA_DIR.into()),
        group.to_string(),
        name.to_string(),
        None,
        Body::from(bytes),
        st.clone(),
        &lock,
        || true,
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("{e:?}"))
}

/// A slot replacement whose `group.json` write fails and whose rename-based restore paths are
/// refused must still leave the accepted bytes at the fixed slot: the in-place restore runs before
/// the protected copy is kept.
#[tokio::test]
async fn a_refused_slot_restore_keeps_the_old_upload_in_place() {
    let host = default_host();
    let p = paths();
    let st = Arc::new(UploadState::new());
    let group = truenas_nvidia_driver_manager::upload::open_group(&host, &p, &st).unwrap();
    let run =
        b"#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\nold";
    receive_run(
        &host,
        &group,
        &st,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        run,
    )
    .await
    .unwrap();
    let dir = format!("/var/lib/nvm/manager/upload/{group}");
    let old_sha = truenas_nvidia_driver_manager::upload::summary(&host, &p, &group)
        .run
        .unwrap()
        .sha256;

    host.fail_write(&format!("{dir}/.group.json.tmp-"));
    // The replacement rename onto `driver.run` is the first one allowed; the rename-based slot
    // restores are refused afterwards. The in-place restore still puts the accepted bytes back.
    host.fail_rename_to_suffix_after("driver.run", 1);
    let replacement =
        b"#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\nnew";
    let failed = receive_run(
        &host,
        &group,
        &st,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        replacement,
    )
    .await;
    assert!(
        failed.unwrap_err().contains("write_failed"),
        "the rejected replacement must be reported as write_failed"
    );
    assert_eq!(
        host.file(&format!("{dir}/driver.run")).unwrap(),
        run,
        "the stored slot must be the accepted bytes again"
    );
    assert!(
        names_with(&host, &dir, ".driver.run.recovery-").is_empty(),
        "a restored slot keeps no protected material"
    );
    // The accepted facts are not falsified: `group.json` still describes the old run and the old
    // file bytes are the only ones kept.
    assert_eq!(
        truenas_nvidia_driver_manager::upload::summary(&host, &p, &group)
            .run
            .unwrap()
            .sha256,
        old_sha
    );
    assert_eq!(host.file(&format!("{dir}/driver.run")).unwrap(), run);

    // Once the fault clears, the slot is replaced normally again.
    host.clear_failures();
    receive_run(
        &host,
        &group,
        &st,
        "NVIDIA-Linux-x86_64-580.178.04.run",
        replacement,
    )
    .await
    .unwrap();
    assert_eq!(
        host.file(&format!("{dir}/driver.run")).unwrap(),
        replacement
    );
}

#[test]
fn unreadable_correction_accounts_survive_reconcile_copy_and_failed_copy() {
    for phase in [Phase::AlignModules, Phase::RestoreAlignModules] {
        for fail_copy in [false, true] {
            let host = default_host();
            let id = "20261003T010203Z-1234abcd";
            invalid_state(&host, id);
            let path = paths().state(id);
            let mut raw: serde_json::Value =
                serde_json::from_slice(&host.file(path.to_str().unwrap()).unwrap()).unwrap();
            raw["phase"] = json!(phase.as_str());
            raw["replace"] = json!([{
                "pass": "update", "unloaded": [], "reloaded": [],
                "module_correction": partial_module_correction(),
            }]);
            let before = serde_json::to_vec(&raw).unwrap();
            host.put_file(path.to_str().unwrap(), before.clone());
            host.fail_rename_suffix(".json");
            if fail_copy {
                host.fail_write("/var/lib/nvm/manager/state/");
            }
            for _ in 0..2 {
                let result = truenas_nvidia_driver_manager::tasks::reconcile(&host, &paths());
                assert_eq!(
                    result.state_of(id).unwrap().status,
                    if fail_copy {
                        None
                    } else {
                        Some(Status::Interrupted)
                    }
                );
                let protected = if fail_copy {
                    path.clone()
                } else {
                    path.with_extension("json.unreadable")
                };
                assert_eq!(host.file(protected.to_str().unwrap()).unwrap(), before);
            }
            assert_eq!(host.count("systemd-run"), 0);
        }
    }
}

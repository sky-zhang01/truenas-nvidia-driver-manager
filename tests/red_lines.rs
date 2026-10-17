//! The two forbidden host commands.

mod common;

use common::*;
use serde_json::json;

async fn setup() -> (TestApp, String) {
    let host = default_host();
    let base = "/usr/src/linux-headers-truenas-production-amd64/include";
    host.put_text(
        &format!("{base}/linux/mm.h"),
        "static inline int follow_pfnmap_start(struct follow_pfnmap_args *args);\n",
    );
    host.put_text(
        &format!("{base}/drm/drm_mode_config.h"),
        "struct drm_mode_config_funcs { void (*atomic_state_free)(void); };\n",
    );
    host.put_text(
        &format!("{base}/linux/pci.h"),
        "int pci_resize_resource(struct pci_dev *dev, int resno, int size,\n                        u32 exclude_bars);\n",
    );
    host.put_text("/data/manifest.json", r#"{"version":"25.10.7"}"#);
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "longpass99").await;
    (app, cookie)
}

#[tokio::test]
async fn no_docker_update_and_no_collect_in_a_real_task() {
    let (app, cookie) = setup().await;
    // Remove the canonical file after planning so the runner stops while saving the original, after
    // issuing host commands.
    let (status, plan) = post_plan(
        &app,
        &cookie,
        json!({"kind": "official", "version": "580.178.04", "gpus": []}),
    )
    .await;
    assert_eq!(status, 200, "{plan}");
    assert_eq!(plan["result"], "ready", "{plan}");
    let (status, task) = post_task(&app, &cookie, &plan["plan_id"], "save").await;
    assert_eq!(status, 202, "{task}");
    app.host.wait_for_units(std::time::Duration::from_secs(5));
    app.host.assert_red_lines();
    let systemd_run = app
        .host
        .argv_log()
        .into_iter()
        .find(|argv| argv[0] == "systemd-run")
        .expect("systemd-run was called");
    assert!(systemd_run
        .iter()
        .any(|a| a.starts_with("--unit=nvm-task-")));
    assert!(!systemd_run.iter().any(|a| a == "--collect"));
    assert!(!systemd_run.iter().any(|a| a.contains("CollectMode")));
    for argv in app.host.argv_log() {
        if argv[0].contains("docker") {
            assert!(
                !(argv.contains(&"update".to_string())
                    && argv.iter().any(|a| a.contains("nvidia")))
            );
        }
    }
}

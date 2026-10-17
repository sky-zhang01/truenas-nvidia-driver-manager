//! Login status `setup.problems` closed key set and the single startup failure synthesis. Twelve
//! distinct startup-check failures each produce one `startup.failed` problem whose `items` keep the
//! internal item keys (`startup.pid_host`, `startup.runtime`, `startup.data_mapping`), both for the
//! public and the signed-in login status; a passed check set leaves `setup` empty.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::CONTAINER_DATA_DIR;
use truenas_nvidia_driver_manager::startup;
use truenas_nvidia_driver_manager::store::Paths;

/// The login status `setup.problems[].key` closed set.
const SETUP_PROBLEM_KEYS: [&str; 3] = ["setting.unknown", "setting.invalid", "startup.failed"];
/// The internal startup failure item keys.
const STARTUP_ITEM_KEYS: [&str; 3] = [
    "startup.pid_host",
    "startup.runtime",
    "startup.data_mapping",
];

fn problem_keys(body: &serde_json::Value) -> Vec<String> {
    body["setup"]["problems"]
        .as_array()
        .expect("setup.problems")
        .iter()
        .map(|p| p["key"].as_str().unwrap_or("<missing>").to_string())
        .collect()
}

fn startup_item_keys(body: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for problem in body["setup"]["problems"]
        .as_array()
        .expect("setup.problems")
    {
        if problem["key"] != "startup.failed" {
            continue;
        }
        for item in problem["params"]["items"].as_array().expect("items") {
            out.push(item["key"].as_str().unwrap_or("<missing>").to_string());
        }
    }
    out
}

fn assert_required_shape(body: &serde_json::Value, label: &str) {
    let keys = problem_keys(body);
    assert!(!keys.is_empty(), "{label}: no setup problem: {body}");
    for key in &keys {
        assert!(
            SETUP_PROBLEM_KEYS.contains(&key.as_str()),
            "{label}: unapproved setup key {key}: {body}"
        );
    }
    assert!(
        !keys
            .iter()
            .any(|k| k.starts_with("startup.") && k != "startup.failed"),
        "{label}: an unapproved outer startup key leaked: {body}"
    );
    for key in startup_item_keys(body) {
        assert!(
            STARTUP_ITEM_KEYS.contains(&key.as_str()),
            "{label}: unapproved startup failure item key {key}: {body}"
        );
    }
}

async fn logged_in_login_status(app: &TestApp) -> (StatusCode, serde_json::Value, String) {
    let _ = get(app, "/api/login", None).await;
    let cookie = login_changed(app, "hunter12", "changed123").await;
    let (status, body) = get(app, "/api/login", Some(&cookie)).await;
    (status, body, cookie)
}

fn legitimate_snapshot() -> truenas_nvidia_driver_manager::plan::Snapshot {
    truenas_nvidia_driver_manager::plan::Snapshot {
        id: String::new(),
        source: truenas_nvidia_driver_manager::task_state::PlanSource::Official,
        version: "580.178.04".into(),
        kernel: KERNEL.into(),
        truenas_version: TRUENAS_VERSION.into(),
        original: truenas_nvidia_driver_manager::task_state::OriginalChoice::Skip,
        original_saved: false,
        save_available: true,
        link: truenas_nvidia_driver_manager::facts::LinkReading::Ok,
        gpus: vec![],
        kernel_module_type: None,
        upload_group: None,
        local_row: None,
    }
}

#[tokio::test]
async fn every_startup_counterexample_has_the_required_login_status_key_set() {
    for case in [
        "pid", "hostname", "nsenter", "inspect", "runtime", "mount0", "mount2", "under", "comma",
        "quote", "control", "mismatch",
    ] {
        let host = default_host();
        prime_canonical(&host, "580.173.02");
        host.set_docker_inspect(&inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]));
        match case {
            "pid" => host.set_same_namespace(true),
            "hostname" => host.set_hostname_container_id("zzz"),
            "nsenter" => host.preset(
                "docker",
                &["inspect"],
                FakeOutput::code(1).with_stderr("nsenter: approved positive"),
            ),
            "inspect" => host.preset(
                "docker",
                &["inspect"],
                FakeOutput::code(2).with_stderr("docker independently refused"),
            ),
            "runtime" => {
                host.set_docker_inspect(&inspect_json("nvidia", &[(HOST_DATA_DIR, "/var/lib/nvm")]))
            }
            "mount0" => host.set_docker_inspect(&inspect_json("runc", &[])),
            "mount2" => host.set_docker_inspect(&inspect_json(
                "runc",
                &[
                    (HOST_DATA_DIR, "/var/lib/nvm"),
                    ("/mnt/<pool>/second", "/var/lib/nvm"),
                ],
            )),
            "under" => host.set_docker_inspect(&inspect_json(
                "runc",
                &[
                    (HOST_DATA_DIR, "/var/lib/nvm"),
                    ("/mnt/<pool>/child", "/var/lib/nvm/child"),
                ],
            )),
            "comma" | "quote" | "control" => {
                let source = match case {
                    "comma" => "/mnt/<pool>/bad,path",
                    "quote" => "/mnt/<pool>/bad\"path",
                    _ => "/mnt/<pool>/bad\npath",
                };
                let mut value: serde_json::Value =
                    serde_json::from_str(&inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]))
                        .unwrap();
                value[0]["Mounts"][0]["Source"] = json!(source);
                host.set_docker_inspect(&value.to_string());
            }
            "mismatch" => host.preset("cat", &[], FakeOutput::stdout("wrong bytes")),
            _ => unreachable!(),
        }
        let checked = startup::check(&host, &Paths::new(CONTAINER_DATA_DIR));
        assert!(
            !checked.problems.is_empty(),
            "case {case}: the input did not fail a check: {checked:?}"
        );
        let app = app(host.clone(), settings("hunter12"), checked);

        let (_, public) = get(&app, "/api/login", None).await;
        assert_required_shape(&public, &format!("{case} public"));
        let (status, signed_in, cookie) = logged_in_login_status(&app).await;
        assert_eq!(status, StatusCode::OK, "{case}: {signed_in}");
        assert_required_shape(&signed_in, &format!("{case} signed-in"));

        // One `startup.failed` problem, never one per failed check.
        let startup_failures: Vec<&serde_json::Value> = signed_in["setup"]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["key"] == "startup.failed")
            .collect();
        assert_eq!(startup_failures.len(), 1, "{case}: {signed_in}");
        assert_eq!(
            startup_failures[0]["params"]["items"]
                .as_array()
                .unwrap()
                .len(),
            app.ctx.startup.problems.len(),
            "{case}: one startup failure line per failed item: {signed_in}"
        );

        // The write entries stay blocked with the `setup` cause (planning/submission gate).
        let blocked = signed_in["update_blocked"].as_array().unwrap();
        assert!(
            blocked.iter().any(|b| b["key"] == "setup"),
            "{case}: update_blocked lacks setup: {signed_in}"
        );
        let (plan_status, plan_body) = post_plan(
            &app,
            &cookie,
            json!({"kind": "official", "version": "580.178.04", "gpus": []}),
        )
        .await;
        assert_eq!(plan_status, StatusCode::CONFLICT, "{case}: {plan_body}");
        assert_eq!(plan_body["error"], "setup.blocked", "{case}");
        let id = truenas_nvidia_driver_manager::plan::remember(&app.ctx, legitimate_snapshot());
        let (task_status, task_body) = post_task(&app, &cookie, &id, "skip").await;
        assert_eq!(task_status, StatusCode::CONFLICT, "{case}: {task_body}");
        assert_eq!(task_body["error"], "setup.blocked", "{case}");
        assert_eq!(host.count("systemd-run"), 0, "{case}");
    }
}

#[tokio::test]
async fn outside_command_output_is_publicly_hidden_and_signed_in_present() {
    // The sign-in page's startup failure stops at the bracket; the signed-in login status carries
    // the external command output as a separate field (never inside the alert text itself).
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    host.set_docker_inspect(&inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]));
    host.preset(
        "docker",
        &["inspect"],
        FakeOutput::code(2).with_stderr("docker: external text"),
    );
    let checked = startup::check(&host, &Paths::new(CONTAINER_DATA_DIR));
    let app = app(host.clone(), settings("hunter12"), checked);
    let (_, public) = get(&app, "/api/login", None).await;
    assert_required_shape(&public, "inspect public");
    assert_eq!(
        public["setup"]["problems"][0]["params"]["items"][0]["raw"],
        serde_json::Value::Null,
        "the public login status must not carry external command output: {public}"
    );
    let (_, signed_in, _) = logged_in_login_status(&app).await;
    assert_required_shape(&signed_in, "inspect signed-in");
    assert_eq!(
        signed_in["setup"]["problems"][0]["params"]["items"][0]["raw"], "docker: external text",
        "the signed-in login status carries the external text: {signed_in}"
    );
}

#[tokio::test]
async fn several_failed_checks_still_make_one_startup_failure_and_three_passes_make_none() {
    // Two failed checks (runtime + mount count) are one startup failure with two lines, while a
    // healthy check set leaves `setup` empty.
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let inspect: serde_json::Value = serde_json::from_str(&inspect_json(
        "nvidia",
        &[
            (HOST_DATA_DIR, "/var/lib/nvm"),
            ("/mnt/<pool>/second", "/var/lib/nvm"),
        ],
    ))
    .unwrap();
    host.set_docker_inspect(&inspect.to_string());
    let checked = startup::check(&host, &Paths::new(CONTAINER_DATA_DIR));
    assert_eq!(checked.problems.len(), 2, "{checked:?}");
    let two = app(host.clone(), settings("hunter12"), checked);
    let (_, body, _) = logged_in_login_status(&two).await;
    assert_required_shape(&body, "two failures");
    let items = startup_item_keys(&body);
    assert_eq!(items.len(), 2, "{body}");
    assert!(items.contains(&"startup.runtime".to_string()), "{body}");
    assert!(
        items.contains(&"startup.data_mapping".to_string()),
        "{body}"
    );
    // Leave only the runtime failure; the items shrink to that one line. A fresh fake host keeps
    // its own credentials, so the app signs in with the initial password again.
    let one_host = default_host();
    prime_canonical(&one_host, "580.173.02");
    one_host.set_docker_inspect(&inspect_json("nvidia", &[(HOST_DATA_DIR, "/var/lib/nvm")]));
    let one = startup::check(&one_host, &Paths::new(CONTAINER_DATA_DIR));
    assert_eq!(one.problems.len(), 1, "{one:?}");
    let one_app = app(one_host.clone(), settings("hunter12"), one);
    let (_, body_one, _) = logged_in_login_status(&one_app).await;
    assert_eq!(
        startup_item_keys(&body_one),
        vec!["startup.runtime".to_string()]
    );

    let passed_host = default_host();
    prime_canonical(&passed_host, "580.173.02");
    let passed = app(
        passed_host.clone(),
        settings("hunter12"),
        healthy_startup(&passed_host),
    );
    let (_, body_passed, _) = logged_in_login_status(&passed).await;
    assert_eq!(
        problem_keys(&body_passed),
        Vec::<String>::new(),
        "{body_passed}"
    );
    assert_eq!(
        body_passed["update_blocked"].as_array().unwrap().len(),
        0,
        "{body_passed}"
    );
}

#[tokio::test]
async fn settings_problems_keep_their_own_keys_next_to_the_startup_failure() {
    let mut configured = settings("hunter12");
    configured.problems = vec![
        truenas_nvidia_driver_manager::settings::SettingProblem::Unknown {
            variable: "NVM_FOO".into(),
        },
        truenas_nvidia_driver_manager::settings::SettingProblem::Invalid {
            variable: truenas_nvidia_driver_manager::settings::VAR_PORT,
            rule: truenas_nvidia_driver_manager::settings::RULE_PORT,
        },
    ];
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let checked = startup::check(&host, &Paths::new(CONTAINER_DATA_DIR));
    assert!(!checked.problems.is_empty());
    let app = app(host.clone(), configured, checked);
    let (_, body, _) = logged_in_login_status(&app).await;
    let keys = problem_keys(&body);
    assert!(keys.contains(&"setting.unknown".to_string()), "{body}");
    assert!(keys.contains(&"setting.invalid".to_string()), "{body}");
    assert_eq!(
        keys.iter().filter(|k| *k == "startup.failed").count(),
        1,
        "{body}"
    );
    assert_required_shape(&body, "mixed");
}

#[tokio::test]
async fn a_failed_pid_host_check_keeps_the_home_readings_unknown() {
    // Without the host PID namespace, home readings cannot be trusted.
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    host.set_same_namespace(true);
    let checked = startup::check(&host, &Paths::new(CONTAINER_DATA_DIR));
    assert_eq!(checked.problems.len(), 1, "{checked:?}");
    let app = app(host.clone(), settings("hunter12"), checked);
    let cookie = {
        let _ = get(&app, "/api/login", None).await;
        login_changed(&app, "hunter12", "changed123").await
    };
    let (_, home) = get(&app, "/api/home", Some(&cookie)).await;
    for key in ["loaded_version", "truenas_version", "link"] {
        assert_eq!(
            home[key]["unknown"]["key"], "reason.startup_check_failed",
            "{key}: {home}"
        );
    }
    assert_eq!(
        home["canonical"]["sha256"]["unknown"]["key"], "reason.startup_check_failed",
        "{home}"
    );
    assert!(
        !home["update_blocked"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["key"] == "extension_missing"),
        "{home}"
    );
}

/// A known setting whose value is not valid UTF-8 is the invalid setting `setting.invalid` problem
/// of the outer closed set, marked in its params; the four variables keep one independent problem
/// each, and the default/fallback rules stay valid.
#[tokio::test]
async fn login_status_non_utf8_setting_uses_the_required_outer_key_closed_set() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use truenas_nvidia_driver_manager::settings::{self, InitialPassword, SettingProblem};

    const NON_UTF8: [u8; 2] = [0xff, 0xfe];
    let variables = [
        settings::VAR_USERNAME,
        settings::VAR_PASSWORD,
        settings::VAR_PORT,
        settings::VAR_BUILD_TIMEOUT,
    ];

    for variable in variables {
        // One non-UTF-8 known variable at a time, with a usable initial password elsewhere; the
        // password variable carries no value at all.
        let mut vars = vec![(
            OsString::from(settings::VAR_PASSWORD),
            OsString::from("hunter12"),
        )];
        if variable == settings::VAR_PASSWORD {
            vars.clear();
        }
        vars.push((
            OsString::from(variable),
            OsString::from_vec(NON_UTF8.to_vec()),
        ));
        let configured = settings::load(vars.into_iter());

        // Each variable keeps its own problem, and the value is never stored.
        assert_eq!(configured.problems.len(), 1, "{variable}: {configured:?}");
        assert!(
            configured
                .problems
                .iter()
                .any(|p| matches!(p, SettingProblem::NotUtf8 { variable: v } if v == variable)),
            "{variable}: {configured:?}"
        );
        match variable {
            settings::VAR_USERNAME => assert_eq!(configured.username, None, "{variable}"),
            settings::VAR_PASSWORD => assert!(
                matches!(configured.password, InitialPassword::Unusable),
                "{variable}"
            ),
            settings::VAR_PORT => assert_eq!(
                configured.port,
                settings::DEFAULT_PORT,
                "{variable}: the default-port fallback stays"
            ),
            _ => assert_eq!(configured.build_timeout_minutes, None, "{variable}"),
        }

        let host = default_host();
        prime_canonical(&host, "580.173.02");
        let app = app(host.clone(), configured, healthy_startup(&host));
        let (status, public) = get(&app, "/api/login", None).await;
        assert_eq!(status, StatusCode::OK, "{variable}: {public}");
        assert_eq!(
            problem_keys(&public),
            vec!["setting.invalid".to_string()],
            "{variable}: {public}"
        );
        // The exact shape: `setting.invalid` is the outer key; the invalid setting UTF-8 form is a
        // params marker, not a protocol key.
        assert_eq!(
            public["setup"]["problems"][0],
            json!({"key": "setting.invalid", "params": {"variable": variable, "invalid_utf8": true}}),
            "{variable}: {public}"
        );
        assert!(
            !public.to_string().contains("setting.invalid_utf8"),
            "{variable}: the retired outer key leaked: {public}"
        );
        assert_required_shape(&public, variable);

        // A non-UTF-8 username or initial password leaves no usable account, so only the port and
        // timeout variables reach the signed-in login status; both phases keep the closed set.
        if matches!(variable, settings::VAR_PORT | settings::VAR_BUILD_TIMEOUT) {
            let (status, signed_in, _) = logged_in_login_status(&app).await;
            assert_eq!(status, StatusCode::OK, "{variable}: {signed_in}");
            assert_eq!(
                problem_keys(&signed_in),
                vec!["setting.invalid".to_string()],
                "{variable}: {signed_in}"
            );
            assert_eq!(
                signed_in["setup"]["problems"][0],
                json!({"key": "setting.invalid", "params": {"variable": variable, "invalid_utf8": true}}),
                "{variable}: {signed_in}"
            );
            assert_required_shape(&signed_in, variable);
            assert!(
                signed_in["update_blocked"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["key"] == "setup"),
                "{variable}: {signed_in}"
            );
        }
        host.assert_red_lines();
    }

    // All four at once: four independent lines, none merged or lost.
    let mut vars = Vec::new();
    for variable in variables {
        vars.push((
            OsString::from(variable),
            OsString::from_vec(NON_UTF8.to_vec()),
        ));
    }
    let configured = settings::load(vars.into_iter());
    assert_eq!(configured.problems.len(), 4, "{configured:?}");
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let multi = app(host.clone(), configured, healthy_startup(&host));
    let (status, public) = get(&multi, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK, "{public}");
    let problems = public["setup"]["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 4, "{public}");
    for variable in variables {
        assert!(
            problems.iter().any(|p| p
                == &json!({"key": "setting.invalid", "params": {"variable": variable, "invalid_utf8": true}})),
            "{variable}: {public}"
        );
    }
    assert_required_shape(&public, "all four");
    host.assert_red_lines();

    // Controls: no setting problem and a textual invalid port both keep the closed set and the
    // invalid setting requirement sentence.
    for value in [None, Some("abc")] {
        let mut vars = vec![(
            OsString::from(settings::VAR_PASSWORD),
            OsString::from("hunter12"),
        )];
        if let Some(v) = value {
            vars.push((OsString::from(settings::VAR_PORT), OsString::from(v)));
        }
        let host = default_host();
        prime_canonical(&host, "580.173.02");
        let control = app(
            host.clone(),
            settings::load(vars.into_iter()),
            healthy_startup(&host),
        );
        let (_, body) = get(&control, "/api/login", None).await;
        match value {
            None => {
                assert_eq!(problem_keys(&body), Vec::<String>::new(), "{body}");
            }
            Some(_) => {
                assert_required_shape(&body, &format!("port control {value:?}"));
                assert_eq!(
                    problem_keys(&body),
                    vec!["setting.invalid".to_string()],
                    "{body}"
                );
                assert_eq!(
                    body["setup"]["problems"][0]["params"]["variable"], "NVM_PORT",
                    "{body}"
                );
                assert_eq!(
                    body["setup"]["problems"][0]["params"]["requirement"],
                    json!({"key": "setting.rule.port", "params": {}}),
                    "{body}"
                );
                assert_eq!(
                    body["setup"]["problems"][0]["params"].get("invalid_utf8"),
                    None,
                    "{body}"
                );
            }
        }
        host.assert_red_lines();
    }
}

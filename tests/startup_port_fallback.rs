//! With an unusable `NVM_PORT`, `web::serve_with` listens on 8199 and writes `port_fallback`. This
//! exercises the real serve call and its complete log sink. The process-global sink is used by one
//! test binary, with no concurrent test.

mod common;

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use truenas_nvidia_driver_manager::settings::{self, VAR_PASSWORD, VAR_PORT};
use truenas_nvidia_driver_manager::{log, web};

/// Run the real `serve_with` until the startup path is past the checks, then stop it and return the
/// complete sink.
async fn serve_log(vars: Vec<(OsString, OsString)>) -> String {
    let configured = settings::load(vars.into_iter());
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    host.set_docker_inspect(&inspect_json("runc", &[(HOST_DATA_DIR, "/var/lib/nvm")]));
    let sink = SharedBuf::default();
    log::set_sink(Box::new(sink.clone()));
    let web_root = std::env::temp_dir().join(format!(
        "nvm-port-fallback-{}",
        truenas_nvidia_driver_manager::store::random_hex(8)
    ));
    std::fs::create_dir(&web_root).unwrap();
    let task = tokio::spawn(web::serve_with(
        configured,
        Arc::new(host),
        web_root.clone(),
    ));
    for _ in 0..600 {
        let text = String::from_utf8_lossy(&sink.0.lock().unwrap()).into_owned();
        if text.contains("\"event\":\"startup_checks_passed\"")
            || text.contains("\"event\":\"startup_check_failed\"")
            || task.is_finished()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    task.abort();
    let _ = task.await;
    log::set_sink(Box::new(std::io::stdout()));
    let _ = std::fs::remove_dir_all(&web_root);
    text
}

fn event_line(text: &str, event: &str) -> Option<serde_json::Value> {
    for line in text.lines() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            if value["event"] == event {
                return Some(value);
            }
        }
    }
    None
}

#[tokio::test]
async fn invalid_port_falls_back_and_logs_port_fallback() {
    // A text-invalid value and an invalid-UTF-8 value both listen on 8199 and write the ERROR line
    // with the message and the port field.
    for label in ["text", "non-utf8"] {
        let value = if label == "text" {
            OsString::from("abc")
        } else {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![0xff, 0xfe])
        };
        let text = serve_log(vec![
            (OsString::from(VAR_PASSWORD), OsString::from("hunter12")),
            (OsString::from(VAR_PORT), value),
        ])
        .await;
        let fallback = event_line(&text, "port_fallback")
            .unwrap_or_else(|| panic!("{label}: no port_fallback in {text}"));
        assert_eq!(fallback["level"], "ERROR", "{label}: {text}");
        assert_eq!(
            fallback["msg"], "NVM_PORT is invalid; listening on the default port",
            "{label}: {text}"
        );
        assert_eq!(fallback["port"], 8199, "{label}: {text}");
        assert!(
            event_line(&text, "setting_invalid").is_some(),
            "{label}: the setting_invalid line must stay: {text}"
        );
    }
    // The default and a legal port never write the fallback (or the invalid setting line).
    for vars in [
        vec![(OsString::from(VAR_PASSWORD), OsString::from("hunter12"))],
        vec![
            (OsString::from(VAR_PASSWORD), OsString::from("hunter12")),
            (OsString::from(VAR_PORT), OsString::from("9000")),
        ],
    ] {
        let text = serve_log(vars).await;
        assert!(
            event_line(&text, "port_fallback").is_none(),
            "unexpected port_fallback: {text}"
        );
        assert!(
            event_line(&text, "setting_invalid").is_none(),
            "unexpected setting_invalid: {text}"
        );
    }
}

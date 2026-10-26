//! Fixed downloads through the public runner entry point and the fake host.

mod common;

use common::{sha256_hex, FakeHost, HOST_DATA_DIR};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::host::Host;
use truenas_nvidia_driver_manager::runner::{
    download::{fetch, CurlRedirects, FixedInput},
    RunCtx,
};
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{
    self, new_state, OriginalChoice, Phase, Plan, PlanSource,
};

const ARCHIVE_URL: &str = "https://snapshot.debian.org/archive/test/patches.tar.xz";
const ARCHIVE_BYTES: &[u8] = b"small fixed archive fixture";

fn archive_ctx(host: &FakeHost) -> RunCtx {
    let id = "20261024T010203Z-1a2b3c4d";
    let plan = Plan {
        source: PlanSource::Official,
        version: COMMUNITY_PATCH_VERSION.into(),
        kernel: "6.18.52-production+truenas".into(),
        truenas_version: "27.0.0-RC.1".into(),
        original: OriginalChoice::Skip,
        image_id: format!("sha256:{}", "a".repeat(64)),
        build_timeout_minutes: 30,
        gpus: Vec::new(),
        kernel_module_type: None,
    };
    let ctx = RunCtx {
        host: Arc::new(host.clone()),
        paths: Paths::new(HOST_DATA_DIR),
        id: id.into(),
        state: new_state(id, 1, plan, Phase::Build),
        build_deadline: None,
    };
    host.mkdir(ctx.task_dir().join("dl").to_str().unwrap());
    host.mkdir(ctx.task_dir().join("in").to_str().unwrap());
    host.mkdir(ctx.paths.state_log(id).parent().unwrap().to_str().unwrap());
    ctx
}

fn archive_spec(ctx: &RunCtx) -> FixedInput {
    FixedInput {
        url: ARCHIVE_URL.into(),
        file_name: COMMUNITY_PATCH_ARCHIVE_NAME.into(),
        part: ctx.task_dir().join("dl").join(COMMUNITY_PATCH_PART_NAME),
        headers: ctx.task_dir().join("dl").join(COMMUNITY_PATCH_HEADERS_NAME),
        target: ctx.task_dir().join("in").join(COMMUNITY_PATCH_INPUT_NAME),
        redirects: CurlRedirects::Https,
        expected: Some((ARCHIVE_BYTES.len() as u64, sha256_hex(ARCHIVE_BYTES))),
        progress: false,
    }
}

#[test]
fn community_archive_follows_rooted_https_redirect_and_lands_verified_bytes() {
    let host = FakeHost::new();
    host.set_curl_redirect(ARCHIVE_URL, 302, "/file/fixed/patches.tar.xz", b"redirect");
    host.set_curl(
        "https://snapshot.debian.org/file/fixed/patches.tar.xz",
        200,
        ARCHIVE_BYTES,
    );
    let mut ctx = archive_ctx(&host);
    let spec = archive_spec(&ctx);
    let deadline = host.now() + Duration::from_secs(60);
    let bytes = fetch(&mut ctx, &spec, deadline).unwrap();
    let target = host.file(spec.target.to_str().unwrap()).unwrap();
    assert_eq!(target, ARCHIVE_BYTES);
    assert_eq!(bytes, target.len() as u64);
    assert!(!host.exists(spec.part.to_str().unwrap()));
    let headers = String::from_utf8(host.file(spec.headers.to_str().unwrap()).unwrap()).unwrap();
    assert_eq!(headers.matches("HTTP/1.1").count(), 2);
    assert!(headers.starts_with("HTTP/1.1 302"));
    assert!(headers.contains("HTTP/1.1 200"));

    let commands = host.commands();
    let curl = commands.iter().find(|c| c.program == "curl").unwrap();
    for pair in [
        ["--proto", "=https"],
        ["--proto-redir", "=https"],
        ["--max-redirs", "5"],
        ["--output", spec.part.to_str().unwrap()],
        ["--dump-header", spec.headers.to_str().unwrap()],
    ] {
        assert!(curl.args.windows(2).any(|w| w == pair), "{pair:?}");
    }
    assert!(curl.args.iter().any(|arg| arg == "--location"));
    assert!(curl.args.iter().any(|arg| arg == "--fail"));
    assert_eq!(host.count("curl"), 1);

    let log = task_state::log_tail(&host, &ctx.paths, &ctx.id, None);
    let downloaded: Vec<_> = log
        .lines
        .iter()
        .filter(|line| line.key == LogKey::FileDownloaded.as_str())
        .collect();
    assert_eq!(downloaded.len(), 1);
    assert_eq!(downloaded[0].p["file_name"], COMMUNITY_PATCH_ARCHIVE_NAME);
    assert_eq!(downloaded[0].p["bytes"], target.len() as u64);
    assert_ne!(
        spec.file_name,
        spec.target.file_name().unwrap().to_str().unwrap()
    );
}

#[test]
fn community_archive_rejects_http_errors_unsafe_redirects_and_bad_checksums() {
    for scene in ["404", "503", "http", "six-hops", "hash", "size"] {
        let host = FakeHost::new();
        match scene {
            "404" | "503" => {
                let location = format!("/file/error{scene}");
                host.set_curl_redirect(ARCHIVE_URL, 302, &location, b"redirect");
                host.set_curl_failure(
                    &format!("https://snapshot.debian.org{location}"),
                    22,
                    &format!("curl: (22) The requested URL returned error: {scene}"),
                );
            }
            "http" => {
                host.set_curl_redirect(
                    ARCHIVE_URL,
                    302,
                    "http://snapshot.debian.org/file/patches",
                    b"redirect",
                );
                host.set_curl(
                    "http://snapshot.debian.org/file/patches",
                    200,
                    ARCHIVE_BYTES,
                );
            }
            "six-hops" => {
                let mut previous = ARCHIVE_URL.to_owned();
                for hop in 1..=6 {
                    let next = format!("https://snapshot.debian.org/file/hop{hop}");
                    host.set_curl_redirect(&previous, 302, &next, b"redirect");
                    previous = next;
                }
                host.set_curl(&previous, 200, ARCHIVE_BYTES);
            }
            "hash" => host.set_curl(ARCHIVE_URL, 200, &vec![b'x'; ARCHIVE_BYTES.len()]),
            "size" => host.set_curl(ARCHIVE_URL, 200, b"short"),
            _ => unreachable!(),
        }
        let mut ctx = archive_ctx(&host);
        let spec = archive_spec(&ctx);
        let deadline = host.now() + Duration::from_secs(60);
        let failure = fetch(&mut ctx, &spec, deadline).unwrap_err();
        assert_eq!(failure.code, ErrorCode::DownloadFailed, "{scene}");
        let reason = if matches!(scene, "hash" | "size") {
            DOWNLOAD_CHECKSUM_MISMATCH
        } else {
            DOWNLOAD_REQUEST_FAILED
        };
        assert_eq!(failure.params["reason"]["key"], reason, "{scene}");
        assert_eq!(
            failure.params["reason"]["params"]["file_name"], COMMUNITY_PATCH_ARCHIVE_NAME,
            "{scene}"
        );
        assert!(!host.exists(spec.target.to_str().unwrap()), "{scene}");
        let part = host.file(spec.part.to_str().unwrap());
        match scene {
            "hash" => assert_eq!(part, Some(vec![b'x'; ARCHIVE_BYTES.len()])),
            "size" => assert_eq!(part.as_deref(), Some(b"short".as_slice())),
            _ => assert!(part.is_none(), "{scene}: failed before a body was written"),
        }
        let headers =
            String::from_utf8(host.file(spec.headers.to_str().unwrap()).unwrap()).unwrap();
        assert_eq!(
            headers.matches("HTTP/1.1").count(),
            if scene == "six-hops" { 6 } else { 1 },
            "{scene}"
        );
        assert!(headers.starts_with(if matches!(scene, "hash" | "size") {
            "HTTP/1.1 200"
        } else {
            "HTTP/1.1 302"
        }));
        assert_eq!(host.count("curl"), 1, "{scene}");
    }
}

#[test]
fn fixed_input_expired_deadline_stops_inside_fetch_without_a_command() {
    let host = FakeHost::new();
    host.set_curl(ARCHIVE_URL, 200, ARCHIVE_BYTES);
    let mut ctx = archive_ctx(&host);
    let spec = archive_spec(&ctx);
    let failure = fetch(&mut ctx, &spec, host.now()).unwrap_err();
    assert_eq!(failure.code, ErrorCode::BuildTimeout);
    assert_eq!(failure.params, json!({}));
    assert_eq!(failure.command, None);
    assert_eq!(failure.raw, None);
    assert_eq!(host.count("curl"), 0);
    for path in [&spec.target, &spec.part, &spec.headers] {
        assert!(!host.exists(path.to_str().unwrap()));
    }
}

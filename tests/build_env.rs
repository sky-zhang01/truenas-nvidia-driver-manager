//! The pinned build environment: the official image, the manifest packages, the installer, and the
//! `image_id` of the task record.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use truenas_nvidia_driver_manager::build_kit;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::host::{Exit, Host};
use truenas_nvidia_driver_manager::runner::build_env::{self, BuildEnv};
use truenas_nvidia_driver_manager::runner::{self, FlowStop, RunCtx};
use truenas_nvidia_driver_manager::store::Paths;
use truenas_nvidia_driver_manager::task_state::{self, new_state, Phase, PlanSource, TaskState};

const ID: &str = "20261011T101530Z-1a2b3c4d";
const ID_NEXT: &str = "20261011T111530Z-5e6f7a8b";
const KIT_DIR: &str = "manager/build-kit";
const CACHE_DIR: &str = "manager/build-kit-cache";
const DEB: &str = "binutils_2.40-2_amd64.deb";

fn paths() -> Paths {
    Paths::new(HOST_DATA_DIR)
}

fn ctx(host: &FakeHost, source: PlanSource) -> RunCtx {
    ctx_for(host, source, ID)
}

fn ctx_for(host: &FakeHost, source: PlanSource, id: &str) -> RunCtx {
    RunCtx {
        host: Arc::new(host.clone()),
        paths: paths(),
        id: id.into(),
        state: new_state(id, 1, sample_plan(source), Phase::Build),
        build_deadline: None,
    }
}

fn upload_source() -> PlanSource {
    // A valid upload source carries the checksum of the installer and of all four toolkit packages.
    let mut deb_sha256 = BTreeMap::new();
    for package in TOOLKIT_PACKAGES {
        deb_sha256.insert(package.to_string(), sha256_hex(b"toolkit package"));
    }
    PlanSource::Upload {
        run_sha256: sha256_hex(b"installer"),
        fingerprint: "a".repeat(12),
        deb_sha256,
    }
}

/// A small manifest of the test's own: the embedded packages have no real bytes inside a test,
/// while every step here (offline hit, download, verification) needs bytes to compare.
fn kit_manifest(files: &[(&str, &[u8])]) -> Vec<build_kit::KitPackage> {
    files
        .iter()
        .map(|(filename, bytes)| build_kit::KitPackage {
            sha256: sha256_hex(bytes),
            bytes: bytes.len() as u64,
            filename: (*filename).into(),
            package: filename.split('_').next().unwrap_or_default().into(),
            version: "1.0-1".into(),
            url: format!(
                "https://snapshot.debian.org/archive/debian/{}/pool/main/b/{filename}",
                build_kit::BUILD_ENV_SNAPSHOT
            ),
        })
        .collect()
}

fn deadline(host: &FakeHost) -> Instant {
    host.now() + Duration::from_secs(600)
}

fn docker_argvs(host: &FakeHost) -> Vec<Vec<String>> {
    host.commands()
        .into_iter()
        .filter(|c| c.program == "docker")
        .map(|c| c.args)
        .collect()
}

fn has_docker(host: &FakeHost, subcommand: &str) -> bool {
    docker_argvs(host)
        .iter()
        .any(|args| args.first().map(String::as_str) == Some(subcommand))
}

fn curl_argv(host: &FakeHost) -> Vec<String> {
    host.commands()
        .into_iter()
        .find(|cmd| cmd.program == "curl")
        .expect("curl ran")
        .argv()
}

fn assert_https_redirect_flags(argv: &[String]) {
    let pairs: Vec<(&str, &str)> = argv
        .windows(2)
        .map(|pair| (pair[0].as_str(), pair[1].as_str()))
        .collect();
    assert!(
        pairs.contains(&("--proto", "=https")),
        "https only: {argv:?}"
    );
    assert!(
        pairs.contains(&("--proto-redir", "=https")),
        "redirects stay on https: {argv:?}"
    );
    assert!(
        argv.iter().any(|arg| arg == "--location"),
        "redirects are followed: {argv:?}"
    );
    assert!(
        pairs.contains(&("--max-redirs", "5")),
        "the redirect cap is 5: {argv:?}"
    );
}

fn image_removal(host: &FakeHost) -> Option<Vec<String>> {
    docker_argvs(host)
        .into_iter()
        .find(|args| args.len() > 1 && args[0] == "image" && args[1] == "rm")
}

/// Every `docker image inspect --format {{.Id}} …` lookup this host saw, in order, as the reference
/// or ID it named.
fn image_lookups(host: &FakeHost) -> Vec<String> {
    docker_argvs(host)
        .into_iter()
        .filter(|args| {
            args.first().map(String::as_str) == Some("image")
                && args.get(1).map(String::as_str) == Some("inspect")
        })
        .filter_map(|args| args.last().cloned())
        .collect()
}

fn persisted(host: &FakeHost) -> TaskState {
    match task_state::read(host, &paths(), ID) {
        truenas_nvidia_driver_manager::store::JsonRead::Ok(state) => state,
        other => panic!("state unreadable: {other:?}"),
    }
}

fn task_file(relative: &str) -> String {
    format!("{HOST_DATA_DIR}/manager/tasks/{ID}/{relative}")
}

fn offline_file(filename: &str) -> String {
    format!("{HOST_DATA_DIR}/{KIT_DIR}/{filename}")
}

fn cache_file(filename: &str) -> String {
    format!("{HOST_DATA_DIR}/{CACHE_DIR}/{filename}")
}

fn task_kit_file(id: &str, filename: &str) -> String {
    format!("{HOST_DATA_DIR}/manager/tasks/{id}/in/build-kit/{filename}")
}

fn prepared_env() -> BuildEnv {
    BuildEnv::new(
        format!("sha256:{}", "d".repeat(64)),
        false,
        vec![format!("/build-kit/{DEB}")],
    )
}

#[test]
fn an_offline_package_with_the_same_checksum_is_used_and_the_image_is_removed_again() {
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    let mut ctx = ctx(&host, PlanSource::Official);
    let env =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect("an offline package needs no network");

    // The offline copy is used as it is: not one curl, and the package path points into
    // `/build-kit`.
    assert_eq!(env.kit, vec![format!("/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 0);
    assert_eq!(host.file(&offline_file(DEB)).as_deref(), Some(&bytes[..]));
    // The image was not local, so this task pulled it once; the task record carries the verified
    // local image ID.
    assert!(env.pulled_image);
    assert_eq!(env.image_id, build_kit::BUILD_ENV_IMAGE_ID);
    assert!(has_docker(&host, "pull"));
    assert_eq!(host.images().len(), 1);
    let state = persisted(&host);
    assert_eq!(
        state.plan.expect("a plan").image_id,
        build_kit::BUILD_ENV_IMAGE_ID
    );
    // "Pulled by this task" is in the task state: the state still reads back, and the field is
    // optional with an unchanged shape.
    assert_eq!(
        state.build_env_pulled_image.as_deref(),
        Some(build_kit::BUILD_ENV_IMAGE_ID)
    );

    let installer = host
        .file_text(&task_file("in/install-build-kit.sh"))
        .expect("the installer is written");
    assert!(installer.contains("dpkg -i"));
    assert_eq!(
        host.file_text(&task_file("in/build-kit.list")).unwrap(),
        format!("/build-kit/{DEB}\n")
    );

    // The build is over: the image this task pulled is removed, and the offline folder is left
    // alone.
    build_env::release(&mut ctx, &env);
    assert!(host.images().is_empty());
    assert_eq!(host.file(&offline_file(DEB)).as_deref(), Some(&bytes[..]));
    assert!(persisted(&host).build_env_pulled_image.is_none());
}

#[test]
fn a_missing_offline_package_is_downloaded_verified_and_renamed() {
    let bytes = b"downloaded package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("a verified download lands the package");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(
        host.file(&task_file(&format!("in/build-kit/{DEB}")))
            .as_deref(),
        Some(&bytes[..])
    );
    // The temporary name does not stay behind (temporary name, verification, rename).
    assert!(!host.exists(&task_file(&format!("dl/{DEB}.part"))));
    let log = host
        .file_text(paths().state_log(ID).to_str().unwrap())
        .unwrap();
    assert!(log.contains("log.file_downloaded"), "{log}");
    assert!(log.contains(DEB), "{log}");
}

#[test]
fn a_checksum_mismatch_is_the_existing_download_reason() {
    let bytes = b"the real package";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.set_curl(&manifest[0].url, 200, b"tampered bytes");
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect_err("a wrong checksum must not land");

    assert_eq!(failure.code, ErrorCode::DownloadFailed);
    assert_eq!(
        failure.params["reason"],
        serde_json::json!({"key": DOWNLOAD_CHECKSUM_MISMATCH, "params": {"file_name": DEB}})
    );
    // Nothing lands under the final name, and the image this task pulled is removed on this failure
    // path too.
    assert!(!host.exists(&task_file(&format!("in/build-kit/{DEB}"))));
    assert!(host.images().is_empty());
}

#[test]
fn a_download_failure_is_the_existing_download_reason() {
    let bytes = b"a package that never arrives";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.set_curl_failure(
        &manifest[0].url,
        22,
        "curl: (22) The requested URL returned error: 404",
    );
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect_err("a refused download must not land");

    assert_eq!(failure.code, ErrorCode::DownloadFailed);
    assert_eq!(failure.params["reason"]["key"], DOWNLOAD_REQUEST_FAILED);
    assert_eq!(failure.params["reason"]["params"]["file_name"], DEB);
    assert!(failure.raw.unwrap().contains("404"));
    let (argv, exit) = failure.command.expect("the failing curl travels with it");
    assert_eq!(argv[0], "curl");
    assert_eq!(exit, Exit::Code(22));
    assert!(host.images().is_empty());
}

#[test]
fn an_https_redirect_lands_the_final_bytes_after_the_checksum() {
    // Snapshot.debian.org answers an `/archive/…` URL with one same-host relative `Location`. The
    // bytes that land are the final response, and the manifest SHA256 is what accepts them.
    let bytes = b"bytes behind the snapshot redirect";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let location = format!("/file/redirect-target/{DEB}");
    let final_url = format!("https://snapshot.debian.org{location}");
    let host = FakeHost::new();
    host.set_curl_redirect(&manifest[0].url, 302, &location, b"redirect page");
    host.set_curl(&final_url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("one https redirect lands the final bytes");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(
        host.file(&task_file(&format!("in/build-kit/{DEB}")))
            .as_deref(),
        Some(&bytes[..])
    );
    assert!(!host.exists(&task_file(&format!("dl/{DEB}.part"))));
    let argv = curl_argv(&host);
    assert_https_redirect_flags(&argv);
    assert!(
        argv.iter().any(|arg| arg == &manifest[0].url),
        "curl is asked for the manifest URL, not the redirect target: {argv:?}"
    );
}

#[test]
fn a_redirect_to_http_is_refused() {
    // The http target even carries the manifest bytes. Following it would pass the checksum, so a
    // success here would mean the scheme check is missing.
    let bytes = b"bytes that must not arrive over http";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let http = format!("http://snapshot.debian.org/file/not-https/{DEB}");
    let host = FakeHost::new();
    host.set_curl_redirect(&manifest[0].url, 302, &http, b"redirect page");
    host.set_curl(&http, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect_err("a redirect to http must not land");

    assert_eq!(failure.code, ErrorCode::DownloadFailed);
    assert_eq!(failure.params["reason"]["key"], DOWNLOAD_REQUEST_FAILED);
    assert_eq!(failure.params["reason"]["params"]["file_name"], DEB);
    let raw = failure.raw.expect("curl's refusal is kept");
    assert!(
        raw.contains("Protocol \"http\" disabled"),
        "the refusal names http: {raw}"
    );
    let (argv, exit) = failure.command.expect("the failing curl travels with it");
    assert_eq!(exit, Exit::Code(1));
    assert_https_redirect_flags(&argv);
    assert!(!host.exists(&task_file(&format!("in/build-kit/{DEB}"))));
    assert!(host.images().is_empty());
}

#[test]
fn a_sixth_https_redirect_is_refused() {
    // Six hops. The last one holds the manifest bytes, so a missing cap would land them.
    // `--max-redirs 5` stops on the sixth redirect.
    let bytes = b"bytes past the redirect cap";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    let mut previous = manifest[0].url.clone();
    for hop in 1..=6 {
        let next = format!("https://snapshot.debian.org/file/hop{hop}/{DEB}");
        host.set_curl_redirect(&previous, 302, &next, b"hop");
        previous = next;
    }
    host.set_curl(&previous, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect_err("a sixth redirect must not land");

    assert_eq!(failure.code, ErrorCode::DownloadFailed);
    assert_eq!(failure.params["reason"]["key"], DOWNLOAD_REQUEST_FAILED);
    let raw = failure.raw.expect("curl's cap is kept");
    assert!(
        raw.contains("Maximum (5) redirects followed"),
        "the cap is five: {raw}"
    );
    let (argv, exit) = failure.command.expect("the failing curl travels with it");
    assert_eq!(exit, Exit::Code(47));
    assert_https_redirect_flags(&argv);
    assert_eq!(host.count("curl"), 1);
    assert!(!host.exists(&task_file(&format!("in/build-kit/{DEB}"))));
    assert!(host.images().is_empty());
}

#[test]
fn an_official_installer_redirect_stays_a_non_200_and_is_not_followed() {
    // The redirected URL holds a full body. Following it would make the status 200. The official
    // installer stops on the first response.
    let bytes = b"installer bytes behind a redirect";
    let host = FakeHost::new();
    let mut ctx = ctx(&host, PlanSource::Official);
    let version = ctx.state.plan.as_ref().unwrap().version.clone();
    let url = format!(
        "https://download.nvidia.com/XFree86/Linux-x86_64/{version}/NVIDIA-Linux-x86_64-{version}-no-compat32.run"
    );
    let redirected = format!("https://download.nvidia.com/redirected/{version}.run");
    host.set_curl_redirect(&url, 302, &redirected, b"redirect page");
    host.set_curl(&redirected, 200, bytes);
    let failure =
        truenas_nvidia_driver_manager::runner::download::driver(&mut ctx, deadline(&host))
            .expect_err("the official installer does not follow a redirect");

    assert_eq!(failure.code, ErrorCode::DownloadFailed);
    assert_eq!(
        failure.params["reason"],
        serde_json::json!({
            "key": DOWNLOAD_HTTP_STATUS,
            "params": {
                "file_name": format!("NVIDIA-Linux-x86_64-{version}-no-compat32.run"),
                "status": 302,
            },
        })
    );
    let argv = curl_argv(&host);
    assert!(
        !argv
            .iter()
            .any(|arg| arg == "--location" || arg == "--proto-redir" || arg == "--max-redirs"),
        "the installer curl does not follow: {argv:?}"
    );
    assert!(
        argv.windows(2)
            .any(|pair| pair[0] == "--proto" && pair[1] == "=https"),
        "https is still required: {argv:?}"
    );
    assert_eq!(host.count("curl"), 1);
    assert_ne!(
        host.file(&task_file("in/driver.run.part")).as_deref(),
        Some(&bytes[..])
    );
    assert!(!host.exists(&task_file("in/driver.run")));
}

#[test]
fn an_image_already_on_the_host_is_used_without_a_pull_and_is_kept() {
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.set_image(
        build_kit::BUILD_ENV_IMAGE_REF,
        build_kit::BUILD_ENV_IMAGE_ID,
    );
    let mut ctx = ctx(&host, PlanSource::Official);
    let env =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect("the local image is used as it is");

    assert_eq!(env.image_id, build_kit::BUILD_ENV_IMAGE_ID);
    assert!(!env.pulled_image);
    assert!(!has_docker(&host, "pull"));
    assert!(persisted(&host).build_env_pulled_image.is_none());
    build_env::release(&mut ctx, &env);
    // An image that was already local is neither pulled again nor removed (its ID was asked for
    // once).
    assert_eq!(host.images().len(), 1);
    assert!(image_removal(&host).is_none());
}

#[test]
fn an_image_that_is_local_under_its_id_only_is_used_without_a_pull_and_is_kept() {
    // A `docker save` / `docker load` round trip leaves the image local under its ID: the pinned
    // reference does not resolve, the known image ID does. The task uses that copy, never pulls,
    // and leaves it alone at release.
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.set_image(build_kit::BUILD_ENV_IMAGE_ID, build_kit::BUILD_ENV_IMAGE_ID);
    let mut ctx = ctx(&host, upload_source());
    let env =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect("the image loaded under its ID is the pinned one");

    assert_eq!(env.image_id, build_kit::BUILD_ENV_IMAGE_ID);
    assert!(!env.pulled_image);
    assert_eq!(
        image_lookups(&host),
        vec![
            build_kit::BUILD_ENV_IMAGE_REF.to_string(),
            build_kit::BUILD_ENV_IMAGE_ID.to_string(),
        ]
    );
    assert!(!has_docker(&host, "pull"));
    assert!(persisted(&host).build_env_pulled_image.is_none());

    let failure =
        runner::build::build(&mut ctx, &env, "gcc-12").expect_err("no artifact is preset");
    assert_eq!(failure.params["reason"]["key"], BUILD_ARTIFACT_UNREADABLE);
    let run = docker_argvs(&host)
        .into_iter()
        .find(|args| args.first().map(String::as_str) == Some("run"))
        .expect("the build container runs");
    assert_eq!(run[run.len() - 2], build_kit::BUILD_ENV_IMAGE_ID);
    assert_eq!(run[run.len() - 1], build_env::INSTALL_KIT_PATH);

    build_env::release(&mut ctx, &env);
    assert_eq!(host.images().len(), 1);
    assert!(image_removal(&host).is_none());
}

#[test]
fn the_index_id_is_the_second_lookup_fallback() {
    // A containerd-backed store loaded from an archive reports the index digest as the local image
    // ID; the second ID lookup finds that copy.
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.set_image(
        build_kit::BUILD_ENV_IMAGE_INDEX_ID,
        build_kit::BUILD_ENV_IMAGE_INDEX_ID,
    );
    let mut ctx = ctx(&host, PlanSource::Official);
    let env =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect("the index digest names the pinned image");

    assert_eq!(env.image_id, build_kit::BUILD_ENV_IMAGE_INDEX_ID);
    assert!(!env.pulled_image);
    assert_eq!(
        image_lookups(&host),
        vec![
            build_kit::BUILD_ENV_IMAGE_REF.to_string(),
            build_kit::BUILD_ENV_IMAGE_ID.to_string(),
            build_kit::BUILD_ENV_IMAGE_INDEX_ID.to_string(),
        ]
    );
    assert!(!has_docker(&host, "pull"));
}

#[test]
fn an_image_that_is_nowhere_is_pulled_after_both_id_lookups_miss() {
    // Neither the pinned reference nor either known content ID is local: all three lookups miss,
    // and only then does the pull run.
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    let mut ctx = ctx(&host, PlanSource::Official);
    let env =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect("the pull lands the pinned image");

    assert_eq!(env.image_id, build_kit::BUILD_ENV_IMAGE_ID);
    assert!(env.pulled_image);
    assert!(has_docker(&host, "pull"));
    // The pull makes the reference resolvable, so the check after it sees one more lookup of the
    // reference.
    assert_eq!(
        image_lookups(&host),
        vec![
            build_kit::BUILD_ENV_IMAGE_REF.to_string(),
            build_kit::BUILD_ENV_IMAGE_ID.to_string(),
            build_kit::BUILD_ENV_IMAGE_INDEX_ID.to_string(),
            build_kit::BUILD_ENV_IMAGE_REF.to_string(),
        ]
    );
    build_env::release(&mut ctx, &env);
    assert!(host.images().is_empty());
}

#[test]
fn a_foreign_id_under_a_known_id_is_refused_without_a_pull() {
    // An ID lookup that resolves to something other than the pinned fingerprint is a refusal, not a
    // miss: nothing is pulled or removed.
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    let foreign = format!("sha256:{}", "0".repeat(64));
    host.set_image(build_kit::BUILD_ENV_IMAGE_ID, &foreign);
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect_err("a foreign ID must be refused");

    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(failure.params["reason"]["key"], BUILD_DOCKER_RUN_FAILED);
    let raw = failure.raw.expect("the failure carries its raw text");
    assert!(raw.contains(&foreign), "the mismatch names the ID: {raw}");
    assert!(
        raw.contains("expected"),
        "the mismatch names the pin: {raw}"
    );
    assert!(!has_docker(&host, "pull"));
    assert_eq!(host.images().len(), 1);
}

#[test]
fn an_image_id_that_is_not_the_pinned_one_is_refused() {
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.set_image(
        build_kit::BUILD_ENV_IMAGE_REF,
        &format!("sha256:{}", "0".repeat(64)),
    );
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect_err("a foreign image must be refused");

    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(failure.params["reason"]["key"], BUILD_DOCKER_RUN_FAILED);
    assert!(failure.raw.unwrap().contains("expected"));
    // A refusal neither pulls nor removes anything, and no package is downloaded.
    assert!(!has_docker(&host, "pull"));
    assert_eq!(host.count("curl"), 0);
    assert_eq!(host.images().len(), 1);
}

#[test]
fn a_failed_pull_is_the_existing_build_reason() {
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.set_docker_pull_fails(true);
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect_err("a failed pull must not be ignored");

    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(failure.params["reason"]["key"], BUILD_DOCKER_RUN_FAILED);
    assert!(failure.raw.unwrap().contains("pull access denied"));
    let (argv, exit) = failure.command.expect("the failing pull travels with it");
    assert_eq!(argv[0], "docker");
    assert_eq!(argv[1], "pull");
    assert_eq!(exit, Exit::Code(1));
}

#[test]
fn a_pull_that_leaves_nothing_behind_is_removed_before_the_failure_goes_out() {
    // The pull succeeded, but the check right after finds no image (the copy vanished between the
    // two calls): the failure is the existing build reason and the copy this task pulled is removed
    // by the pinned reference, best effort -- it is not left on the host.
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.set_docker_pull_leaves_nothing(true);
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect_err("a pull whose image cannot be checked must fail");

    assert!(has_docker(&host, "pull"));
    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(failure.params["reason"]["key"], BUILD_DOCKER_RUN_FAILED);
    // The reason object is untouched by the removal: the removal is one note line of `raw` and
    // nothing else.
    assert!(failure.params["reason"]["params"]
        .as_object()
        .expect("the reason params are an object")
        .is_empty());
    let raw = failure.raw.expect("the failure carries its raw text");
    assert!(
        raw.contains("cannot be inspected"),
        "the original reason stays: {raw}"
    );
    assert!(
        raw.contains(&format!(
            "docker image rm {}",
            build_kit::BUILD_ENV_IMAGE_REF
        )),
        "the removal is on record: {raw}"
    );
    let removal = image_removal(&host).expect("the pulled copy is removed");
    assert_eq!(removal[2], build_kit::BUILD_ENV_IMAGE_REF);
    assert!(host.images().is_empty());
    assert_eq!(host.count("curl"), 0);
}

#[test]
fn a_pulled_image_with_a_foreign_id_is_removed_before_the_failure_goes_out() {
    // The pulled image carries a foreign ID: it is refused and the copy this task pulled is
    // removed; the failure reason and the shape of `image_id` stay as they are.
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    let foreign = format!("sha256:{}", "a".repeat(64));
    host.set_docker_pull_id(&foreign);
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect_err("a foreign pulled image must be refused");

    assert!(has_docker(&host, "pull"));
    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(failure.params["reason"]["key"], BUILD_DOCKER_RUN_FAILED);
    let raw = failure.raw.expect("the failure carries its raw text");
    assert!(raw.contains(&foreign), "the mismatch names the ID: {raw}");
    assert!(
        raw.contains("expected"),
        "the mismatch names the pin: {raw}"
    );
    assert!(
        raw.contains(&format!(
            "docker image rm {}",
            build_kit::BUILD_ENV_IMAGE_REF
        )),
        "the removal is on record: {raw}"
    );
    let removal = image_removal(&host).expect("the pulled copy is removed");
    assert_eq!(removal[2], build_kit::BUILD_ENV_IMAGE_REF);
    assert!(host.images().is_empty());
    assert_eq!(host.count("curl"), 0);
}

#[test]
fn a_failed_removal_after_a_failed_check_keeps_the_original_reason() {
    // The copy this task pulled cannot be removed (the image is in use): the reason stays "the
    // pulled image cannot be inspected", and the removal's command and exit only become one note
    // line of `raw`.
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.set_docker_pull_leaves_nothing(true);
    host.set_docker_image_rm_fails(true);
    let mut ctx = ctx(&host, PlanSource::Official);
    let failure =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect_err("the failed check still fails");

    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(failure.params["reason"]["key"], BUILD_DOCKER_RUN_FAILED);
    let raw = failure.raw.expect("the failure carries its raw text");
    assert!(raw.contains("cannot be inspected"));
    assert!(
        raw.contains(&format!(
            "docker image rm {}",
            build_kit::BUILD_ENV_IMAGE_REF
        )),
        "the attempted removal is on record: {raw}"
    );
    assert!(
        raw.contains(r#"{"code":1}"#),
        "the removal's exit is on record: {raw}"
    );
    assert!(image_removal(&host).is_some());
}

#[test]
fn a_containerd_style_index_digest_counts_as_the_pinned_image() {
    // A containerd-backed image store reports the index digest as the local image ID; that is the
    // same fingerprint, so it is accepted, and it does not count as "pulled by this task".
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.set_image(
        build_kit::BUILD_ENV_IMAGE_REF,
        build_kit::BUILD_ENV_IMAGE_INDEX_ID,
    );
    let mut ctx = ctx(&host, PlanSource::Official);
    let env =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect("the index digest names the same pinned image");
    assert_eq!(env.image_id, build_kit::BUILD_ENV_IMAGE_INDEX_ID);
    assert!(!env.pulled_image);
    build_env::release(&mut ctx, &env);
    assert_eq!(host.images().len(), 1);
}

#[test]
fn a_failed_image_removal_stays_on_record() {
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.set_docker_image_rm_fails(true);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env =
        build_env::acquire_packages(&mut ctx, deadline(&host), &kit_manifest(&[(DEB, bytes)]))
            .expect("the environment is ready");
    build_env::release(&mut ctx, &env);

    // A failed removal changes no verdict, but its argv and exit stay in the task log.
    assert_eq!(host.images().len(), 1);
    let removal = docker_argvs(&host)
        .into_iter()
        .find(|args| args.len() > 1 && args[0] == "image" && args[1] == "rm")
        .expect("the removal was attempted");
    assert_eq!(removal[1], "rm");
    assert_eq!(removal[2], env.image_id);
    // A failed removal keeps the record: the task state says honestly that the image this task
    // pulled is still there.
    assert_eq!(
        persisted(&host).build_env_pulled_image.as_deref(),
        Some(env.image_id.as_str())
    );
}

#[test]
fn the_build_script_is_the_one_in_the_repository() {
    // The build script is embedded in the program and its bytes are those of
    // docker/build-driver.sh; the build writes it into the task's `in` directory and the container
    // runs it from there.
    assert_eq!(
        build_env::BUILD_DRIVER_SCRIPT,
        include_str!("../docker/build-driver.sh")
    );
    assert_eq!(build_env::BUILD_SCRIPT_PATH, "/in/build-driver.sh");
    assert_eq!(build_env::INSTALL_KIT_PATH, "/in/install-build-kit.sh");
    assert_eq!(build_env::KIT_LIST_PATH, "/in/build-kit.list");
    assert_eq!(
        build_env::INSTALL_KIT_SCRIPT,
        include_str!("../docker/install-build-kit.sh")
    );

    let host = FakeHost::new();
    let ctx = ctx(&host, PlanSource::Official);
    build_env::write_build_script(&ctx).expect("the script is written");
    assert_eq!(
        host.file_text(&task_file("in/build-driver.sh")).unwrap(),
        build_env::BUILD_DRIVER_SCRIPT
    );
}

#[test]
fn a_failed_package_installation_is_the_existing_container_exit_reason() {
    // A failed package installation: the installer exits before the first NVM:STEP, so the
    // container ends with exit code 1 — the existing `container_exit` reason, with the dpkg tail in
    // the raw output.
    let host = FakeHost::new();
    let mut run = FakeOutput::ok();
    run.exit = Exit::Code(1);
    run.stderr = b"install-build-kit: dpkg -i failed; tail of /tmp/nvm-build-kit-dpkg.log:\n\
dpkg: error processing archive /build-kit/binutils_2.40-2_amd64.deb (--install):\n"
        .to_vec();
    host.preset_once("docker", &["run"], run);
    let mut ctx = ctx(&host, upload_source());

    let failure = runner::build::build(&mut ctx, &prepared_env(), "gcc-12").unwrap_err();
    assert_eq!(failure.code, ErrorCode::BuildFailed);
    assert_eq!(
        failure.params["reason"],
        serde_json::json!({"key": BUILD_CONTAINER_EXIT, "params": {"code": 1}})
    );
    assert!(failure
        .raw
        .unwrap()
        .contains("dpkg: error processing archive"));
    assert_eq!(failure.command.unwrap().1, Exit::Code(1));
}

#[test]
fn both_compiling_sources_prepare_the_build_environment() {
    // Both the official and the upload source must prepare the build environment: on the same empty
    // host both reach "pull the image, download the first package" and end with the existing
    // download failure reason.
    for source in [PlanSource::Official, upload_source()] {
        let host = FakeHost::new();
        let mut ctx = ctx(&host, source.clone());
        let outcome = runner::stage_build(&mut ctx, None, "gcc-12");
        assert_eq!(outcome, Err(FlowStop::Terminal), "source {source:?}");

        assert!(has_docker(&host, "pull"), "source {source:?}");
        let snapshot = host.commands().into_iter().any(|c| {
            c.program == "curl"
                && c.args
                    .last()
                    .is_some_and(|url| url.starts_with("https://snapshot.debian.org/"))
        });
        assert!(snapshot, "source {source:?} never asked the snapshot");
        let error = persisted(&host).error.expect("a terminal failure");
        assert_eq!(error.code, "DOWNLOAD_FAILED", "source {source:?}");
        assert_eq!(
            error.params["reason"]["key"], DOWNLOAD_REQUEST_FAILED,
            "source {source:?}"
        );
    }
}

#[test]
fn the_prepared_package_paths_are_all_inside_a_read_only_mount() {
    let bytes = b"offline package bytes";
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest).unwrap();
    assert!(env
        .kit
        .iter()
        .all(|path| path.starts_with("/build-kit/") || path.starts_with("/in/build-kit/")));
    assert_eq!(
        build_env::offline_dir(&PathBuf::from(HOST_DATA_DIR)),
        PathBuf::from(format!("{HOST_DATA_DIR}/{KIT_DIR}"))
    );
}

/// The first build downloads the package and leaves a verified copy in the cache. The second build,
/// in a fresh task directory, reads that copy and does not call curl.
#[test]
fn a_second_build_reads_the_cache_and_does_not_download() {
    let bytes = b"downloaded package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut first = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut first, deadline(&host), &manifest)
        .expect("the first build downloads the package");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
    assert!(!host.exists(&cache_file(&format!("{DEB}.part"))));
    assert_eq!(
        build_env::cache_dir(&PathBuf::from(HOST_DATA_DIR)),
        PathBuf::from(format!("{HOST_DATA_DIR}/{CACHE_DIR}"))
    );

    let mut second = ctx_for(&host, PlanSource::Official, ID_NEXT);
    let again = build_env::acquire_packages(&mut second, deadline(&host), &manifest)
        .expect("the cached package needs no network");
    assert_eq!(again.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1, "the second build must not download");
    assert_eq!(
        host.file(&task_kit_file(ID_NEXT, DEB)).as_deref(),
        Some(&bytes[..])
    );
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
}

#[test]
fn a_cached_package_with_the_wrong_size_is_downloaded_again_and_overwrites_it() {
    let bytes = b"downloaded package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.put_file(&cache_file(DEB), b"short".to_vec());
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("a short cache file is downloaded again");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
    assert_eq!(
        host.file(&task_kit_file(ID, DEB)).as_deref(),
        Some(&bytes[..])
    );
}

#[test]
fn a_cached_package_with_the_wrong_checksum_is_downloaded_again_and_overwrites_it() {
    let bytes = b"downloaded package bytes";
    // Same length as `bytes`, so a size check alone would accept it.
    let tampered = b"tampered package bytes!!";
    assert_eq!(bytes.len(), tampered.len());
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.put_file(&cache_file(DEB), tampered.to_vec());
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("a mismatched cache file is downloaded again");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
    assert_eq!(
        host.file(&task_kit_file(ID, DEB)).as_deref(),
        Some(&bytes[..])
    );
}

#[test]
fn a_cache_file_outside_the_manifest_is_removed_before_the_build() {
    let bytes = b"cached package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let obsolete = "oldpkg_1.0-1_amd64.deb";
    let host = FakeHost::new();
    host.put_file(&cache_file(DEB), bytes.to_vec());
    host.put_file(&cache_file(obsolete), b"from the previous manifest");
    let mut run = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut run, deadline(&host), &manifest)
        .expect("the current package comes from the cache");

    assert_eq!(host.count("curl"), 0);
    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert!(!host.exists(&cache_file(obsolete)));
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));

    // The removal happens before staging, so a build that then fails to download has already
    // dropped the file the manifest no longer names.
    let failed = FakeHost::new();
    failed.put_file(&cache_file(obsolete), b"from the previous manifest");
    let missing = kit_manifest(&[(DEB, b"a package that never arrives")]);
    failed.set_curl_failure(
        &missing[0].url,
        22,
        "curl: (22) The requested URL returned error: 404",
    );
    let mut failed_ctx = ctx(&failed, PlanSource::Official);
    let failure = build_env::acquire_packages(&mut failed_ctx, deadline(&failed), &missing)
        .expect_err("the missing package is still a download failure");
    assert_eq!(failure.code, ErrorCode::DownloadFailed);
    assert!(!failed.exists(&cache_file(obsolete)));
    assert!(!failed.exists(&cache_file(DEB)));
}

#[test]
fn a_partial_cache_file_is_not_used() {
    let bytes = b"downloaded package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    // The bytes match the manifest. Accepting either temporary name would skip the download.
    host.put_file(&cache_file(&format!("{DEB}.part")), bytes.to_vec());
    host.put_file(
        &cache_file(&format!(".{DEB}.tmp-0123456789abcdef")),
        bytes.to_vec(),
    );
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("a temporary cache name is downloaded");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
    assert!(!host.exists(&cache_file(&format!("{DEB}.part"))));
    assert!(!host.exists(&cache_file(&format!(".{DEB}.tmp-0123456789abcdef"))));
    let mut names = host.children(&format!("{HOST_DATA_DIR}/{CACHE_DIR}"));
    names.sort();
    assert_eq!(names, vec![DEB.to_string()]);
}

#[test]
fn deleting_the_cache_directory_downloads_the_package_again() {
    let bytes = b"downloaded package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut first = ctx(&host, PlanSource::Official);
    build_env::acquire_packages(&mut first, deadline(&host), &manifest)
        .expect("the first build fills the cache");
    assert_eq!(host.count("curl"), 1);
    host.remove_tree(&format!("{HOST_DATA_DIR}/{CACHE_DIR}"));
    assert!(!host.exists(&cache_file(DEB)));

    let mut second = ctx_for(&host, PlanSource::Official, ID_NEXT);
    let env = build_env::acquire_packages(&mut second, deadline(&host), &manifest)
        .expect("a deleted cache is downloaded again");
    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 2);
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
    assert_eq!(
        host.file(&task_kit_file(ID_NEXT, DEB)).as_deref(),
        Some(&bytes[..])
    );
}

#[test]
fn the_offline_folder_is_used_before_the_cache() {
    let bytes = b"offline package bytes";
    let stale = b"stale cached package!!";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.put_file(&cache_file(DEB), stale.to_vec());
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("the offline package needs no network");

    assert_eq!(env.kit, vec![format!("/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 0);
    assert_eq!(host.file(&offline_file(DEB)).as_deref(), Some(&bytes[..]));
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&stale[..]));
    assert!(!host.exists(&task_kit_file(ID, DEB)));
}

fn cache_skip_lines(host: &FakeHost) -> Vec<serde_json::Value> {
    host.file_text(&format!("{HOST_DATA_DIR}/manager/state/{ID}.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|line: &serde_json::Value| line["key"] == "log.build_kit_cache")
        .collect()
}

fn assert_one_cache_skip(host: &FakeHost, reason: &str) {
    let lines = cache_skip_lines(host);
    assert_eq!(lines.len(), 1, "cache log lines: {lines:?}");
    assert_eq!(lines[0]["p"]["reason"]["key"], reason);
}

#[test]
fn an_unreadable_cache_directory_is_downloaded_and_the_build_continues() {
    let bytes = b"downloaded package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.fail_read_dir(&format!("{HOST_DATA_DIR}/{CACHE_DIR}"));
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("an unreadable cache is downloaded");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(
        host.file(&task_kit_file(ID, DEB)).as_deref(),
        Some(&bytes[..])
    );
    // Listing failed, but the directory can still take the verified download.
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
    assert_one_cache_skip(&host, REASON_PERMISSION_DENIED);
}

#[test]
fn an_undeletable_cache_file_outside_the_manifest_is_downloaded() {
    let bytes = b"cached package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let obsolete = "oldpkg_1.0-1_amd64.deb";
    let host = FakeHost::new();
    host.put_file(&cache_file(DEB), bytes.to_vec());
    host.put_file(&cache_file(obsolete), b"from the previous manifest");
    host.fail_remove(&cache_file(obsolete));
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("a cache file that cannot be deleted is downloaded");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    // A failed prune disables cache hits, so the matching file is not used.
    assert_eq!(host.count("curl"), 1);
    assert_eq!(
        host.file(&task_kit_file(ID, DEB)).as_deref(),
        Some(&bytes[..])
    );
    assert_eq!(
        host.file(&cache_file(obsolete)).as_deref(),
        Some(&b"from the previous manifest"[..])
    );
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
    assert_one_cache_skip(&host, REASON_PERMISSION_DENIED);
}

#[test]
fn a_cache_write_failure_keeps_the_downloaded_package() {
    let bytes = b"downloaded package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.fail_rename_contains("build-kit-cache");
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("a cache write failure still finishes the download");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(
        host.file(&task_kit_file(ID, DEB)).as_deref(),
        Some(&bytes[..])
    );
    assert!(!host.exists(&cache_file(DEB)));
    assert!(!host.exists(&cache_file(&format!("{DEB}.part"))));
    assert_eq!(
        host.children(&format!("{HOST_DATA_DIR}/{CACHE_DIR}")),
        Vec::<String>::new()
    );
    assert_one_cache_skip(&host, REASON_PERMISSION_DENIED);
}

#[test]
fn a_cache_hit_that_cannot_be_copied_is_downloaded() {
    let bytes = b"cached package bytes";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.put_file(&cache_file(DEB), bytes.to_vec());
    // The copy into the task directory syncs a hidden temporary name. Failing only that prefix
    // leaves the download and the later cache write alone.
    host.fail_sync_file(&format!(
        "{HOST_DATA_DIR}/manager/tasks/{ID}/in/build-kit/.{DEB}"
    ));
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("a cache copy failure is downloaded");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(
        host.file(&task_kit_file(ID, DEB)).as_deref(),
        Some(&bytes[..])
    );
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
    assert_one_cache_skip(&host, REASON_IO_ERROR);
}

fn cache_root() -> String {
    format!("{HOST_DATA_DIR}/{CACHE_DIR}")
}

/// Cleanup is a directory listing or a removal under the cache root. A rename into that root is a
/// cache write and is not cleanup.
fn assert_cache_not_cleaned(host: &FakeHost) {
    let root = cache_root();
    let prefix = format!("{root}/");
    let cleaned: Vec<_> = host
        .ops()
        .into_iter()
        .filter(|op| {
            (op.kind == "read_dir" || op.kind == "remove_file")
                && (op.path == root || op.path.starts_with(&prefix))
        })
        .collect();
    assert!(cleaned.is_empty(), "cache cleanup ran: {cleaned:?}");
}

#[test]
fn a_cache_root_symlink_into_the_offline_folder_is_not_cleaned() {
    let bytes = b"offline package bytes";
    let note = b"administrator note";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.put_file(&offline_file(DEB), bytes.to_vec());
    host.put_file(&offline_file("administrator-note.txt"), note.to_vec());
    host.put_link(&cache_root(), "build-kit");
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("the offline package is used");

    assert_eq!(env.kit, vec![format!("/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 0);
    assert_eq!(
        host.file(&offline_file("administrator-note.txt"))
            .as_deref(),
        Some(&note[..])
    );
    assert_eq!(host.file(&offline_file(DEB)).as_deref(), Some(&bytes[..]));
    assert_cache_not_cleaned(&host);
    assert_one_cache_skip(&host, REASON_IO_ERROR);
}

#[test]
fn a_cache_root_symlink_into_another_directory_is_not_cleaned() {
    let bytes = b"downloaded package bytes";
    let note = b"sentinel in another directory";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let elsewhere = format!("{HOST_DATA_DIR}/manager/elsewhere");
    let note_path = format!("{elsewhere}/administrator-note.txt");
    let host = FakeHost::new();
    host.put_file(&note_path, note.to_vec());
    host.put_link(&cache_root(), "elsewhere");
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("a linked cache is downloaded");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(
        host.file(&task_kit_file(ID, DEB)).as_deref(),
        Some(&bytes[..])
    );
    assert_eq!(host.file(&note_path).as_deref(), Some(&note[..]));
    assert!(!host.exists(&format!("{elsewhere}/{DEB}")));
    assert_cache_not_cleaned(&host);
    assert_one_cache_skip(&host, REASON_IO_ERROR);
}

#[test]
fn a_cache_root_that_is_a_regular_file_is_left_unchanged() {
    let bytes = b"downloaded package bytes";
    let sentinel = b"this file is the cache path";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let host = FakeHost::new();
    host.put_file(&cache_root(), sentinel.to_vec());
    host.set_curl(&manifest[0].url, 200, bytes);
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("a file at the cache path is downloaded");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 1);
    assert_eq!(
        host.file(&task_kit_file(ID, DEB)).as_deref(),
        Some(&bytes[..])
    );
    assert_eq!(host.file(&cache_root()).as_deref(), Some(&sentinel[..]));
    assert_cache_not_cleaned(&host);
    assert_one_cache_skip(&host, REASON_IO_ERROR);
}

#[test]
fn a_symlink_inside_the_cache_is_removed_without_its_target() {
    let bytes = b"cached package bytes";
    let sentinel = b"outside the cache";
    let manifest = kit_manifest(&[(DEB, bytes)]);
    let outside = format!("{HOST_DATA_DIR}/manager/outside-note.txt");
    let link_name = "oldpkg_1.0-1_amd64.deb";
    let host = FakeHost::new();
    host.put_file(&outside, sentinel.to_vec());
    host.put_file(&cache_file(DEB), bytes.to_vec());
    host.put_link(&cache_file(link_name), "../outside-note.txt");
    let mut ctx = ctx(&host, PlanSource::Official);
    let env = build_env::acquire_packages(&mut ctx, deadline(&host), &manifest)
        .expect("the cached package is used");

    assert_eq!(env.kit, vec![format!("/in/build-kit/{DEB}")]);
    assert_eq!(host.count("curl"), 0);
    assert!(!host.exists(&cache_file(link_name)));
    assert_eq!(host.file(&outside).as_deref(), Some(&sentinel[..]));
    assert_eq!(host.file(&cache_file(DEB)).as_deref(), Some(&bytes[..]));
}

#[test]
fn real_kernel_scenes_select_the_compiler_passed_to_the_container() {
    for (kernel, truenas, text, expected) in [
        (
            "6.18.52-production+truenas",
            "27.0.0-RC.1",
            "gcc (Debian 14.2.0-19) 14.2.0",
            "gcc-14",
        ),
        (
            KERNEL,
            TRUENAS_VERSION,
            "gcc (Debian 12.2.0-14+deb12u1) 12.2.0",
            "gcc-12",
        ),
    ] {
        let host = FakeHost::new();
        fake_host::prime_kernel_scene(&host, kernel, truenas, text);
        let mut ctx = ctx(&host, PlanSource::Official);
        let compiler = build_env::compiler(&ctx).unwrap();
        assert_eq!(compiler, expected);
        assert_eq!(
            truenas_nvidia_driver_manager::facts::truenas_version(&host)
                .value()
                .map(String::as_str),
            Some(truenas)
        );
        let _ = runner::build::build(&mut ctx, &prepared_env(), &compiler);
        assert!(docker_argvs(&host)
            .iter()
            .any(|args| args.contains(&format!("NVM_BUILD_COMPILER={expected}"))));
        assert_eq!(host.count("curl"), 0);
    }
}

#[test]
fn compiler_config_uses_header_priority_and_boot_fallback() {
    let host = FakeHost::new();
    let header = format!("{KERNEL_HEADERS_PATH}/include/config/auto.conf");
    host.put_text(
        &header,
        "CONFIG_CC_VERSION_TEXT=gcc (Debian 14.2.0-19) 14.2.0\n",
    );
    assert_eq!(
        build_env::compiler(&ctx(&host, PlanSource::Official)).unwrap(),
        "gcc-14"
    );
    // Unreadable/absent or non-GCC header text falls back to the boot config.
    for text in [
        None,
        Some("CONFIG_CC_VERSION_TEXT=clang 15.0.0\n"),
        Some("CONFIG_CC_VERSION_TEXT=gcc unknown\n"),
    ] {
        if let Some(text) = text {
            host.put_text(&header, text);
        } else {
            host.remove(&header);
        }
        assert_eq!(
            build_env::compiler(&ctx(&host, PlanSource::Official)).unwrap(),
            "gcc-12"
        );
    }
    host.put_text(&header, "CONFIG_CC_VERSION_TEXT=gcc 15.1.0\n");
    let failure = build_env::compiler(&ctx(&host, PlanSource::Official)).unwrap_err();
    assert_eq!(failure.params["reason"]["key"], "compiler_missing:gcc-15");
    host.remove(&header);
    host.remove(&format!("/boot/config-{KERNEL}"));
    let failure = build_env::compiler(&ctx(&host, PlanSource::Official)).unwrap_err();
    assert_eq!(failure.params["reason"]["key"], BUILD_KERNEL_GCC_UNKNOWN);
    assert_eq!(host.count("curl"), 0);
    assert_eq!(host.count("docker"), 0);
}

#[test]
fn compiler_requirement_is_read_from_the_host_view() {
    let host = FakeHost::new();
    host.set_view_prefix(Some("/host-view"));
    host.put_text(
        &format!("/host-view/boot/config-{KERNEL}"),
        "CONFIG_CC_VERSION_TEXT=gcc 14.2.0\n",
    );
    assert_eq!(
        build_env::compiler(&ctx(&host, PlanSource::Official)).unwrap(),
        "gcc-14"
    );
}

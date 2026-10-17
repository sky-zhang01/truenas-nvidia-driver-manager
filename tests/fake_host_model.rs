//! Regression tests for the fake host's sandbox mapping. A virtual path whose resolution cannot
//! finish inside the sandbox (a symlink loop) must not let a helper method fall back to a fixed
//! node and then do a real read, list, or delete on it: the resolve error is propagated, so the
//! helper refuses the operation instead of touching a real node that a test's own symlink could
//! have placed outside the sandbox. The controlled `Host` methods already return the resolve error;
//! these tests keep that positive control next to the helper contract.

mod common;

use common::*;
use std::path::Path;
use truenas_nvidia_driver_manager::host::Host;
use truenas_nvidia_driver_manager::store::random_hex;

/// A real node outside the fake sandbox, under the test's own temp dir (`TMPDIR`), so the sandbox
/// escape would be visible as a foreign read.
fn outside_path(suffix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("nvm-sentinel-{suffix}-{}", random_hex(8)))
}

#[test]
fn unresolved_sentinel_must_not_read_an_owned_outside_file() {
    let host = FakeHost::new();
    host.put_link("/loop", "/loop");
    assert!(host.read(Path::new("/loop")).is_err());
    assert!(host.read_dir(Path::new("/loop")).is_err());

    let owned = outside_path("file");
    assert!(
        !owned.starts_with(host.sandbox_root()),
        "the marker must live outside the sandbox: {owned:?}"
    );
    std::fs::write(&owned, b"OWNED outside file marker").unwrap();

    // A pre-existing fixed fallback node as a symlink out of the sandbox: a helper that read the
    // fallback path would follow it to the real file.
    host.put_link("/.fake-host-unresolvable", owned.to_str().unwrap());
    let bytes = host.file("/loop");
    let text = host.file_text("/loop");
    assert!(
        bytes.is_none(),
        "the helper read a real file outside the sandbox: {bytes:?}"
    );
    assert!(
        text.is_none(),
        "the helper read a real file outside the sandbox: {text:?}"
    );

    assert_eq!(std::fs::read(&owned).unwrap(), b"OWNED outside file marker");
    std::fs::remove_file(&owned).unwrap();
    host.remove("/.fake-host-unresolvable");
    host.assert_red_lines();
}

#[test]
fn unresolved_sentinel_must_not_list_an_owned_outside_directory() {
    let host = FakeHost::new();
    host.put_link("/loop", "/loop");
    assert!(host.read(Path::new("/loop")).is_err());
    assert!(host.read_dir(Path::new("/loop")).is_err());

    let owned = outside_path("dir");
    assert!(
        !owned.starts_with(host.sandbox_root()),
        "the marker must live outside the sandbox: {owned:?}"
    );
    std::fs::create_dir(&owned).unwrap();
    std::fs::write(owned.join("ONLY_OWNED_OUTSIDE_MARKER"), b"owned").unwrap();

    host.put_link("/.fake-host-unresolvable", owned.to_str().unwrap());
    let names = host.children("/loop");
    assert!(
        !names.iter().any(|n| n == "ONLY_OWNED_OUTSIDE_MARKER"),
        "the helper listed a real directory outside the sandbox: {names:?}"
    );
    assert!(names.is_empty(), "{names:?}");

    assert!(owned.join("ONLY_OWNED_OUTSIDE_MARKER").exists());
    std::fs::remove_file(owned.join("ONLY_OWNED_OUTSIDE_MARKER")).unwrap();
    std::fs::remove_dir(&owned).unwrap();
    host.remove("/.fake-host-unresolvable");
    host.assert_red_lines();
}

/// The other file helpers refuse an unresolvable path too: with an unresolvable intermediate
/// component, existence reads `false`, and removal does not act on the fallback node (a symlink to
/// a real directory outside the sandbox stays untouched, as does the sentinel node itself because
/// the virtual path never resolved).
#[test]
fn unresolved_helpers_refuse_instead_of_acting_on_a_fallback_node() {
    let host = FakeHost::new();
    host.put_link("/loop", "/loop");

    let owned = outside_path("tree");
    assert!(!owned.starts_with(host.sandbox_root()));
    std::fs::create_dir(&owned).unwrap();
    std::fs::write(owned.join("OWNED_MARKER"), b"owned").unwrap();
    host.put_link("/.fake-host-unresolvable", owned.to_str().unwrap());

    let indirect = "/loop/child";
    assert!(!host.exists(indirect));
    host.remove(indirect);
    host.remove_tree(indirect);
    assert!(
        owned.join("OWNED_MARKER").exists(),
        "a helper acted on a real directory outside the sandbox"
    );
    assert!(
        host.exists("/.fake-host-unresolvable"),
        "the sentinel symlink is a sandbox node and was not addressed"
    );

    std::fs::remove_file(owned.join("OWNED_MARKER")).unwrap();
    std::fs::remove_dir(&owned).unwrap();
    host.remove("/.fake-host-unresolvable");
    assert!(!host.exists("/.fake-host-unresolvable"));
    host.assert_red_lines();
}

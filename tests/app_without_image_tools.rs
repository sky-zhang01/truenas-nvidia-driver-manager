//! The web container uses the binary's namespace mode and runs host tools with host paths. Source
//! checks and real namespace-mode invocations enforce that boundary.

mod common;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use truenas_nvidia_driver_manager::codes::CANONICAL_PATH;
use truenas_nvidia_driver_manager::facts;
use truenas_nvidia_driver_manager::host::{self, At};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_truenas-nvidia-driver-manager"))
}

/// Every line of every source file of `src/`, with a flag that marks the lines of the top-level
/// `#[cfg(test)] mod tests {` module: those are test code and not production paths.
fn source_lines() -> Vec<(String, usize, String, bool)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&src)
        .expect("the source directory is readable")
        .map(|entry| entry.expect("a source entry").path())
        .filter(|path| path.extension() == Some(OsStr::new("rs")))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).expect("a source file is UTF-8");
        let lines: Vec<&str> = text.lines().collect();
        // The test module starts at a top-level `#[cfg(test)]` whose next line opens `mod tests`.
        // Its lines are the only ones the scan skips.
        let tests_at = lines.iter().enumerate().find_map(|(i, line)| {
            let opens_tests = lines
                .get(i + 1)
                .map(|next| next.trim_start().starts_with("mod tests"))
                .unwrap_or(false);
            (line.trim_end() == "#[cfg(test)]" && opens_tests).then_some(i)
        });
        let tests_at = tests_at.unwrap_or(lines.len());
        for (i, line) in lines.iter().enumerate() {
            out.push((name.clone(), i + 1, line.to_string(), i >= tests_at));
        }
    }
    out
}

#[test]
fn no_production_code_runs_a_program_in_the_container_itself() {
    let lines = source_lines();
    // Engine controls: the scan read the production region (two known production items are there)
    // and its needle exists in the skipped test modules, so an empty result below is an assertion
    // and not a broken read.
    let production: Vec<&(String, usize, String, bool)> =
        lines.iter().filter(|(_, _, _, tests)| !tests).collect();
    assert!(
        production
            .iter()
            .any(|(_, _, line, _)| line.contains("pub fn canonical_facts")),
        "the scan must read the production code"
    );
    assert!(
        production
            .iter()
            .any(|(_, _, line, _)| line.contains("fn build_command")),
        "the scan must read the command construction"
    );
    assert!(
        lines
            .iter()
            .any(|(_, _, line, tests)| *tests && line.contains("At::Here")),
        "the needle must be one the test modules really contain"
    );
    let hits: Vec<String> = production
        .iter()
        .filter(|(_, _, line, _)| line.contains("At::Here"))
        .map(|(file, n, line, _)| format!("{file}:{n}: {}", line.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "the web process runs host commands through its own namespace mode and \
         nothing in its own environment; found:\n{}",
        hits.join("\n")
    );
}

#[test]
fn the_internal_mode_rejects_a_call_without_a_program() {
    let out = Command::new(binary())
        .arg(host::INTERNAL_NSENTER_ARG)
        .output()
        .expect("the binary runs");
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage:"), "{stderr}");
    assert!(stderr.contains("task run <task id>"), "{stderr}");
    assert!(
        !stderr.contains(host::INTERNAL_NSENTER_ARG),
        "the internal argument stays out of the usage text: {stderr}"
    );
}

#[test]
fn the_internal_mode_failure_keeps_the_nsenter_shape() {
    // A name no `HOST_PATH` entry can hold. On a host whose namespaces are reachable the `execve`
    // fails; everywhere else entering them fails first. Both are the same contract: exit code 1 and
    // one line starting with `nsenter: `, which is what the start-up classification expects.
    let out = Command::new(binary())
        .arg(host::INTERNAL_NSENTER_ARG)
        .arg("nvm-no-such-host-command")
        .output()
        .expect("the binary runs");
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.starts_with(b"nsenter:"),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn the_canonical_listing_runs_unsquashfs_on_the_host_with_the_host_path() {
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    // Model the container: a read through `view` would carry this prefix.
    host.set_view_prefix(Some("/proc/1/root"));
    let facts = facts::canonical_facts(&host).expect("the listing is readable");
    assert_eq!(
        facts.file_version.value().map(String::as_str),
        Some("580.173.02")
    );
    let command = host
        .commands()
        .into_iter()
        .find(|c| c.program == "unsquashfs")
        .expect("unsquashfs ran");
    assert_eq!(command.at, At::Host, "the listing runs on the host");
    assert_eq!(
        command.args,
        vec!["-lls".to_string(), CANONICAL_PATH.to_string()],
        "the host path, not the container view of it"
    );
}

//! The web container uses the binary's namespace mode and runs host tools with host paths. Source
//! checks and real namespace-mode invocations enforce that boundary.

mod common;

use std::ffi::OsStr;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use syn::spanned::Spanned;
use syn::visit::Visit;
use truenas_nvidia_driver_manager::codes::CANONICAL_PATH;
use truenas_nvidia_driver_manager::facts;
use truenas_nvidia_driver_manager::host::{self, At};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_truenas-nvidia-driver-manager"))
}

type SourceLine = (String, usize, String, bool);

/// Source lines under `src/`, split at test-module boundaries so production code on the same
/// line as a test module stays visible. Paths are relative to `src/`.
fn source_lines() -> Vec<(String, usize, String, bool)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = source_files(&src);
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let name = path.strip_prefix(&src).unwrap().to_string_lossy();
        let text = std::fs::read_to_string(&path).expect("a source file is UTF-8");
        out.extend(classify_source(&name, &text));
    }
    out
}

fn source_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).expect("a source directory is readable") {
        let path = entry.expect("a source entry").path();
        if path.is_dir() {
            files.extend(source_files(&path));
        } else if path.extension() == Some(OsStr::new("rs")) {
            files.push(path);
        }
    }
    files
}

#[derive(Default)]
struct TestModules(Vec<Range<usize>>);

impl<'ast> Visit<'ast> for TestModules {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let test_only = item.attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Path>()
                    .is_ok_and(|path| path.is_ident("test"))
        });
        if test_only {
            self.0.push(item.span().byte_range());
        } else {
            syn::visit::visit_item_mod(self, item);
        }
    }
}

fn classify_source(name: &str, text: &str) -> Vec<SourceLine> {
    let syntax = syn::parse_file(text).expect("a source file parses as Rust");
    // syn strips these prefixes before parsing, so its spans start after them.
    let bom = "\u{feff}";
    let prefix = usize::from(text.starts_with(bom)) * bom.len()
        + syntax.shebang.as_ref().map_or(0, String::len);
    let mut modules = TestModules::default();
    modules.visit_file(&syntax);
    modules.0.sort_by_key(|range| range.start);
    let mut out = Vec::new();
    let (mut offset, mut number) = (0, 1);
    let mut append = |region: &str, tests| {
        for line in region.split_inclusive('\n') {
            out.push((name.to_string(), number, line.to_string(), tests));
            number += usize::from(line.ends_with('\n'));
        }
    };
    for range in modules.0 {
        let range = range.start + prefix..range.end + prefix;
        append(&text[offset..range.start], false);
        append(&text[range.clone()], true);
        offset = range.end;
    }
    append(&text[offset..], false);
    out
}

#[test]
fn source_scan_skips_only_test_module_items() {
    let text = r##"fn before() { let _ = "#[cfg(test)] mod tests {"; }
#[cfg(test)]
mod tests { fn test() { let _ = r#"} At::Here {"#; } }
fn after() { let _ = At::Here; }
mod nested {
    #[cfg(test)] mod checks { fn test() { let _ = At::Here; } }
    fn after_nested() { let _ = At::Here; }
}
#[cfg(test)] mod more { fn test() { let _ = At::Here; } } fn same_line() { let _ = At::Here; }
#[cfg(not(test))] mod tests { fn production() { let _ = At::Here; } }
"##;
    let lines = classify_source("fixture.rs", text);
    let hits: Vec<usize> = lines
        .iter()
        .filter(|(_, _, line, tests)| !tests && line.contains("At::Here"))
        .map(|(_, number, _, _)| *number)
        .collect();
    assert_eq!(hits, [4, 7, 9, 10]);
    assert_eq!(
        lines
            .iter()
            .filter(|(_, _, line, tests)| *tests && line.contains("At::Here"))
            .count(),
        3
    );
}

#[test]
fn source_scan_preserves_offsets_after_bom_and_shebang() {
    for prefix in [
        "\u{feff}",
        "#!/usr/bin/env rust-script\n",
        "\u{feff}#!/bin/runner\n",
    ] {
        let text = format!(
            "{prefix}//🙂\n#[cfg(test)] mod tests {{ fn test() {{ let _ = At::Here; }} }}\n\
             fn after() {{ let _ = At::Here; }}\n"
        );
        let lines = classify_source("fixture.rs", &text);
        let hits: Vec<_> = lines
            .iter()
            .filter(|(_, _, line, tests)| !tests && line.contains("At::Here"))
            .collect();
        assert_eq!(hits.len(), 1, "{prefix:?}");
        assert!(hits[0].2.contains("fn after"), "{prefix:?}");
        assert_eq!(hits[0].1, 3 + prefix.matches('\n').count());
    }
}

#[test]
fn source_scan_reads_nested_production() {
    assert!(source_lines().iter().any(|(file, _, line, tests)| {
        file == "runner/build.rs" && !tests && line.contains("pub fn docker_run_argv")
    }));
}

#[test]
fn source_scan_reads_production_after_a_test_module() {
    assert!(source_lines().iter().any(|(file, _, line, tests)| {
        file == "tasks.rs" && !tests && line.contains("fn is_sixteen_hex")
    }));
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

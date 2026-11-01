//! The vendored upstream files, embedded in the binary and written into a task's input.
//!
//! The build container has no network, so the module source travels with the program. It is
//! copied unchanged from upstream into `third_party/gpu-unlock/`.

use std::io;
use std::path::Path;

use crate::host::Host;
use crate::store;

macro_rules! vendored {
    ($($path:literal,)*) => {
        &[$((
            $path,
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/third_party/gpu-unlock/", $path))
                as &[u8],
        ),)*]
    };
}

/// Every vendored file, by its path under `third_party/gpu-unlock/`.
#[rustfmt::skip]
pub const FILES: &[(&str, &[u8])] = vendored![
    "firmware/NOTICE.md",
    "kernel/gpu_unlock/Kbuild", "kernel/gpu_unlock/Makefile",
    "kernel/gpu_unlock/device.h", "kernel/gpu_unlock/diagnostic.h",
    "kernel/gpu_unlock/falcon.c", "kernel/gpu_unlock/falcon.h",
    "kernel/gpu_unlock/firmware_610.c", "kernel/gpu_unlock/firmware_610.h",
    "kernel/gpu_unlock/gfw_eval.c", "kernel/gpu_unlock/gfw_eval.h",
    "kernel/gpu_unlock/layout_610.c", "kernel/gpu_unlock/layout_610.h",
    "kernel/gpu_unlock/main.c",
    "kernel/gpu_unlock/payload.c", "kernel/gpu_unlock/payload.h",
    "kernel/gpu_unlock/prep_610.c", "kernel/gpu_unlock/prep_610.h",
    "kernel/gpu_unlock/recipes.c", "kernel/gpu_unlock/recipes.h",
    "kernel/gpu_unlock/registers.h",
    "kernel/gpu_unlock/unlock_610.c", "kernel/gpu_unlock/unlock_610.h",
    "kernel/gpu_unlock/vbios_610.c", "kernel/gpu_unlock/vbios_610.h",
    "licenses/CMP40HX-MIT.txt", "licenses/GPL-2.0.txt", "licenses/NVIDIA-MIT.txt",
];

/// Write the vendored files and the rule for `targets` under `dir`, keeping the upstream
/// layout. The caller checks `targets` with [`super::targets_ok`] first.
pub fn write_kit(h: &dyn Host, dir: &Path, targets: &[String]) -> io::Result<()> {
    for (path, bytes) in FILES {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            h.create_dir(parent, 0o755)?;
        }
        store::write_atomic(h, &file, bytes, 0o644)?;
    }
    store::write_atomic(
        h,
        &dir.join(super::RULE_FILE_NAME),
        super::rule_file(targets).as_bytes(),
        0o644,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RealHost;
    use crate::test_support::TestDir;

    fn files_under(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files_under(root, &path, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }

    #[test]
    fn the_embedded_list_is_exactly_the_vendored_directory() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/gpu-unlock");
        let mut on_disk = Vec::new();
        files_under(&root, &root, &mut on_disk);
        // Our record of the upstream files the firmware generation follows, not upstream content.
        on_disk.retain(|path| path != "UPSTREAM.tsv");
        on_disk.sort_unstable();
        let mut embedded: Vec<String> = FILES.iter().map(|(path, _)| path.to_string()).collect();
        embedded.sort_unstable();
        assert_eq!(embedded, on_disk);
        assert_eq!(FILES.len(), 28);
        assert!(FILES.iter().all(|(_, bytes)| !bytes.is_empty()));
    }

    #[test]
    fn the_kit_is_written_with_the_upstream_layout_and_the_rule() {
        let dir = TestDir::new("gpu-unlock-kit");
        let targets = vec!["0000:01:00.0".to_string()];
        write_kit(&RealHost::on_host(), &dir.join("gpu-unlock"), &targets).unwrap();
        for (path, bytes) in FILES {
            assert_eq!(
                &std::fs::read(dir.join("gpu-unlock").join(path)).unwrap(),
                bytes
            );
        }
        assert_eq!(
            std::fs::read_to_string(
                dir.join("gpu-unlock")
                    .join(crate::gpu_unlock::RULE_FILE_NAME)
            )
            .unwrap(),
            crate::gpu_unlock::rule_file(&targets)
        );
    }
}

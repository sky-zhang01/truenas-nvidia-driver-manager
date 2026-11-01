//! Optional full-rate tensor unlock with the upstream gpu-unlock module.
//!
//! A build with the unlock carries the upstream `gpu_unlock` kernel module, its firmware and a
//! modprobe rule that loads `vfio-pci` and `gpu_unlock` before `nvidia`. This module holds what
//! the later stages share: the pinned upstream, the supported drivers, the fixed values of the
//! firmware input and outputs, the unlock targets and the full text of the rule.

pub mod artifact;
pub mod boot;
pub mod cards;
pub mod firmware;
pub mod kit;
pub mod resolve;
pub mod status;

/// The upstream vendored in `third_party/gpu-unlock/`.
pub const UPSTREAM: &str = "Zagitta/gpu-unlock";
pub const UPSTREAM_VERSION: &str = "2.0.0";
pub const UPSTREAM_COMMIT: &str = "1fc6d628c49c7fba34ef3a2e7f758700d35ce890";

/// The NVIDIA release the firmware is extracted from. It does not depend on the driver the
/// user installs.
pub const FIRMWARE_VERSION: &str = "610.43.03";

/// The driver releases an update with the unlock may install, at most one per major version.
/// A release is added once the unlock has been verified with it.
pub const DRIVER_VERSIONS: &[&str] = &["610.43.03"];

/// The installer's license agreement, kept next to the firmware.
pub const FIRMWARE_LICENSE: &str = "NVIDIA-LICENSE";

/// The file name of the rule, in the task input and in the image's `usr/lib/modprobe.d/`.
pub const RULE_FILE_NAME: &str = "gpu-unlock.conf";

/// The module in an image built for `kernel`.
pub fn module_member(kernel: &str) -> String {
    format!("usr/lib/modules/{kernel}/extra/gpu_unlock.ko")
}

/// The rule in an image.
pub fn rule_member() -> String {
    format!("usr/lib/modprobe.d/{RULE_FILE_NAME}")
}

/// One file pinned by name, byte count and SHA-256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedFile {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// The NVIDIA installer the firmware is extracted from. The values equal NVIDIA's published
/// `.sha256sum` file.
#[rustfmt::skip]
pub const INSTALLER: FixedFile = FixedFile { name: "NVIDIA-Linux-x86_64-610.43.03-no-compat32.run", bytes: 344_190_507, sha256: "e084451db730a6df29d3d74f655749c4d236e3c945f7d94b5205e84318fe2f90" };

/// The exact content of the firmware directory: the 16 files the module requests and the
/// NVIDIA license. Upstream's scripts produce these bytes from the 610.43.03 installer and kernel
/// module source; [`firmware::generate`] produces them from [`INSTALLER`] alone.
#[rustfmt::skip]
pub const OUTPUTS: [FixedFile; 17] = [
    FixedFile { name: "NVIDIA-LICENSE", bytes: 22_653, sha256: "c692fb14ad4f499c6aee1b9e144e6905148c420e6225b17cb2937366c1e507ad" },
    FixedFile { name: "booter_load_ad102.bin", bytes: 57_672, sha256: "6ee05b20e1dc244c08c052f36ef28469b8d09f4955d1fe33e48a179e11a7fd49" },
    FixedFile { name: "booter_load_ga100.bin", bytes: 60_616, sha256: "b6e70d37fdbd23f3dee008165d9b3510e8faeb32a60a1689fb098e18d0c721bf" },
    FixedFile { name: "booter_load_ga102.bin", bytes: 61_256, sha256: "056cbc064a888f80717726c54d7f64fcd68044d123870c95a74f37f4975dcaa4" },
    FixedFile { name: "booter_load_tu102.bin", bytes: 59_224, sha256: "51507de61777cf46f3703a084cdfa896c21f6bc0c8af8f245bd09b2e9c49bf54" },
    FixedFile { name: "falcon_loader.bin", bytes: 768, sha256: "f21f1cfbb8fffa8067685314367fc4c50f3f84dd99eda7c19db4292d9269b9d9" },
    FixedFile { name: "gsp_boot_ad102.bin", bytes: 36_864, sha256: "45d6335a8185d6ff13e8d1484cea73f68d3d13aa1c19dc080ed1c3482dd5bac0" },
    FixedFile { name: "gsp_boot_desc_ad102.bin", bytes: 84, sha256: "83f8f3f1f5f9496e0ea8fc7103c2beb64d0b6de2f5ffe725f13514de5a95fa42" },
    FixedFile { name: "gsp_boot_desc_ga100.bin", bytes: 76, sha256: "9e7cca2cfdc5ce601b0adba39967850046108df6c2d23fff95578ca3f0ccbe01" },
    FixedFile { name: "gsp_boot_desc_ga102.bin", bytes: 84, sha256: "cc5a3fc1f4c47f2fe107b1b68f0176d8a448e6dec84b575e5560ccef5262bf15" },
    FixedFile { name: "gsp_boot_desc_tu102.bin", bytes: 76, sha256: "9e7cca2cfdc5ce601b0adba39967850046108df6c2d23fff95578ca3f0ccbe01" },
    FixedFile { name: "gsp_boot_ga100.bin", bytes: 4_096, sha256: "b80c6623f09df2f155ab335095575c637aef5e66cba6542e047422a6bab094e4" },
    FixedFile { name: "gsp_boot_ga102.bin", bytes: 24_576, sha256: "3df651acb174fd5f5561a1996965b192f50c5d763168446ed0d4ead19d895768" },
    FixedFile { name: "gsp_boot_tu102.bin", bytes: 4_096, sha256: "8765694cd38beaa4e2b8b1b83a31fb7aee6cb9d8b7c6bc7a86e6d931a3a056c2" },
    FixedFile { name: "gsp_ga10x.bin", bytes: 84_277_400, sha256: "572373620a37418f24dc16b5031c39338778c3257e48e8408de9a57291b24f3a" },
    FixedFile { name: "gsp_tu10x.bin", bytes: 29_352_832, sha256: "73065619db9ec921d19fc4e519dd04d91a9199b525eaca9b257b89fb8c5ec52c" },
    FixedFile { name: "sec2_bl_tu102.bin", bytes: 768, sha256: "f21f1cfbb8fffa8067685314367fc4c50f3f84dd99eda7c19db4292d9269b9d9" },
];

/// The 16 firmware file names the module requests: [`OUTPUTS`] without the license.
pub fn firmware_names() -> impl Iterator<Item = &'static str> {
    OUTPUTS
        .iter()
        .map(|file| file.name)
        .filter(|name| *name != FIRMWARE_LICENSE)
}

/// Unlock targets: at least one PCI address in the lowercase sysfs form, each once. The
/// addresses go into the rule verbatim, so a value that is not exactly an address is refused
/// before it can add a line to the rule.
pub fn targets_ok(targets: &[String]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    !targets.is_empty()
        && targets
            .iter()
            .all(|t| crate::facts::pci_address_ok(t) && seen.insert(t.as_str()))
}

/// The full text of the rule for `targets`. `vfio-pci` stays first so that a GPU isolated for
/// a virtual machine is still bound to it before `nvidia` loads; this rule replaces the TrueNAS
/// rule of the same module.
pub fn rule_file(targets: &[String]) -> String {
    format!(
        "# Written by truenas-nvidia-driver-manager: gpu-unlock {UPSTREAM_VERSION} ({UPSTREAM} {})\n\
         softdep nvidia pre: vfio-pci gpu_unlock\n\
         options gpu_unlock targets={}\n",
        &UPSTREAM_COMMIT[..8],
        targets.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_rule_text_names_the_upstream_and_keeps_vfio_pci_first() {
        assert_eq!(
            rule_file(&strings(&["0000:01:00.0", "0000:02:00.0"])),
            "# Written by truenas-nvidia-driver-manager: gpu-unlock 2.0.0 (Zagitta/gpu-unlock 1fc6d628)\n\
             softdep nvidia pre: vfio-pci gpu_unlock\n\
             options gpu_unlock targets=0000:01:00.0,0000:02:00.0\n"
        );
    }

    #[test]
    fn targets_are_distinct_sysfs_addresses() {
        assert!(targets_ok(&strings(&["0000:01:00.0"])));
        assert!(targets_ok(&strings(&["0000:01:00.0", "0000:81:00.0"])));
        assert!(!targets_ok(&[]));
        assert!(!targets_ok(&strings(&["0000:01:00.0", "0000:01:00.0"])));
        assert!(!targets_ok(&strings(&["0000:01:00.0", ""])));
        assert!(!targets_ok(&strings(&["0000:0A:00.0"])));
        assert!(!targets_ok(&strings(&["all"])));
        assert!(!targets_ok(&strings(&["0000:01:00.0,0000:02:00.0"])));
        assert!(!targets_ok(&strings(&[
            "0000:01:00.0\nsoftdep nvidia pre: x"
        ])));
    }

    #[test]
    fn the_fixed_values_name_each_file_once_with_a_full_digest() {
        let mut names: Vec<&str> = OUTPUTS.iter().map(|f| f.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), OUTPUTS.len());
        for file in OUTPUTS.iter().chain([&INSTALLER]) {
            assert!(file.bytes > 0, "{}", file.name);
            assert_eq!(file.sha256.len(), 64, "{}", file.name);
            assert!(
                file.sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{}",
                file.name
            );
        }
        assert!(INSTALLER.name.contains(FIRMWARE_VERSION));
        assert_eq!(firmware_names().count(), 16);
    }

    #[test]
    fn the_supported_drivers_are_releases_one_per_major_version() {
        let mut majors: Vec<u32> = DRIVER_VERSIONS
            .iter()
            .map(|v| crate::facts::parse_version(v).expect(v).major())
            .collect();
        majors.sort_unstable();
        majors.dedup();
        assert_eq!(majors.len(), DRIVER_VERSIONS.len());
        assert!(DRIVER_VERSIONS.contains(&FIRMWARE_VERSION));
    }

    #[test]
    fn the_upstream_record_names_the_vendored_commit_and_files() {
        // UPSTREAM.tsv is what the scheduled check compares with upstream; the files that are
        // vendored must still be the recorded ones.
        use sha2::Digest;
        let table = include_str!("../../third_party/gpu-unlock/UPSTREAM.tsv");
        let rows: Vec<(&str, &str)> = table
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.split_once('\t').unwrap())
            .collect();
        assert_eq!(rows[0], ("repository", UPSTREAM));
        assert_eq!(rows[1], ("commit", UPSTREAM_COMMIT));
        let mut vendored = 0;
        for (path, sha256) in &rows[2..] {
            if let Some((_, bytes)) = kit::FILES.iter().find(|(p, _)| p == path) {
                assert_eq!(
                    crate::store::hex(&sha2::Sha256::digest(bytes)),
                    *sha256,
                    "{path}"
                );
                vendored += 1;
            }
        }
        assert_eq!((rows.len() - 2, vendored), (6, 2));
    }

    #[test]
    fn the_firmware_names_are_the_ones_the_vendored_module_requests() {
        // recipes.h names every firmware file as "gpu-unlock/<version>/<name>"; the fixed
        // table must list exactly those names, or a build would ship firmware the module
        // never loads and miss one it does.
        let header = kit::FILES
            .iter()
            .find(|(path, _)| *path == "kernel/gpu_unlock/recipes.h")
            .map(|(_, bytes)| std::str::from_utf8(bytes).unwrap())
            .unwrap();
        let prefix = format!("\"gpu-unlock/{FIRMWARE_VERSION}/");
        let mut requested: Vec<&str> = header
            .lines()
            .filter_map(|line| {
                let start = line.find(&prefix)? + prefix.len();
                let rest = &line[start..];
                Some(&rest[..rest.find('"')?])
            })
            .collect();
        requested.sort_unstable();
        let mut fixed: Vec<&str> = firmware_names().collect();
        fixed.sort_unstable();
        assert_eq!(requested, fixed);
    }
}

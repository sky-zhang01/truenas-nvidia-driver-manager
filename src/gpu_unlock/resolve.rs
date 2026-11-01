//! The load order that `modprobe --show-depends nvidia` reports.
//!
//! The output is what kmod would do at the next boot with the merged rules and the dependency
//! index. It lists modules that are loaded already and loads nothing.

/// One line of the output: a module kmod would insert, or one built into the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The module name with `-` written as `_`.
    pub name: String,
    /// The parameters of an `insmod` line.
    pub params: String,
}

/// The steps of `output`, or `None` when a line is not `insmod <path>[ <params>]` or
/// `builtin <name>`, or when `nvidia` is not listed.
pub fn parse(output: &str) -> Option<Vec<Step>> {
    let mut steps = Vec::new();
    for line in output.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        steps.push(match line.strip_prefix("insmod ") {
            Some(rest) => {
                let (path, params) = rest.split_once(' ').unwrap_or((rest, ""));
                let file = path.rsplit('/').next()?;
                let file = [".xz", ".zst", ".gz"]
                    .iter()
                    .find_map(|s| file.strip_suffix(s))
                    .unwrap_or(file);
                let name = file.strip_suffix(".ko").filter(|n| !n.is_empty())?;
                Step {
                    name: name.replace('-', "_"),
                    params: params.trim().to_string(),
                }
            }
            None => {
                let name = line.strip_prefix("builtin ")?.trim();
                if name.is_empty() || name.contains(char::is_whitespace) {
                    return None;
                }
                Step {
                    name: name.replace('-', "_"),
                    params: String::new(),
                }
            }
        });
    }
    steps.iter().any(|s| s.name == "nvidia").then_some(steps)
}

/// The two items of the load order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Judged {
    pub vfio_pci_first: bool,
    pub gpu_unlock_before_nvidia: bool,
}

/// Judge `steps` (`None` when the command or the parse failed). `isolation` is whether
/// `/etc/modprobe.d/nvidia.conf` exists, `None` when that could not be read; `targets` are the
/// planned unlock targets, empty when only the order counts.
pub fn judge(steps: Option<&[Step]>, isolation: Option<bool>, targets: &[String]) -> Judged {
    let Some(steps) = steps else {
        return Judged {
            vfio_pci_first: false,
            gpu_unlock_before_nvidia: false,
        };
    };
    let at = |name: &str| steps.iter().position(|s| s.name == name);
    let nvidia = at("nvidia");
    let before_nvidia = |i: usize| nvidia.is_some_and(|n| i < n);
    let vfio_pci_first = match at("vfio_pci") {
        Some(v) => before_nvidia(v) && at("gpu_unlock").is_none_or(|g| v < g),
        None => isolation == Some(false),
    };
    let wanted = format!("targets={}", targets.join(","));
    let gpu_unlock_before_nvidia = at("gpu_unlock").is_some_and(|g| {
        let mut given = steps[g]
            .params
            .split_whitespace()
            .filter(|p| p.starts_with("targets="));
        before_nvidia(g)
            && (targets.is_empty()
                || (given.next() == Some(wanted.as_str()) && given.next().is_none()))
    });
    Judged {
        vfio_pci_first,
        gpu_unlock_before_nvidia,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGETS: &str = "0000:01:00.0,0000:02:00.0";

    fn targets() -> Vec<String> {
        TARGETS.split(',').map(str::to_string).collect()
    }

    fn judged(output: &str, isolation: Option<bool>, targets: &[String]) -> (bool, bool) {
        let steps = parse(output);
        let j = judge(steps.as_deref(), isolation, targets);
        (j.vfio_pci_first, j.gpu_unlock_before_nvidia)
    }

    fn order(lines: &[&str]) -> String {
        lines.iter().map(|l| format!("{l} \n")).collect()
    }

    const VFIO: &str = "insmod /lib/modules/6.12/kernel/drivers/vfio/pci/vfio-pci.ko";
    const UNLOCK: &str =
        "insmod /lib/modules/6.12/extra/gpu_unlock.ko targets=0000:01:00.0,0000:02:00.0";
    const NVIDIA: &str = "insmod /lib/modules/6.12/kernel/drivers/video/nvidia.ko";

    #[test]
    fn the_expected_order_passes_both_items() {
        let output = order(&[VFIO, UNLOCK, NVIDIA]);
        assert_eq!(judged(&output, Some(true), &targets()), (true, true));
    }

    #[test]
    fn the_real_output_of_truenas_25_10_7_passes_both_items() {
        // Verbatim output of `modprobe --show-depends nvidia` on TrueNAS 25.10.7 (kmod 30) with an image
        // built with the unlock: `drm` first, `vfio` twice, a space ending every line.
        let output: String = [
            "insmod /lib/modules/6.12.105-production+truenas/kernel/drivers/gpu/drm/drm.ko ",
            "insmod /lib/modules/6.12.105-production+truenas/kernel/virt/lib/irqbypass.ko ",
            "insmod /lib/modules/6.12.105-production+truenas/kernel/drivers/vfio/vfio.ko ",
            "insmod /lib/modules/6.12.105-production+truenas/kernel/drivers/vfio/vfio.ko ",
            "insmod /lib/modules/6.12.105-production+truenas/kernel/drivers/vfio/vfio_iommu_type1.ko ",
            "insmod /lib/modules/6.12.105-production+truenas/kernel/drivers/vfio/pci/vfio-pci-core.ko ",
            "insmod /lib/modules/6.12.105-production+truenas/kernel/drivers/vfio/pci/vfio-pci.ko ",
            "insmod /lib/modules/6.12.105-production+truenas/extra/gpu_unlock.ko targets=0000:00:1f.7 ",
            "insmod /lib/modules/6.12.105-production+truenas/kernel/drivers/video/nvidia.ko ",
        ]
        .iter()
        .map(|l| format!("{l}\n"))
        .collect();
        let real = vec!["0000:00:1f.7".to_string()];
        for isolation in [Some(false), Some(true), None] {
            assert_eq!(
                judged(&output, isolation, &real),
                (true, true),
                "{isolation:?}"
            );
        }
    }

    #[test]
    fn the_real_output_of_truenas_27_0_0_rc_1_passes_both_items() {
        // Verbatim output of `modprobe --show-depends nvidia` on TrueNAS 27.0.0-RC.1 (kmod 34.2) with an image
        // built with the unlock: `drm` first, `vfio` twice, a space ending every line.
        let output: String = [
            "insmod /lib/modules/6.18.52-production+truenas/kernel/drivers/gpu/drm/drm.ko ",
            "insmod /lib/modules/6.18.52-production+truenas/kernel/virt/lib/irqbypass.ko ",
            "insmod /lib/modules/6.18.52-production+truenas/kernel/drivers/vfio/vfio.ko ",
            "insmod /lib/modules/6.18.52-production+truenas/kernel/drivers/vfio/vfio.ko ",
            "insmod /lib/modules/6.18.52-production+truenas/kernel/drivers/vfio/vfio_iommu_type1.ko ",
            "insmod /lib/modules/6.18.52-production+truenas/kernel/drivers/vfio/pci/vfio-pci-core.ko ",
            "insmod /lib/modules/6.18.52-production+truenas/kernel/drivers/vfio/pci/vfio-pci.ko ",
            "insmod /lib/modules/6.18.52-production+truenas/extra/gpu_unlock.ko targets=0000:00:1f.7 ",
            "insmod /lib/modules/6.18.52-production+truenas/kernel/drivers/video/nvidia.ko ",
        ]
        .iter()
        .map(|l| format!("{l}\n"))
        .collect();
        let real = vec!["0000:00:1f.7".to_string()];
        for isolation in [Some(false), Some(true), None] {
            assert_eq!(
                judged(&output, isolation, &real),
                (true, true),
                "{isolation:?}"
            );
        }
    }

    #[test]
    fn a_builtin_vfio_pci_is_the_same_module() {
        let output = order(&["builtin vfio_pci", UNLOCK, NVIDIA]);
        assert_eq!(judged(&output, Some(true), &targets()), (true, true));
    }

    #[test]
    fn compressed_module_files_name_the_same_modules() {
        let output = order(&[
            "insmod /m/vfio-pci.ko.zst",
            "insmod /m/gpu_unlock.ko.xz targets=0000:01:00.0,0000:02:00.0",
            "insmod /m/nvidia.ko.gz",
        ]);
        assert_eq!(judged(&output, None, &targets()), (true, true));
    }

    #[test]
    fn without_a_vfio_pci_line_the_isolation_file_decides() {
        let output = order(&[UNLOCK, NVIDIA]);
        assert_eq!(judged(&output, Some(false), &targets()), (true, true));
        assert_eq!(judged(&output, Some(true), &targets()), (false, true));
        assert_eq!(judged(&output, None, &targets()), (false, true));
    }

    #[test]
    fn vfio_pci_must_come_before_gpu_unlock_and_nvidia() {
        let late = order(&[UNLOCK, VFIO, NVIDIA]);
        assert_eq!(judged(&late, Some(true), &targets()), (false, true));
        let last = order(&[NVIDIA, VFIO]);
        assert!(!judged(&last, Some(true), &targets()).0);
    }

    #[test]
    fn output_that_does_not_parse_fails_both_items() {
        for output in [
            "",
            "modprobe: ERROR: could not find module nvidia\n",
            "insmod /m/nvidia.so\n",
            "builtin\n",
            "builtin vfio pci\n",
            "alias nvidia nvidia\n",
            "insmod /m/vfio-pci.ko\n",
        ] {
            assert_eq!(
                judged(output, Some(false), &targets()),
                (false, false),
                "{output}"
            );
        }
        assert_eq!(parse("\n  \n"), None);
    }

    #[test]
    fn a_shadowed_rule_leaves_gpu_unlock_out_of_the_order() {
        let output = order(&[VFIO, NVIDIA]);
        assert_eq!(judged(&output, Some(true), &targets()), (true, false));
    }

    #[test]
    fn gpu_unlock_after_nvidia_does_not_count() {
        let output = order(&[VFIO, NVIDIA, UNLOCK]);
        assert_eq!(judged(&output, Some(true), &targets()), (true, false));
    }

    #[test]
    fn the_targets_must_be_the_planned_ones_exactly_once() {
        let wrong = order(&[VFIO, "insmod /m/gpu_unlock.ko targets=0000:01:00.0", NVIDIA]);
        assert_eq!(judged(&wrong, Some(true), &targets()), (true, false));
        let twice = order(&[
            VFIO,
            "insmod /m/gpu_unlock.ko targets=0000:01:00.0,0000:02:00.0 targets=0000:01:00.0,0000:02:00.0",
            NVIDIA,
        ]);
        assert_eq!(judged(&twice, Some(true), &targets()), (true, false));
        let none = order(&[VFIO, "insmod /m/gpu_unlock.ko", NVIDIA]);
        assert_eq!(judged(&none, Some(true), &targets()), (true, false));
    }

    #[test]
    fn a_saved_original_without_targets_only_needs_the_order() {
        let output = order(&[VFIO, "insmod /m/gpu_unlock.ko targets=0000:09:00.0", NVIDIA]);
        assert_eq!(judged(&output, Some(true), &[]), (true, true));
        let output = order(&[VFIO, "insmod /m/gpu_unlock.ko", NVIDIA]);
        assert_eq!(judged(&output, Some(true), &[]), (true, true));
        assert_eq!(
            judged(&order(&[VFIO, NVIDIA]), Some(true), &[]),
            (true, false)
        );
    }
}

//! Generated-table facts that the fixture table in `catalog` does not cover.

use truenas_nvidia_driver_manager::catalog::{first_of, fit, GpuFit};
use truenas_nvidia_driver_manager::facts::parse_version;
use truenas_nvidia_driver_manager::gpu_table::{GPUS, VERSIONS};

#[test]
fn rtx_pro_5500_blackwell_workstation_starts_at_620() {
    // NVIDIA lists 2BBD on the 620.70.06 supported-chips page and not on 615.71.09.
    let first = first_of(GPUS, VERSIONS, 0x2BBD).expect("0x2BBD is listed");
    assert_eq!(first.text(), "620.70.06");
    assert_eq!(
        fit(GPUS, VERSIONS, 0x2BBD, &parse_version("620.70.06").unwrap()),
        GpuFit::Supported,
    );
    let mut seen = false;
    for text in VERSIONS {
        let version = parse_version(text).unwrap();
        if version.major() != 615 {
            continue;
        }
        seen = true;
        assert_eq!(
            fit(GPUS, VERSIONS, 0x2BBD, &version),
            GpuFit::Newer {
                first: first.clone()
            },
            "{text}",
        );
    }
    assert!(seen, "VERSIONS includes a 615 release");
}

#[test]
fn gb300_is_newer_before_580_196_07_and_supported_from_that_release() {
    // NVIDIA lists 31C3 on the 580.196.07 supported-chips page and not on 580.178.04.
    let first = first_of(GPUS, VERSIONS, 0x31C3).expect("0x31C3 is listed");
    assert_eq!(first.text(), "580.196.07");
    assert_eq!(
        fit(
            GPUS,
            VERSIONS,
            0x31C3,
            &parse_version("580.178.04").unwrap()
        ),
        GpuFit::Newer {
            first: first.clone()
        },
    );
    assert_eq!(
        fit(
            GPUS,
            VERSIONS,
            0x31C3,
            &parse_version("580.196.07").unwrap()
        ),
        GpuFit::Supported,
    );
}

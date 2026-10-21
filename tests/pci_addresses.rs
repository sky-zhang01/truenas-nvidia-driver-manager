use serde_json::json;
use truenas_nvidia_driver_manager::plan::{self, SourceRequest};

#[test]
fn pci_address_boundaries_match_plan_request_validation() {
    let cases = [
        ("0000:0a:ff.7", true),
        ("00000000:0a:00.0", true),
        ("00000:01:00.0", true),
        ("ffff:ff:ff.0", true),
        ("0000:0A:00.0", false),
        ("0000:ab:CD.0", false),
        (" 0000:01:00.0", false),
        ("0000:01:00.0 ", false),
        ("0000:01:00.0\n", false),
        ("0000:01:00.0\0", false),
        ("\u{ff10}\u{ff10}\u{ff10}\u{ff10}:01:00.0", false),
        ("0000:\u{ff10}1:00.0", false),
        ("0000:01:00.\u{ff10}", false),
        ("000:01:00.0", false),
        ("000000000:01:00.0", false),
        ("0000:1:00.0", false),
        ("0000:001:00.0", false),
        ("0000:01:0.0", false),
        ("0000:01:000.0", false),
        ("0000:01:00.8", false),
        ("0000:01:00.00", false),
        ("0000:01:00.", false),
        ("0000:01:00.0:00", false),
        ("0000:01:00.0.0", false),
        ("0000::01:00.0", false),
        ("", false),
        ("\u{00e9}000:01:00.0", false),
    ];
    for (address, valid) in cases {
        assert_eq!(plan::pci_address_ok(address), valid, "{address:?}");
        for source in ["official", "upload"] {
            let mut request = json!({"kind": source, "version": "580.178.04", "gpus": [address]});
            if source == "upload" {
                request["group_id"] = json!("group");
            }
            assert_eq!(
                serde_json::from_value::<SourceRequest>(request).is_ok(),
                valid,
                "{source}: {address:?}"
            );
        }
    }
}

#[test]
fn request_addresses_remain_unique() {
    for source in ["official", "upload"] {
        for (gpus, valid) in [
            (vec![], true),
            (vec!["0000:01:00.0", "0000:02:00.0"], true),
            (vec!["0000:01:00.0", "0000:01:00.0"], false),
        ] {
            let mut request = json!({"kind": source, "version": "580.178.04", "gpus": gpus});
            if source == "upload" {
                request["group_id"] = json!("group");
            }
            assert_eq!(
                serde_json::from_value::<SourceRequest>(request).is_ok(),
                valid
            );
        }
    }
}

//! The GPUs the vendored gpu-unlock supports, by PCI device ID.
//!
//! Copied by hand from the `cards[]` table of `kernel/gpu_unlock/recipes.c` (lines 387-435 at
//! upstream commit `1fc6d628`). The maturity follows the upstream README ("Supported GPUs").
//! A test pins the SHA-256 of the vendored `recipes.c`: replacing the upstream files fails it
//! until this table is checked against the new file.

/// How far upstream has validated the unlock on a GPU generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Maturity {
    Stable,
    Experimental,
}

impl Maturity {
    /// The value of a GPU's `gpu_unlock` field in the versions response.
    pub fn as_str(self) -> &'static str {
        match self {
            Maturity::Stable => "stable",
            Maturity::Experimental => "experimental",
        }
    }
}

/// The GPU series a user knows a card by, in the order the interface lists them: most stable
/// first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Series {
    Rtx30,
    Rtx40,
    Rtx20,
}

impl Series {
    pub fn maturity(self) -> Maturity {
        match self {
            Series::Rtx30 => Maturity::Stable,
            Series::Rtx40 | Series::Rtx20 => Maturity::Experimental,
        }
    }

    /// The series name as the confirmation shows it.
    pub fn label(self) -> &'static str {
        match self {
            Series::Rtx30 => "RTX 30",
            Series::Rtx40 => "RTX 40",
            Series::Rtx20 => "RTX 20",
        }
    }
}

/// GA10x (RTX 30).
#[rustfmt::skip]
const GA: &[u16] = &[
    // GA102
    0x2200, 0x2203, 0x2204, 0x2205, 0x2206, 0x2207, 0x2208, 0x220a, 0x220d, 0x2216, 0x222b, 0x222f,
    // GA103
    0x2414,
    // GA104
    0x2480, 0x2482, 0x2483, 0x2484, 0x2486, 0x2487, 0x2488, 0x2489, 0x248a, 0x248c, 0x248d, 0x248e,
    0x249c, 0x249d, 0x249f, 0x24a0, 0x24a4, 0x24ac, 0x24ad, 0x24af, 0x24bf, 0x24c0, 0x24c7, 0x24c8,
    0x24c9, 0x24dc, 0x24dd, 0x24df, 0x24e0,
    // GA106
    0x2501, 0x2503, 0x2504, 0x2505, 0x2507, 0x2508, 0x2509, 0x252f, 0x2544,
];

/// AD10x (RTX 40).
#[rustfmt::skip]
const AD: &[u16] = &[
    // AD102, AD103, AD104, AD106, AD107
    0x2684, 0x2685, 0x2689,
    0x2702, 0x2704, 0x2705, 0x2709, 0x2757,
    0x2782, 0x2783, 0x2786,
    0x2803, 0x2805,
    0x2882, 0x28a0, 0x28e0, 0x28e1,
];

/// TU10x (RTX 20).
#[rustfmt::skip]
const TU: &[u16] = &[
    // TU102, TU104, TU106
    0x1e03, 0x1e04, 0x1e07, 0x1e2d, 0x1e2e,
    0x1e81, 0x1e82, 0x1e84, 0x1e87, 0x1e89, 0x1ec2, 0x1ec7,
    0x1f02, 0x1f03, 0x1f06, 0x1f07, 0x1f08, 0x1f42, 0x1f47,
];

/// The series of a supported GPU; `None` when gpu-unlock does not support it.
pub fn series(device: u16) -> Option<Series> {
    if GA.contains(&device) {
        Some(Series::Rtx30)
    } else if AD.contains(&device) {
        Some(Series::Rtx40)
    } else if TU.contains(&device) {
        Some(Series::Rtx20)
    } else {
        None
    }
}

/// The maturity of a supported GPU; `None` when gpu-unlock does not support it.
pub fn maturity(device: u16) -> Option<Maturity> {
    series(device).map(Series::maturity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// The vendored `recipes.c` this table was copied from.
    const RECIPES_C_SHA256: &str =
        "bf24bdaadb410d3f3fc31920bda60f98321d66155aa88d27aeda292a857f6b8d";

    fn recipes_c() -> &'static [u8] {
        super::super::kit::FILES
            .iter()
            .find(|(path, _)| *path == "kernel/gpu_unlock/recipes.c")
            .map(|(_, bytes)| *bytes)
            .unwrap()
    }

    #[test]
    fn the_vendored_recipes_file_is_the_one_the_table_was_copied_from() {
        let digest: String = Sha256::digest(recipes_c())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            digest, RECIPES_C_SHA256,
            "recipes.c changed: check GA, AD and TU against it, then update the hash"
        );
    }

    #[test]
    fn the_table_lists_87_distinct_ids_grouped_as_upstream_does() {
        let mut all: Vec<u16> = GA.iter().chain(AD).chain(TU).copied().collect();
        assert_eq!((GA.len(), AD.len(), TU.len()), (51, 17, 19));
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 87);
        // The upstream table names each card by a chip macro such as `GA102(0x2204)`; GA chips
        // are RTX 30, AD chips RTX 40 and TU chips RTX 20.
        let text = std::str::from_utf8(recipes_c()).unwrap();
        let start = text
            .find("static const struct gu_card cards[] = {")
            .unwrap();
        let block = &text[start..start + text[start..].find("};").unwrap()];
        let (mut ga, mut ad, mut tu) = (Vec::new(), Vec::new(), Vec::new());
        for (index, _) in block.match_indices("(0x") {
            let chip = &block[index - 5..index];
            let id = u16::from_str_radix(&block[index + 3..index + 7], 16).unwrap();
            match &chip[..2] {
                "GA" => ga.push(id),
                "AD" => ad.push(id),
                "TU" => tu.push(id),
                other => panic!("unexpected chip family {other}"),
            }
        }
        let sorted = |ids: &[u16]| {
            let mut ids = ids.to_vec();
            ids.sort_unstable();
            ids
        };
        assert_eq!(sorted(GA), sorted(&ga));
        assert_eq!(sorted(AD), sorted(&ad));
        assert_eq!(sorted(TU), sorted(&tu));
        assert_eq!(series(0x2204), Some(Series::Rtx30));
        assert_eq!(series(0x2684), Some(Series::Rtx40));
        assert_eq!(series(0x1e07), Some(Series::Rtx20));
        assert_eq!(series(0x2b85), None);
        assert_eq!(maturity(0x2204), Some(Maturity::Stable));
        assert_eq!(maturity(0x2684), Some(Maturity::Experimental));
        assert_eq!(maturity(0x1e07), Some(Maturity::Experimental));
        assert_eq!(maturity(0x2b85), None);
    }
}

//! The 14 Booter and GSP boot files, from the BINDATA arrays of the open RM object.
//!
//! Each image is a `<archive>_BINDATA_LABEL_<label>_data` symbol in the object's `.rodata`. Most
//! are raw DEFLATE followed by a gzip CRC-32 and length; [`ARRAYS`] pins, per array, what the
//! 610.43.03 generated sources state in their comments. The GSP boot images and descriptors are
//! written as NVIDIA stores them; each Booter image is packed with the header the `gpu_unlock`
//! module reads. An independent implementation of these formats: the upstream scripts serve only
//! as the reference the 17 pinned output hashes were taken from.

use std::collections::BTreeMap;
use std::io;

use object::read::elf::ElfFile64;
use object::{Object, ObjectSection, ObjectSymbol};

use super::firmware::invalid;

/// The BINDATA arrays by symbol name.
pub type Symbols<'a> = BTreeMap<&'a str, &'a [u8]>;

const BOOTER: &str = "kgspBinArchiveBooterLoadUcode_";
const BOOT: &str = "kgspBinArchiveGspRmBoot_";
const LOADER: &str = "ksec2BinArchiveBlUcode_TU102";
/// The magic of a Booter bundle, "BTL1", and its format version.
const BUNDLE_MAGIC: [u32; 2] = [0x314C_5442, 1];

/// Per array: the archive, the label, whether it is compressed and its size.
#[rustfmt::skip]
const ARRAYS: [(&str, &str, bool, usize); 21] = [
    ("kgspBinArchiveBooterLoadUcode_AD102", "IMAGE_PROD", true, 56_832),
    ("kgspBinArchiveBooterLoadUcode_AD102", "HEADER_PROD", true, 36),
    ("kgspBinArchiveBooterLoadUcode_AD102", "SIG_PROD", false, 768),
    ("kgspBinArchiveBooterLoadUcode_GA100", "IMAGE_PROD", true, 60_160),
    ("kgspBinArchiveBooterLoadUcode_GA100", "HEADER_PROD", true, 36),
    ("kgspBinArchiveBooterLoadUcode_GA100", "SIG_PROD", false, 384),
    ("kgspBinArchiveBooterLoadUcode_GA102", "IMAGE_PROD", true, 60_416),
    ("kgspBinArchiveBooterLoadUcode_GA102", "HEADER_PROD", true, 36),
    ("kgspBinArchiveBooterLoadUcode_GA102", "SIG_PROD", true, 768),
    ("kgspBinArchiveBooterLoadUcode_TU102", "IMAGE_PROD", true, 59_136),
    ("kgspBinArchiveBooterLoadUcode_TU102", "HEADER_PROD", true, 36),
    ("kgspBinArchiveBooterLoadUcode_TU102", "SIG_PROD", false, 16),
    ("kgspBinArchiveGspRmBoot_AD102", "UCODE_IMAGE_PROD", true, 36_864),
    ("kgspBinArchiveGspRmBoot_AD102", "UCODE_DESC_PROD", true, 84),
    ("kgspBinArchiveGspRmBoot_GA100", "UCODE_IMAGE", true, 4_096),
    ("kgspBinArchiveGspRmBoot_GA100", "UCODE_DESC", true, 76),
    ("kgspBinArchiveGspRmBoot_GA102", "UCODE_IMAGE_PROD", true, 24_576),
    ("kgspBinArchiveGspRmBoot_GA102", "UCODE_DESC_PROD", true, 84),
    ("kgspBinArchiveGspRmBoot_TU102", "UCODE_IMAGE", true, 4_096),
    ("kgspBinArchiveGspRmBoot_TU102", "UCODE_DESC", true, 76),
    ("ksec2BinArchiveBlUcode_TU102", "UCODE_IMAGE", true, 768),
];

/// Every `*_BINDATA_LABEL_*_data` symbol of the object, with its bytes.
pub fn symbols(object: &[u8]) -> io::Result<Symbols<'_>> {
    let elf = ElfFile64::<object::Endianness>::parse(object)
        .map_err(|e| invalid(format!("the RM object is not a 64-bit ELF: {e}")))?;
    let mut symbols = Symbols::new();
    for symbol in elf.symbols() {
        let Ok(name) = symbol.name() else { continue };
        if !name.contains("_BINDATA_LABEL_") || !name.ends_with("_data") {
            continue;
        }
        let start = usize::try_from(symbol.address()).ok();
        let size = usize::try_from(symbol.size()).ok();
        let bytes = symbol
            .section_index()
            .and_then(|index| elf.section_by_index(index).ok()?.data().ok())
            .and_then(|data| data.get(start?..)?.get(..size?))
            .ok_or_else(|| invalid(format!("{name} lies outside its section")))?;
        if symbols.insert(name, bytes).is_some() {
            return Err(invalid(format!("{name} is in the RM object twice")));
        }
    }
    Ok(symbols)
}

/// CRC-32 (IEEE 802.3), as in the gzip trailer.
fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0, |crc: u32, &b| {
        (0..8).fold(crc ^ u32::from(b), |c, _| {
            (c >> 1) ^ (0xEDB8_8320 & (c & 1).wrapping_neg())
        })
    })
}

/// Raw DEFLATE followed by the CRC-32 and the length of the `size` bytes it inflates to.
fn inflate(raw: &[u8], size: usize) -> Result<Vec<u8>, &'static str> {
    use miniz_oxide::inflate::stream::{inflate, InflateState};
    use miniz_oxide::{DataFormat, MZFlush, MZStatus};
    let mut state = InflateState::new_boxed(DataFormat::Raw);
    let mut out = vec![0; size + 1];
    let result = inflate(&mut state, raw, &mut out, MZFlush::Finish);
    let trailer = &raw[result.bytes_consumed..];
    if result.status != Ok(MZStatus::StreamEnd) || result.bytes_written != size {
        return Err("the compressed stream is cut short or inflates to another size");
    }
    out.truncate(size);
    if trailer.len() != 8
        || trailer[..4] != crc32(&out).to_le_bytes()
        || trailer[4..] != (size as u32).to_le_bytes()
    {
        return Err("the checksum and length after the compressed stream do not match");
    }
    Ok(out)
}

/// One array: inflated when it is compressed, and of its pinned size. The three small Booter
/// fields are plain words: the signature patch location, the signature metadata (fuse version,
/// engine id, ucode id) and the signature count.
fn array(symbols: &Symbols, archive: &str, label: &str) -> io::Result<Vec<u8>> {
    let key = format!("{archive}_BINDATA_LABEL_{label}_data");
    let raw = symbols
        .get(key.as_str())
        .ok_or_else(|| invalid(format!("the RM object lacks {key}")))?;
    let pinned = ARRAYS.iter().find(|a| a.0 == archive && a.1 == label);
    let (compressed, size) = match (pinned, label) {
        (Some(&(_, _, compressed, size)), _) => (compressed, size),
        (None, "PATCH_LOC" | "NUM_SIGS") => (false, 4),
        (None, "PATCH_META") => (false, 12),
        (None, _) => return Err(invalid(format!("{key} is not pinned"))),
    };
    let bytes = match compressed {
        true => inflate(raw, size).map_err(|e| invalid(format!("{key}: {e}")))?,
        false => raw.to_vec(),
    };
    if bytes.len() != size {
        return Err(invalid(format!(
            "{key} is {} bytes, not {size}",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// One Booter bundle as the module reads it (`gpu_unlock_booter_hdr` in `prep_610.h`): the magic,
/// fourteen header words, the image and signature lengths, the image, the signatures. The first
/// nine header words are NVIDIA's Falcon load header: OS code offset and size, OS data offset and
/// size, the application count, application code offset and size, application data offset and
/// size. The fields are copied as they are, not judged here: the pinned output hashes check the
/// whole bundle.
fn bundle(symbols: &Symbols, chip: &str) -> io::Result<Vec<u8>> {
    let archive = format!("{BOOTER}{chip}");
    let get = |label: &str| array(symbols, &archive, label);
    let (image, sigs, load) = (
        get("IMAGE_PROD")?,
        get("SIG_PROD")?,
        words(&get("HEADER_PROD")?),
    );
    let (patch_loc, meta) = (words(&get("PATCH_LOC")?)[0], words(&get("PATCH_META")?));
    let (num_sigs, engine_id, ucode_id) = (words(&get("NUM_SIGS")?)[0], meta[1], meta[2]);
    let (sig_bytes, image_size) = (sigs.len() as u32, image.len() as u32);
    let tail = [
        patch_loc, ucode_id, engine_id, num_sigs, sig_bytes, image_size, sig_bytes,
    ];
    let header = BUNDLE_MAGIC.into_iter().chain(load).chain(tail);
    let header = header.flat_map(u32::to_le_bytes);
    Ok(header.chain(image).chain(sigs).collect())
}

/// The 14 files of [`super::OUTPUTS`] that come from the RM object. The descriptors and images
/// are NVIDIA's arrays as they are; the pinned output hashes check the whole set.
pub fn assets(symbols: &Symbols) -> io::Result<Vec<(&'static str, Vec<u8>)>> {
    let boot = |chip: &str, label: &str| array(symbols, &format!("{BOOT}{chip}"), label);
    let loader = array(symbols, LOADER, "UCODE_IMAGE")?;
    Ok(vec![
        ("booter_load_ad102.bin", bundle(symbols, "AD102")?),
        ("booter_load_ga100.bin", bundle(symbols, "GA100")?),
        ("booter_load_ga102.bin", bundle(symbols, "GA102")?),
        ("booter_load_tu102.bin", bundle(symbols, "TU102")?),
        ("falcon_loader.bin", loader.clone()),
        ("gsp_boot_ad102.bin", boot("AD102", "UCODE_IMAGE_PROD")?),
        ("gsp_boot_desc_ad102.bin", boot("AD102", "UCODE_DESC_PROD")?),
        ("gsp_boot_desc_ga100.bin", boot("GA100", "UCODE_DESC")?),
        ("gsp_boot_desc_ga102.bin", boot("GA102", "UCODE_DESC_PROD")?),
        ("gsp_boot_desc_tu102.bin", boot("TU102", "UCODE_DESC")?),
        ("gsp_boot_ga100.bin", boot("GA100", "UCODE_IMAGE")?),
        ("gsp_boot_ga102.bin", boot("GA102", "UCODE_IMAGE_PROD")?),
        ("gsp_boot_tu102.bin", boot("TU102", "UCODE_IMAGE")?),
        ("sec2_bl_tu102.bin", loader),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compressed(bytes: &[u8]) -> Vec<u8> {
        let mut out = miniz_oxide::deflate::compress_to_vec(bytes, 6);
        out.extend(crc32(bytes).to_le_bytes());
        out.extend((bytes.len() as u32).to_le_bytes());
        out
    }

    #[test]
    fn the_checksum_is_the_gzip_crc32() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn a_compressed_array_needs_its_exact_trailer() {
        let data = b"falcon image bytes ".repeat(40);
        let good = compressed(&data);
        assert_eq!(inflate(&good, data.len()).unwrap(), data);
        let stream = good.len() - 8;
        let mut wrong_crc = good.clone();
        wrong_crc[stream] ^= 1;
        let mut wrong_length = good.clone();
        wrong_length[stream + 4] ^= 1;
        let trailing = [&good[..], b"x"].concat();
        let truncated = &good[..stream / 2];
        let no_trailer = &good[..stream];
        let (checksum, short) = (
            "checksum and length",
            "cut short or inflates to another size",
        );
        for (label, raw, size, why) in [
            ("crc", &wrong_crc[..], data.len(), checksum),
            ("length", &wrong_length[..], data.len(), checksum),
            ("trailing", &trailing[..], data.len(), checksum),
            ("no trailer", no_trailer, data.len(), checksum),
            ("truncated", truncated, data.len(), short),
            ("larger", &good[..], data.len() - 1, short),
            ("smaller", &good[..], data.len() + 1, short),
        ] {
            let error = inflate(raw, size).unwrap_err();
            assert!(error.contains(why), "{label}: {error}");
        }
    }

    /// A relocatable x86-64 ELF with one `.rodata` section and local object symbols in it, given
    /// as (name, offset, size).
    fn elf(rodata: &[u8], symbols: &[(&str, u64, u64)]) -> Vec<u8> {
        let names = b"\0.rodata\0.symtab\0.strtab\0.shstrtab\0";
        let (mut strtab, mut symtab) = (vec![0u8], vec![0u8; 24]);
        for (name, value, size) in symbols {
            symtab.extend((strtab.len() as u32).to_le_bytes());
            symtab.extend([1, 0, 1, 0]); // STT_OBJECT, STB_LOCAL, section 1
            symtab.extend(value.to_le_bytes());
            symtab.extend(size.to_le_bytes());
            strtab.extend(name.as_bytes());
            strtab.push(0);
        }
        let (mut out, mut headers) = (vec![0u8; 64], vec![0u8; 64]);
        let info = symbols.len() as u32 + 1;
        for (name, kind, data, link, info, entsize) in [
            (1u32, 1u32, rodata, 0u32, 0u32, 0u64),
            (9, 2, &symtab[..], 3, info, 24),
            (17, 3, &strtab[..], 0, 0, 0),
            (25, 3, &names[..], 0, 0, 0),
        ] {
            out.resize(out.len().next_multiple_of(8), 0);
            headers.extend(name.to_le_bytes());
            headers.extend(kind.to_le_bytes());
            headers.extend([0; 16]);
            headers.extend((out.len() as u64).to_le_bytes());
            headers.extend((data.len() as u64).to_le_bytes());
            headers.extend(link.to_le_bytes());
            headers.extend(info.to_le_bytes());
            headers.extend(1u64.to_le_bytes());
            headers.extend(entsize.to_le_bytes());
            out.extend(data);
        }
        out.resize(out.len().next_multiple_of(8), 0);
        let shoff = out.len() as u64;
        out.extend(headers);
        out[..8].copy_from_slice(b"\x7fELF\x02\x01\x01\0");
        out[16..24].copy_from_slice(&[1, 0, 62, 0, 1, 0, 0, 0]); // ET_REL, EM_X86_64, version 1
        out[40..48].copy_from_slice(&shoff.to_le_bytes());
        out[52..54].copy_from_slice(&64u16.to_le_bytes());
        out[58..64].copy_from_slice(&[64, 0, 5, 0, 4, 0]);
        out
    }

    #[test]
    fn the_bindata_symbols_are_read_from_their_section() {
        let rodata = b"..AAAA..BBBBBB";
        let object = elf(
            rodata,
            &[
                ("x_BINDATA_LABEL_A_data", 2, 4),
                ("x_BINDATA_LABEL_B_data", 8, 6),
                ("unrelated", 0, 2),
                ("x_BINDATA_LABEL_C_desc", 0, 1),
            ],
        );
        assert_eq!(
            symbols(&object).unwrap(),
            Symbols::from([
                ("x_BINDATA_LABEL_A_data", &b"AAAA"[..]),
                ("x_BINDATA_LABEL_B_data", &b"BBBBBB"[..]),
            ])
        );
        let outside = elf(rodata, &[("x_BINDATA_LABEL_A_data", 12, 4)]);
        let twice = elf(
            rodata,
            &[
                ("x_BINDATA_LABEL_A_data", 2, 4),
                ("x_BINDATA_LABEL_A_data", 8, 6),
            ],
        );
        for (object, why) in [
            (&outside[..], "outside its section"),
            (&twice[..], "twice"),
            (b"\x7fELF but not one", "not a 64-bit ELF"),
        ] {
            let error = symbols(object).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains(why), "{error}");
        }
    }

    const FIELDS: [u32; 9] = [0, 256, 512, 1024, 1, 1536, 256, 5, 6];

    /// The TU102 Booter arrays: a 59,136-byte image, the header [`FIELDS`], one 16-byte
    /// signature patched at 512.
    fn booter() -> BTreeMap<String, Vec<u8>> {
        let key = |label: &str| format!("{BOOTER}TU102_BINDATA_LABEL_{label}_data");
        let le = |w: &[u32]| w.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
        BTreeMap::from([
            (key("IMAGE_PROD"), compressed(&vec![7; 59_136])),
            (key("HEADER_PROD"), compressed(&le(&FIELDS))),
            (key("SIG_PROD"), vec![9; 16]),
            (key("PATCH_LOC"), le(&[512])),
            (key("PATCH_META"), le(&[0, 2, 3])),
            (key("NUM_SIGS"), le(&[1])),
        ])
    }

    fn tu102_bundle(map: &BTreeMap<String, Vec<u8>>) -> io::Result<Vec<u8>> {
        let symbols = map
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        bundle(&symbols, "TU102")
    }

    #[test]
    fn a_booter_bundle_has_the_module_header_then_the_image_and_signatures() {
        let out = tu102_bundle(&booter()).unwrap();
        assert_eq!(out.len(), 18 * 4 + 59_136 + 16);
        let mut want = BUNDLE_MAGIC.to_vec();
        want.extend(FIELDS);
        want.extend([512, 3, 2, 1, 16, 59_136, 16]);
        assert_eq!(words(&out[..72]), want);
        assert!(out[72..72 + 59_136].iter().all(|b| *b == 7));
        assert!(out[72 + 59_136..].iter().all(|b| *b == 9));
    }

    #[test]
    fn a_booter_array_of_another_size_or_missing_is_refused() {
        let mut short_signature = booter();
        short_signature.insert(
            format!("{BOOTER}TU102_BINDATA_LABEL_SIG_PROD_data"),
            vec![9; 15],
        );
        let mut missing = booter();
        missing.remove(&format!("{BOOTER}TU102_BINDATA_LABEL_NUM_SIGS_data"));
        for (label, map, why) in [
            ("signature size", short_signature, "is 15 bytes, not 16"),
            ("missing array", missing, "lacks"),
        ] {
            let error = tu102_bundle(&map).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains(why), "{label}: {error}");
        }
    }
}

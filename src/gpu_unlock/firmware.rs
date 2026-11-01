//! The firmware, generated in process from the NVIDIA installer alone.
//!
//! The installer is a makeself archive: a shell header, then a zstd-compressed tar. The two GSP
//! images and the license are tar members and are kept unchanged. The other 14 files come from
//! the BINDATA arrays of the open RM object in the same tar ([`super::boot`]). An independent
//! implementation of the NVIDIA file formats; the fixed values of [`super::OUTPUTS`] check the
//! result.

use std::collections::BTreeSet;
use std::io::{self, Read};

use super::{boot, FIRMWARE_LICENSE, FIRMWARE_VERSION, OUTPUTS};

const GSP: [&str; 2] = ["firmware/gsp_ga10x.bin", "firmware/gsp_tu10x.bin"];
const LICENSE: &str = "LICENSE";
const RM_OBJECT: &str = "kernel-open/nvidia/nv-kernel.o_binary";
/// The RM object is read into memory; in the 610.43.03 installer it is 19,223,480 bytes.
const MAX_OBJECT: u64 = 32 << 20;
/// The makeself header lies within this many bytes.
const MAX_HEADER: u64 = 1 << 20;

/// Receives one output file: its name and its content.
pub type Output<'a> = dyn FnMut(&str, &mut dyn Read) -> io::Result<()> + 'a;

pub(super) fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Write the 17 files of [`super::OUTPUTS`] from the installer, each through `output` once.
pub fn generate(installer: impl Read, output: &mut Output) -> io::Result<()> {
    let (license, object) = scan(payload(installer)?, output)?;
    output(FIRMWARE_LICENSE, &mut license.as_slice())?;
    for (name, bytes) in boot::assets(&boot::symbols(&object)?)? {
        output(name, &mut bytes.as_slice())?;
    }
    Ok(())
}

/// The compressed tar after the makeself header. The header names the release, and `skip=N` is
/// the line the payload starts on (the installer runs `tail -n +N`).
fn payload(mut installer: impl Read) -> io::Result<impl Read> {
    let mut head = Vec::new();
    installer.by_ref().take(MAX_HEADER).read_to_end(&mut head)?;
    let value = |key: &str| {
        head.split(|b| *b == b'\n')
            .find_map(|line| line.strip_prefix(key.as_bytes()))
            .map(|v| String::from_utf8_lossy(v).into_owned())
    };
    let version = value("version_string=").unwrap_or_default();
    if version != FIRMWARE_VERSION {
        return Err(invalid(format!(
            "the installer is not the {FIRMWARE_VERSION} release: {version:?}"
        )));
    }
    let skip = value("skip=").and_then(|v| v.parse::<usize>().ok());
    let mut start = 0;
    for _ in 1..skip
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("no skip= line".into()))?
    {
        let line = head[start..].iter().position(|b| *b == b'\n');
        start += line.ok_or_else(|| invalid("the installer header ends early".into()))? + 1;
    }
    Ok(io::Cursor::new(head.split_off(start)).chain(installer))
}

/// The most bytes taken from the tar member at `path`, `None` for a member that is not taken. The
/// GSP images and the license become output files unchanged, so one larger than its pinned
/// output can never match it; the RM object is held in memory.
fn limit(path: &str) -> Option<u64> {
    let output = match path {
        RM_OBJECT => return Some(MAX_OBJECT),
        LICENSE => FIRMWARE_LICENSE,
        _ => path
            .strip_prefix("firmware/")
            .filter(|_| GSP.contains(&path))?,
    };
    OUTPUTS.iter().find(|f| f.name == output).map(|f| f.bytes)
}

/// One pass over the tar: the GSP images go straight to `output`, the license and the RM object
/// (22 KB and 19 MB) are returned. Decoding stops once all four were read; a later copy of a
/// member already read is passed over, as the pinned outputs check what the first one gave.
fn scan(payload: impl Read, output: &mut Output) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let decoder = ruzstd::decoding::StreamingDecoder::new(io::BufReader::new(payload))
        .map_err(|e| invalid(format!("the installer payload is not zstd: {e}")))?;
    let mut archive = tar::Archive::new(decoder);
    let mut taken = BTreeSet::new();
    let (mut license, mut object) = (Vec::new(), Vec::new());
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some(most) = limit(&path).filter(|_| !taken.contains(&path)) else {
            continue;
        };
        if !entry.header().entry_type().is_file() || entry.size() > most {
            return Err(invalid(format!(
                "{path} is not a regular file of at most {most} bytes"
            )));
        }
        match path.strip_prefix("firmware/") {
            Some(name) => output(name, &mut entry)?,
            None if path == LICENSE => drop(entry.read_to_end(&mut license)?),
            None => drop(entry.read_to_end(&mut object)?),
        }
        taken.insert(path);
        if taken.len() == 4 {
            return Ok((license, object));
        }
    }
    Err(invalid("the installer lacks a firmware member".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(skip: &str, version: &str) -> Vec<u8> {
        format!("#!/bin/sh\nskip={skip}\nversion_string={version}\n").into_bytes()
    }

    fn read_payload(installer: Vec<u8>) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        payload(installer.as_slice())?.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn the_payload_starts_on_the_skip_line_of_the_named_release() {
        let mut installer = header("4", "610.43.03");
        installer.extend_from_slice(b"PAYLOAD\nmore");
        assert_eq!(read_payload(installer).unwrap(), b"PAYLOAD\nmore");
        // A payload beyond the first MiB is chained after the header bytes.
        let mut long = header("4", "610.43.03");
        long.extend(std::iter::repeat_n(b'x', 2 << 20));
        assert_eq!(read_payload(long).unwrap().len(), 2 << 20);
    }

    #[test]
    fn a_header_of_another_release_or_without_its_lines_is_refused() {
        let release = "not the 610.43.03 release";
        for (installer, why) in [
            (header("4", "610.43.04"), release),
            (b"#!/bin/sh\nskip=4\n".to_vec(), release),
            (b"not an installer".to_vec(), release),
            (
                b"#!/bin/sh\nversion_string=610.43.03\n".to_vec(),
                "no skip=",
            ),
            (header("0", "610.43.03"), "no skip="),
            (header("x", "610.43.03"), "no skip="),
            (header("9", "610.43.03"), "ends early"),
        ] {
            let error = read_payload(installer).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains(why), "{error}");
        }
    }

    fn tar(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, bytes) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn zstd(bytes: &[u8]) -> Vec<u8> {
        ruzstd::encoding::compress_to_vec(bytes, ruzstd::encoding::CompressionLevel::Fastest)
    }

    /// The license, the object and the files written by a scan of `tar`, compressed.
    type Scanned = (Vec<u8>, Vec<u8>, Vec<(String, Vec<u8>)>);

    fn scan_tar(tar: &[u8]) -> io::Result<Scanned> {
        let mut written = Vec::new();
        let mut output = |name: &str, data: &mut dyn Read| {
            let mut bytes = Vec::new();
            data.read_to_end(&mut bytes)?;
            written.push((name.to_string(), bytes));
            Ok(())
        };
        let (license, object) = scan(zstd(tar).as_slice(), &mut output)?;
        Ok((license, object, written))
    }

    #[test]
    fn the_four_members_are_read_from_the_zstd_tar() {
        let tar = tar(&[
            ("README.txt", b"other"),
            ("firmware/gsp_ga10x.bin", b"ga10x"),
            ("firmware/gsp_ga10x.bin", b"a later copy: passed over"),
            ("kernel-open/nvidia/nv-kernel.o_binary", b"object"),
            ("firmware/gsp_tu10x.bin", b"tu10x"),
            ("LICENSE", b"license"),
            ("LICENSE", b"after the four: never read"),
        ]);
        let (license, object, written) = scan_tar(&tar).unwrap();
        assert_eq!((license, object), (b"license".to_vec(), b"object".to_vec()));
        assert_eq!(
            written,
            [
                ("gsp_ga10x.bin".to_string(), b"ga10x".to_vec()),
                ("gsp_tu10x.bin".to_string(), b"tu10x".to_vec())
            ]
        );
    }

    #[test]
    fn each_member_is_bounded_by_its_pinned_output_or_the_object_limit() {
        assert_eq!(limit("firmware/gsp_ga10x.bin"), Some(84_277_400));
        assert_eq!(limit("firmware/gsp_tu10x.bin"), Some(29_352_832));
        assert_eq!(limit("LICENSE"), Some(22_653));
        assert_eq!(limit(RM_OBJECT), Some(MAX_OBJECT));
        assert_eq!(limit("firmware/gsp_ad10x.bin"), None);
        assert_eq!(limit("kernel/nvidia/nv-kernel.o_binary"), None);
    }

    /// A tar of one header that claims one byte more than `path` may have, without the bytes.
    fn oversized(path: &str) -> Vec<u8> {
        let mut big = tar::Header::new_gnu();
        big.set_path(path).unwrap();
        big.set_size(limit(path).unwrap() + 1);
        big.set_cksum();
        [big.as_bytes().as_slice(), &[0; 1024]].concat()
    }

    #[test]
    fn a_missing_irregular_or_oversized_member_is_refused() {
        let gsp: [(&str, &[u8]); 2] = [
            ("firmware/gsp_ga10x.bin", b"a"),
            ("firmware/gsp_tu10x.bin", b"b"),
        ];
        let missing = tar(&gsp);
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_link(&mut link, "LICENSE", "elsewhere")
            .unwrap();
        let irregular = builder.into_inner().unwrap();
        for (sample, why) in [
            (missing, "lacks a firmware member"),
            (irregular, "not a regular file"),
            (oversized("LICENSE"), "at most 22653 bytes"),
            (oversized(RM_OBJECT), "at most 33554432 bytes"),
        ] {
            let error = scan_tar(&sample).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains(why), "{error}");
        }
        let not_zstd = scan(&b"plain tar"[..], &mut |_, _| Ok(())).unwrap_err();
        assert!(not_zstd.to_string().contains("not zstd"), "{not_zstd}");
    }
}

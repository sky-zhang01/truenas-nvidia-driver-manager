#!/usr/bin/env bash
# Exercise image content assertions with local docker-save archive fixtures.
set -Eeuo pipefail
exec python3 - "$(dirname "${BASH_SOURCE[0]}")/image-assert.py" <<'PY'
import gzip
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

SCRIPT = Path(sys.argv.pop()).resolve()
PREFIX = "usr/local/share/truenas-nvidia-driver-manager"
LICENSE = PREFIX + "/THIRD_PARTY_LICENSES.txt"
VALID = b"Rust runtime packages: 1\nRust crate: libc 0.2.0\n"


class ImageTests(unittest.TestCase):
    def archive(self, notices=VALID, compressed=False, replacement=None, link=False):
        entries = {name: None for name in (
            "usr", "usr/local", "usr/local/bin", "usr/local/share", PREFIX,
            PREFIX + "/web",
        )}
        entries.update({
            "usr/local/bin/truenas-nvidia-driver-manager": b"binary",
            PREFIX + "/LICENSE": b"MIT",
            PREFIX + "/THIRD_PARTY_NOTICES.md": b"notices",
            PREFIX + "/web/index.html": b"html",
            PREFIX + "/web/styles.css": b"css",
            PREFIX + "/web/app.js": b"js",
        })
        if notices is not None:
            entries[LICENSE] = notices

        def layer(files, license_link=False):
            data = io.BytesIO()
            with tarfile.open(fileobj=data, mode="w") as archive:
                for name, content in files.items():
                    member = tarfile.TarInfo(name)
                    if name == LICENSE and license_link:
                        member.type = tarfile.SYMTYPE
                        member.linkname = "LICENSE"
                        archive.addfile(member)
                    elif content is None:
                        member.type = tarfile.DIRTYPE
                        archive.addfile(member)
                    else:
                        member.size = len(content)
                        archive.addfile(member, io.BytesIO(content))
            return gzip.compress(data.getvalue()) if compressed else data.getvalue()

        blobs = {"layer.tar": layer(entries, link)}
        if replacement is not None:
            blobs["replacement.tar"] = layer({LICENSE: replacement})
        blobs["manifest.json"] = json.dumps([{"Layers": list(blobs)}]).encode()
        result = io.BytesIO()
        with tarfile.open(fileobj=result, mode="w") as archive:
            for name, content in blobs.items():
                member = tarfile.TarInfo(name)
                member.size = len(content)
                archive.addfile(member, io.BytesIO(content))
        return result.getvalue()

    def run_assertion(self, **options):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "image.tar"
            archive.write_bytes(self.archive(**options))
            return subprocess.run([sys.executable, str(SCRIPT), str(archive)],
                                  capture_output=True, text=True)

    def test_valid_plain_and_compressed_layers(self):
        for compressed in (False, True):
            with self.subTest(compressed=compressed):
                result = self.run_assertion(compressed=compressed)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_license_file_fails(self):
        self.assertNotEqual(self.run_assertion(notices=None).returncode, 0)

    def test_empty_or_missing_package_count_fails(self):
        for notices in (b"", b"Rust crate: libc 0.2.0\n", b"Rust runtime packages: 0\n"):
            with self.subTest(notices=notices):
                self.assertNotEqual(self.run_assertion(notices=notices).returncode, 0)

    def test_declared_count_must_match_package_lines(self):
        self.assertNotEqual(self.run_assertion(
            notices=b"Rust runtime packages: 2\nRust crate: libc 0.2.0\n").returncode, 0)

    def test_libc_is_required(self):
        self.assertNotEqual(self.run_assertion(
            notices=b"Rust runtime packages: 1\nRust crate: other 1.0.0\n").returncode, 0)

    def test_license_must_be_a_regular_file(self):
        self.assertNotEqual(self.run_assertion(link=True).returncode, 0)

    def test_last_layer_license_content_is_checked(self):
        self.assertNotEqual(self.run_assertion(replacement=b"").returncode, 0)


unittest.main()
PY

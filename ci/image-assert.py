#!/usr/bin/env python3
"""Assert what the docker save archive of the final image carries.

The empty-base image's own filesystem holds only the static program, the web
files and the license and notice files. Image content is the union of the layer
tars; the runtime scaffolding Docker mounts at container creation
(.dockerenv, /etc/hostname, /proc, /sys) lives in no layer. The archive
layout varies with the daemon and image store (both blobs/sha256/<hex> plus
manifest.json/index.json with gzipped layer tars, and the older
<layer>/layer.tar layout are read), gzipped or not.

Assertions (any failure is red): the entries outside the web prefix equal
the approved set exactly; the web prefix carries index.html and styles.css
and at least one .js; no whiteout entry (".wh.", which deletes rather than
adds) appears; runtime licenses declare a positive Rust package count and libc.

usage: image-assert.py <docker save archive>
"""
import gzip
import io
import json
import re
import sys
import tarfile

# The image's own content: these entries plus what lives under the web
# directory, and nothing else.
NON_WEB = frozenset((
    "usr",
    "usr/local",
    "usr/local/bin",
    "usr/local/bin/truenas-nvidia-driver-manager",
    "usr/local/share",
    "usr/local/share/truenas-nvidia-driver-manager",
    "usr/local/share/truenas-nvidia-driver-manager/LICENSE",
    "usr/local/share/truenas-nvidia-driver-manager/THIRD_PARTY_NOTICES.md",
    "usr/local/share/truenas-nvidia-driver-manager/THIRD_PARTY_LICENSES.txt",
))
WEB_PREFIX = "usr/local/share/truenas-nvidia-driver-manager/web"
LICENSE_PATH = "usr/local/share/truenas-nvidia-driver-manager/THIRD_PARTY_LICENSES.txt"


def die(message):
    print("image: " + message, file=sys.stderr)
    raise SystemExit(1)


def blob(archive, name):
    """One member of the outer archive, read whole."""
    handle = archive.extractfile(name)
    if handle is None:
        die("the archive has no readable member " + name)
    return handle.read()


def layer_members(data):
    """Layer entries and license bytes, gzipped or not; None if it is no tar."""
    plain = data
    if data[:2] == b"\x1f\x8b":
        try:
            plain = gzip.decompress(data)
        except OSError:
            return None
    try:
        with tarfile.open(fileobj=io.BytesIO(plain)) as layer:
            members = layer.getmembers()
            notices = None
            for member in members:
                if member.name.rstrip("/") == LICENSE_PATH:
                    if not member.isfile():
                        die("runtime licenses must be a regular file")
                    notices = blob(layer, member.name)
            return [member.name for member in members], notices
    except tarfile.TarError:
        return None


def layer_paths(archive, names):
    """Every image layer of the archive, in both docker save layouts."""
    if "manifest.json" in names:
        paths = []
        for image in json.loads(blob(archive, "manifest.json")):
            paths.extend(image.get("Layers", []))
        return paths
    if "index.json" in names:
        paths = []
        for reference in json.loads(blob(archive, "index.json")).get("manifests", []):
            manifest = "blobs/sha256/" + reference.get("digest", "").rsplit(":", 1)[-1]
            if manifest not in names:
                die("the archive is missing the manifest " + manifest)
            for layer in json.loads(blob(archive, manifest)).get("layers", []):
                media = layer.get("mediaType", "")
                if media and "tar" not in media:
                    continue  # an attestation or another non-layer blob
                paths.append(
                    "blobs/sha256/" + layer.get("digest", "").rsplit(":", 1)[-1])
        return paths
    die("the archive carries neither manifest.json nor index.json")


def main():
    if len(sys.argv) != 2:
        die("usage: image-assert.py <docker save archive>")
    entries = set()
    layers = 0
    notices = None
    with tarfile.open(sys.argv[1]) as archive:
        names = set(archive.getnames())
        for path in layer_paths(archive, names):
            if path not in names:
                die("the archive is missing layer " + path)
            content = layer_members(blob(archive, path))
            if content is None:
                die("layer " + path + " is not a tar (gzipped or not)")
            members, layer_notices = content
            if layer_notices is not None:
                notices = layer_notices
            layers += 1
            entries.update(name.rstrip("/") for name in members)
    if layers == 0:
        die("the archive carries no image layer")
    whiteouts = sorted(entry for entry in entries if ".wh." in entry)
    if whiteouts:
        die("whiteout entries are not image content: " + ", ".join(whiteouts))
    outside = {
        entry for entry in entries
        if entry != WEB_PREFIX and not entry.startswith(WEB_PREFIX + "/")
    }
    if outside != set(NON_WEB):
        missing = sorted(set(NON_WEB) - outside)
        extra = sorted(outside - set(NON_WEB))
        die("the entries outside the web directory are not the approved set"
            + ("; missing: " + ", ".join(missing) if missing else "")
            + ("; extra: " + ", ".join(extra) if extra else ""))
    for required in ("index.html", "styles.css"):
        if WEB_PREFIX + "/" + required not in entries:
            die("the web directory does not carry " + required)
    if not any(entry.startswith(WEB_PREFIX + "/") and entry.endswith(".js")
               for entry in entries):
        die("the web directory carries no .js file")
    if notices is None:
        die("the runtime license file has no readable content")
    text = notices.decode("utf-8")
    count = re.search(r"^Rust runtime packages: ([0-9]+)$", text, re.MULTILINE)
    crates = re.findall(r"^Rust crate: (\S+) (\S+)$", text, re.MULTILINE)
    if count is None or int(count[1]) <= 0 or int(count[1]) != len(crates):
        die("runtime licenses must list a matching positive Rust package count")
    if not any(name == "libc" for name, _ in crates):
        die("runtime licenses must include the libc Rust crate")
    print("image: %d image layers, %d filesystem entries, exactly the approved set"
          % (layers, len(entries)))


main()

#!/usr/bin/env python3
"""Publish and verify the image artifact from this workflow run."""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urljoin, urlparse
from urllib.request import Request, urlopen

IMAGE = os.environ.get("GHCR_IMAGE", "")
if not re.fullmatch(r"ghcr\.io/[a-z0-9]+(?:-[a-z0-9]+)*/[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*", IMAGE):
    raise ValueError("GHCR_IMAGE must be a lowercase ghcr.io/owner/repository name without a tag or digest")
PACKAGE = IMAGE.split("/", 1)[1]
API = "https://ghcr.io/v2/" + PACKAGE
ACCEPT = ", ".join(("application/vnd.oci.image.index.v1+json",
                    "application/vnd.oci.image.manifest.v1+json",
                    "application/vnd.docker.distribution.manifest.list.v2+json",
                    "application/vnd.docker.distribution.manifest.v2+json"))
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?\Z")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def version_key(value):
    match = VERSION.fullmatch(value)
    if not match or len(value) > 128:
        raise ValueError("invalid container version: " + value)
    if match[4] and any(p.isdigit() and len(p) > 1 and p[0] == "0" for p in match[4].split(".")):
        raise ValueError("numeric prerelease identifiers cannot have leading zeros")
    return tuple(int(match[i]) for i in (1, 2, 3)), match[4]


def package_version():
    """Read this repository's literal package.version; reject ambiguous TOML."""
    text = Path("Cargo.toml").read_text()
    content = "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))
    # Multiline strings and inherited versions need an explicit format change.
    if '"""' in content or "'''" in content:
        raise ValueError("Cargo.toml must declare one literal package.version")
    sections = re.findall(r"(?ms)^[ \t]*\[package\][ \t]*(?:#[^\n]*)?\n(.*?)(?=^[ \t]*\[|\Z)", text)
    if len(sections) != 1:
        raise ValueError("Cargo.toml must declare one literal package.version")
    body = "\n".join(line for line in sections[0].splitlines() if not line.lstrip().startswith("#"))
    declarations = re.findall(r"(?m)^[ \t]*(?:version|[\"']version[\"'])(?=[ \t]*[.=]).*$", body)
    match = (re.fullmatch(r"[ \t]*version[ \t]*=[ \t]*([\"'])([^\"'\n]+)\1[ \t]*(?:#[^\n]*)?", declarations[0])
             if len(declarations) == 1 else None)
    if match is None or VERSION.fullmatch(match[2]) is None:
        raise ValueError("Cargo.toml must declare one literal package.version")
    return match[2]


def validate():
    if os.environ.get("GITHUB_EVENT_NAME") != "push":
        raise ValueError("publication requires a tag push")
    ref = os.environ.get("GITHUB_REF", "")
    if not ref.startswith("refs/tags/v"):
        raise ValueError("publication requires a v<version> tag")
    version = ref[len("refs/tags/v"):]
    version_key(version)
    if version != package_version():
        raise ValueError("tag does not equal Cargo.toml package.version")
    head = run("git", "rev-parse", "HEAD")
    if run("git", "rev-parse", ref + "^{commit}") != head or os.environ.get("GITHUB_SHA") != head:
        raise ValueError("checkout is not the pushed tag commit")
    subprocess.run(("git", "merge-base", "--is-ancestor", head, "origin/main"), check=True)
    return version


def request(url, headers):
    for attempt in range(5):
        try:
            with urlopen(Request(url, headers=headers), timeout=60) as response:
                return response.read(), response.headers
        except HTTPError as error:
            if error.code not in (429, 500, 502, 503, 504) or attempt == 4:
                raise
            error.close()
        except (URLError, TimeoutError):
            if attempt == 4:
                raise
        time.sleep(3)
    raise RuntimeError("registry retry exhausted")


class Registry:
    def __init__(self):
        credentials = os.environ["GITHUB_ACTOR"] + ":" + os.environ["GHCR_TOKEN"]
        basic = base64.b64encode(credentials.encode()).decode()
        body, _ = request("https://ghcr.io/token?service=ghcr.io&scope=repository:" + PACKAGE + ":pull,push",
                          {"Authorization": "Basic " + basic})
        token = json.loads(body)["token"]
        if not isinstance(token, str) or not token:
            raise ValueError("registry returned an empty token")
        self.headers = {"Authorization": "Bearer " + token, "Accept": ACCEPT}

    def get(self, path, missing=False):
        try:
            return request(API + path, self.headers)
        except HTTPError as error:
            if missing and error.code == 404:
                error.close()
                return None
            raise

    def manifest(self, reference, missing=False):
        result = self.get("/manifests/" + reference, missing)
        if result is None:
            return None
        body, headers = result
        digest = "sha256:" + hashlib.sha256(body).hexdigest()
        if headers.get("Docker-Content-Digest") != digest:
            raise ValueError("registry digest header does not match manifest bytes")
        return json.loads(body), digest

    def tags(self):
        path = "/tags/list?n=100"
        seen = set()
        tags = []
        while path:
            if path in seen:
                raise ValueError("registry repeated a tag-list page")
            seen.add(path)
            result = self.get(path, missing=True)
            if result is None:
                if tags:
                    raise ValueError("registry lost a tag-list page")
                return []
            body, headers = result
            page = json.loads(body)
            if not isinstance(page, dict) or page.get("name") != PACKAGE or "tags" not in page:
                raise ValueError("registry tag inventory does not identify this package")
            entries = page["tags"]
            if entries is not None and (not isinstance(entries, list) or not all(isinstance(t, str) for t in entries)):
                raise ValueError("registry tag inventory is not a string list")
            tags.extend(entries or [])
            link = headers.get("Link", "")
            if not link:
                break
            match = re.fullmatch(r'<([^>]+)>;\s*rel="next"', link)
            if not match:
                raise ValueError("unrecognized registry pagination")
            url = urlparse(urljoin(API + path, match[1]))
            if url.scheme != "https" or url.netloc != "ghcr.io" or url.path != "/v2/" + PACKAGE + "/tags/list":
                raise ValueError("registry pagination left the package")
            path = "/tags/list" + ("?" + url.query if url.query else "")
        return tags


def archive_names(archive, config_digest):
    """Return the digests a Docker image store can use as this archive's image ID.

    The classic store names an image by its config digest; the containerd store
    names it by the digest of a manifest in the archive index that points to
    that config.
    """
    names = {config_digest}
    try:
        index = json.load(archive.extractfile("index.json"))
    except KeyError:
        return names
    for entry in index.get("manifests", []) if isinstance(index, dict) else []:
        digest = entry.get("digest", "") if isinstance(entry, dict) else ""
        if not DIGEST.fullmatch(digest):
            continue
        try:
            member = archive.extractfile("blobs/sha256/" + digest[len("sha256:"):])
        except KeyError:
            continue
        body = member.read() if member else b""
        if "sha256:" + hashlib.sha256(body).hexdigest() != digest:
            continue
        manifest = json.loads(body)
        if isinstance(manifest, dict) and manifest.get("config", {}).get("digest") == config_digest:
            names.add(digest)
    return names


def artifact():
    """Bind the archive to the tested image and load it; return the local ID and config digest."""
    image_id = Path("nvm-image.id").read_text().strip()
    if not DIGEST.fullmatch(image_id):
        raise ValueError("invalid image artifact ID")
    with tarfile.open("nvm-image.tar") as archive:
        entries = json.load(archive.extractfile("manifest.json"))
        if len(entries) != 1:
            raise ValueError("image archive must contain one image")
        config = archive.extractfile(entries[0]["Config"]).read()
        config_digest = "sha256:" + hashlib.sha256(config).hexdigest()
        names = archive_names(archive, config_digest)
    if image_id not in names:
        raise ValueError("image archive config differs from the tested image ID")
    metadata = json.loads(Path("nvm-build-metadata.json").read_text())
    if not isinstance(metadata, dict):
        raise ValueError("build metadata differs from the tested image config")
    # With the classic store BuildKit's metadata names the config digest. With
    # the containerd store it names only the manifest digest, the image ID there.
    if "containerimage.config.digest" in metadata:
        built = metadata["containerimage.config.digest"] == config_digest
    else:
        built = metadata.get("containerimage.digest") == image_id
    if not built:
        raise ValueError("build metadata differs from the tested image config")
    provenance = metadata.get("buildx.build.provenance")
    if not isinstance(provenance, dict) or not provenance:
        raise ValueError("build metadata is missing native BuildKit provenance")
    platform = json.loads(config)
    if platform.get("os") != "linux" or platform.get("architecture") != "amd64":
        raise ValueError("image archive is not linux/amd64")
    # The loading runner's image store may differ from the one that built the image.
    loaded = re.findall(r"(?m)^Loaded image ID: (sha256:[0-9a-f]{64})$", run("docker", "load", "-i", "nvm-image.tar"))
    if len(loaded) != 1 or loaded[0] not in names:
        raise ValueError("loaded image differs from the tested image ID")
    if run("docker", "image", "inspect", "-f", "{{.Id}}", loaded[0]) != loaded[0]:
        raise ValueError("loaded image differs from the tested image ID")
    return loaded[0], config_digest


def tested_sbom():
    sbom = json.loads(Path("nvm-image.spdx.json").read_text())
    if not sbom.get("spdxVersion", "").startswith("SPDX-") or not sbom.get("packages"):
        raise ValueError("image SBOM is missing SPDX package data")


def readback(registry, reference, config_digest):
    doc, digest = registry.manifest(reference)
    if "manifests" in doc:
        images = [m for m in doc["manifests"]
                  if m.get("annotations", {}).get("vnd.docker.reference.type") != "attestation-manifest"]
        if len(images) != 1 or images[0].get("platform", {}).get("os") != "linux" or images[0].get("platform", {}).get("architecture") != "amd64":
            raise ValueError("published index must carry exactly one linux/amd64 image")
        child_digest = images[0]["digest"]
        if not DIGEST.fullmatch(child_digest):
            raise ValueError("invalid child manifest digest")
        doc, actual = registry.manifest(child_digest)
        if child_digest != actual:
            raise ValueError("index descriptor differs from the child manifest digest")
    if doc.get("config", {}).get("digest") != config_digest:
        raise ValueError("published manifest does not point to the tested image config")
    return digest


def highest_stable(version, tags):
    candidate, prerelease = version_key(version)
    if prerelease:
        return False
    for tag in tags:
        try:
            key, pre = version_key(tag)
        except ValueError:
            continue
        if pre is None and key > candidate:
            return False
    return True


def attestation_mode():
    mode = os.environ.get("GHCR_ATTESTATION_MODE", "github")
    if mode not in ("github", "build-record"):
        raise ValueError("unknown GHCR_ATTESTATION_MODE")
    return mode


def receipt(version, digest, latest):
    mode = attestation_mode()
    # These hashes bind the retained files, not a signature or remote attestation.
    metadata_hash = hashlib.sha256(Path("nvm-build-metadata.json").read_bytes()).hexdigest()
    sbom_hash = hashlib.sha256(Path("nvm-image.spdx.json").read_bytes()).hexdigest()
    Path("ghcr-receipt.json").write_text(json.dumps({"image": IMAGE, "version": version,
        "digest": digest, "latest": latest, "commit": os.environ["GITHUB_SHA"],
        "run_id": os.environ["GITHUB_RUN_ID"], "attestation_mode": mode,
        "build_record": {"file": "nvm-build-metadata.json", "sha256": metadata_hash,
                         "type": "native-buildkit-provenance", "github_signed": False},
        "sbom": {"file": "nvm-image.spdx.json", "sha256": sbom_hash},
        "github_attestations": "requested" if mode == "github" else "not-requested"}, indent=2) + "\n")
    print(IMAGE + ":" + version + " -> " + digest + "; latest=" + str(latest))


def main(command):
    if command == "artifact":
        # The image job runs this check on every build, before any tag exists.
        artifact()
        return
    version = validate()
    if command == "validate":
        return
    if command not in ("publish", "promote"):
        raise ValueError("unknown command")
    attestation_mode()
    image_id, config_digest = artifact()
    tested_sbom()
    registry = Registry()
    if command == "publish":
        if registry.manifest(version, missing=True) is None:
            subprocess.run(("docker", "tag", image_id, IMAGE + ":" + version), check=True)
            subprocess.run(("docker", "push", IMAGE + ":" + version), check=True)
        # A failed attestation can be resumed using the same tested artifact.
        # An existing version with different content fails the readback.
        digest = readback(registry, version, config_digest)
        receipt(version, digest, False)
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write("digest=" + digest + "\n")
    else:
        digest = os.environ["GHCR_DIGEST"]
        if not DIGEST.fullmatch(digest) or readback(registry, version, config_digest) != digest:
            raise ValueError("version digest changed before promotion")
        # Only successfully published registry tags determine latest.
        # The workflow serializes every release job across all tag refs.
        tags = registry.tags()
        if version not in tags:
            raise ValueError("registry tag inventory does not contain the published version")
        promote = highest_stable(version, tags)
        if promote:
            subprocess.run(("docker", "tag", image_id, IMAGE + ":latest"), check=True)
            subprocess.run(("docker", "push", IMAGE + ":latest"), check=True)
            if readback(registry, "latest", config_digest) != digest:
                raise ValueError("latest digest differs from the version digest")
        if readback(registry, version, config_digest) != digest:
            raise ValueError("version digest changed after publication")
        receipt(version, digest, promote)


if __name__ == "__main__":
    try:
        main(sys.argv[1])
    except (ValueError, KeyError, IndexError, OSError, subprocess.CalledProcessError) as error:
        print("release: " + str(error), file=sys.stderr)
        sys.exit(1)

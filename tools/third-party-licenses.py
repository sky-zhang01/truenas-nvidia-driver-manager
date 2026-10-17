#!/usr/bin/env python3
"""Generate full runtime dependency licenses from local, locked package sources.

Run after populating the Cargo cache (and npm ci when runtime npm packages exist).
This command never installs packages or accesses the network. Missing source or
license files fail closed.
"""

import argparse
import json
from pathlib import Path
import subprocess
import sys

RUST_TARGET = "x86_64-unknown-linux-musl"
OUTPUT = "THIRD_PARTY_LICENSES.txt"


class LicenseError(Exception):
    """A dependency cannot be attributed from its local source."""


def cargo_metadata(root):
    result = subprocess.run(
        ["cargo", "metadata", "--offline", "--locked", "--format-version", "1",
         "--filter-platform", RUST_TARGET],
        cwd=root, capture_output=True, text=True, check=False,
    )
    if result.returncode:
        raise LicenseError("Offline Cargo metadata failed:\n" + result.stderr.strip())
    return json.loads(result.stdout)


def cargo_runtime_tree(root):
    result = subprocess.run(
        ["cargo", "tree", "--offline", "--locked", "--target", RUST_TARGET,
         "--edges", "normal,no-proc-macro", "--prefix", "none", "--format", "{p}"],
        cwd=root, capture_output=True, text=True, check=False,
    )
    if result.returncode:
        raise LicenseError("Offline Cargo runtime tree failed:\n" + result.stderr.strip())
    return result.stdout


def runtime_packages(metadata, tree):
    """Use Cargo's normal-feature tree rather than metadata's dev-unified graph."""
    root = metadata["resolve"]["root"]
    if root is None:
        raise LicenseError("A single Cargo package root is required")
    indexed = {}
    for package in metadata["packages"]:
        indexed.setdefault((package["name"], package["version"]), []).append(package)
    selected = {}
    for line in tree.splitlines():
        fields = line.split()
        if len(fields) < 2 or not fields[1].startswith("v"):
            raise LicenseError("Unrecognized Cargo runtime tree entry: " + line)
        key = (fields[0], fields[1][1:])
        matches = indexed.get(key, [])
        if len(matches) != 1:
            raise LicenseError("Cargo runtime package source is missing or ambiguous: " + line)
        if matches[0]["id"] != root:
            selected[key] = matches[0]
    if not tree.strip():
        raise LicenseError("Cargo runtime tree is empty")
    return [selected[key] for key in sorted(selected)]


def inside(directory, path):
    resolved = path.resolve()
    if not resolved.is_relative_to(directory.resolve()):
        raise LicenseError("Package path escapes its source directory: " + str(path))
    return resolved


def license_documents(directory, explicit=None):
    """Keep upstream license alternatives, additional grants and notices intact."""
    candidates = set()
    for entry in directory.iterdir():
        if entry.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")):
            if entry.is_dir():
                candidates.update(path for path in entry.rglob("*") if path.is_file())
            else:
                candidates.add(entry)
    if explicit:
        candidates.add(directory / explicit)
    documents = []
    for path in sorted(candidates, key=lambda candidate: candidate.relative_to(directory).as_posix()):
        source = inside(directory, path)
        if not source.is_file():
            raise LicenseError("License source is not a file: " + str(path))
        content = source.read_text(encoding="utf-8").replace("\r\n", "\n").replace("\r", "\n")
        if not content.strip():
            raise LicenseError("Empty license source: " + str(path))
        documents.append((path.relative_to(directory).as_posix(), content.rstrip("\n") + "\n"))
    if not documents:
        raise LicenseError("No full license text found in local package source: " + str(directory))
    return documents


def npm_packages(root):
    web = root / "web"
    lock = json.loads((web / "package-lock.json").read_text(encoding="utf-8"))
    if lock.get("lockfileVersion") != 3 or "packages" not in lock:
        raise LicenseError("npm package-lock.json must use lockfileVersion 3 with package records")
    result = []
    for relative, package in sorted(lock["packages"].items()):
        if not relative or package.get("dev") is True:
            continue
        directory = inside(web, web / relative)
        if not relative.startswith("node_modules/") or package.get("link"):
            raise LicenseError("Runtime npm links and non-node_modules sources are unsupported: " + relative)
        manifest = directory / "package.json"
        if not manifest.is_file():
            raise LicenseError("Missing runtime npm source; run npm ci first: " + relative)
        installed = json.loads(manifest.read_text(encoding="utf-8"))
        expected_name = relative.rsplit("node_modules/", 1)[-1]
        if installed.get("name") != expected_name or installed.get("version") != package.get("version"):
            raise LicenseError("Installed npm package differs from package-lock.json: " + relative)
        declared = package.get("license") or installed.get("license")
        if not isinstance(declared, str) or not declared.strip():
            raise LicenseError("Missing npm license declaration: " + relative)
        result.append({"name": expected_name, "version": package["version"],
                       "license": declared, "location": relative,
                       "documents": license_documents(directory)})
    return result


def append_package(lines, label, package, documents):
    lines.extend(["", "=" * 78, label + ": " + package["name"] + " " + package["version"],
                  "Declared license: " + package["license"]])
    if "location" in package:
        lines.append("Package location: " + package["location"])
    for name, content in documents:
        lines.extend(["", "--- " + name + " ---", content.rstrip("\n")])


def generate(root):
    lines = [
        "THIRD-PARTY RUNTIME LICENSES",
        "Generated by tools/third-party-licenses.py; do not edit by hand.",
        "",
        "Rust scope: normal dependencies for " + RUST_TARGET + ".",
        "Build dependencies, procedural macros and development-only dependencies are excluded.",
        "All upstream license alternatives and accompanying notices found in each package",
        "are reproduced below. Package declarations do not replace the license texts.",
        "npm scope: all non-development packages in web/package-lock.json.",
    ]
    packages = runtime_packages(cargo_metadata(root), cargo_runtime_tree(root))
    lines.extend(["", "Rust runtime packages: " + str(len(packages))])
    for package in packages:
        if not package.get("license"):
            raise LicenseError("Missing Cargo license declaration: " + package["name"])
        directory = Path(package["manifest_path"]).parent
        documents = license_documents(directory, package.get("license_file"))
        append_package(lines, "Rust crate", package, documents)
    packages = npm_packages(root)
    lines.extend(["", "npm runtime packages: " + str(len(packages))])
    if not packages:
        lines.append("No npm runtime dependencies are declared in the locked web dependency tree.")
    for package in packages:
        append_package(lines, "npm package", package, package["documents"])
    return "\n".join(lines) + "\n"


def main(arguments=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent,
                        help="repository root (defaults to this script's repository)")
    args = parser.parse_args(arguments)
    try:
        output = generate(args.root).encode("utf-8")
        destination = args.root / OUTPUT
        destination.write_bytes(output)
        print("Generated " + OUTPUT + ".")
        return 0
    except (LicenseError, OSError, ValueError, KeyError, TypeError) as error:
        print("Third-party license generation failed: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

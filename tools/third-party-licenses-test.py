#!/usr/bin/env python3
"""Regression tests for the offline third-party license generator."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "third_party_licenses", Path(__file__).with_name("third-party-licenses.py")
)
licenses = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(licenses)


class LicenseTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "web").mkdir()
        for name in ("Cargo.lock", "Cargo.toml"):
            (self.root / name).write_text("fixture\n")
        (self.root / "web/package.json").write_text('{"name":"web"}\n')
        (self.root / "web/package-lock.json").write_text(json.dumps({
            "lockfileVersion": 3,
            "packages": {"": {"name": "web"}, "node_modules/compiler": {
                "version": "1.0.0", "dev": True, "license": "MIT"}},
        }))

    def package(self, name, version="1.0.0", kind="lib", license_name="MIT"):
        directory = self.root / "cache" / (name + "-" + version)
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "Cargo.toml").write_text("fixture")
        (directory / "LICENSE").write_text("Copyright Example\nPermission granted.\n")
        return {"id": name, "name": name, "version": version,
                "manifest_path": str(directory / "Cargo.toml"),
                "license": license_name, "license_file": None,
                "targets": [{"kind": [kind]}]}

    def graph(self):
        packages = [self.package("app", kind="bin"), self.package("runtime"),
                    self.package("build"), self.package("dev"),
                    self.package("macro", kind="proc-macro"), self.package("helper")]
        return {"packages": packages, "resolve": {"root": "app"}}

    def test_runtime_tree_excludes_dev_unified_packages_and_deduplicates(self):
        self.assertEqual([p["name"] for p in licenses.runtime_packages(
            self.graph(), "app v1.0.0\nruntime v1.0.0\nruntime v1.0.0 (*)\n")],
            ["runtime"])

    def test_runtime_tree_resolves_transitive_packages_in_stable_order(self):
        self.assertEqual([p["name"] for p in licenses.runtime_packages(
            self.graph(), "app v1.0.0\nruntime v1.0.0\nhelper v1.0.0\n")],
            ["helper", "runtime"])

    def test_runtime_tree_rejects_unknown_ambiguous_or_malformed_sources(self):
        metadata = self.graph()
        for tree in ("", "invalid", "unknown v1.0.0"):
            with self.subTest(tree=tree), self.assertRaises(licenses.LicenseError):
                licenses.runtime_packages(metadata, tree)
        metadata["packages"].append(dict(metadata["packages"][1]))
        with self.assertRaises(licenses.LicenseError):
            licenses.runtime_packages(metadata, "runtime v1.0.0")

    def test_cargo_commands_are_offline_locked_and_select_runtime_target(self):
        from subprocess import CompletedProcess
        with patch.object(licenses.subprocess, "run", return_value=CompletedProcess(
                [], 0, stdout=json.dumps(self.graph()), stderr="")) as run:
            licenses.cargo_metadata(self.root)
            command = run.call_args.args[0]
            self.assertIn("--offline", command)
            self.assertIn("--locked", command)
            self.assertIn("--filter-platform", command)
            self.assertIn(licenses.RUST_TARGET, command)
        with patch.object(licenses.subprocess, "run", return_value=CompletedProcess(
                [], 0, stdout="runtime v1.0.0", stderr="")) as run:
            self.assertEqual(licenses.cargo_runtime_tree(self.root), "runtime v1.0.0")
            command = run.call_args.args[0]
            for flag in ("--offline", "--locked", "normal,no-proc-macro", licenses.RUST_TARGET):
                self.assertIn(flag, command)
        with patch.object(licenses.subprocess, "run", return_value=CompletedProcess(
                [], 1, stdout="", stderr="cache missing")):
            for operation in (licenses.cargo_metadata, licenses.cargo_runtime_tree):
                with self.assertRaisesRegex(licenses.LicenseError, "cache missing"):
                    operation(self.root)

    def test_metadata_requires_single_root(self):
        metadata = self.graph()
        metadata["resolve"]["root"] = None
        with self.assertRaises(licenses.LicenseError):
            licenses.runtime_packages(metadata, "runtime v1.0.0\n")

    def test_collects_all_license_and_notice_texts(self):
        package = self.package("runtime")
        directory = Path(package["manifest_path"]).parent
        (directory / "LICENSE.httprouter").write_text("BSD grant\n")
        (directory / "NOTICE").write_text("Required notice\n")
        (directory / "README.md").write_text("Not a license\n")
        documents = licenses.license_documents(directory)
        self.assertEqual([name for name, _ in documents],
                         ["LICENSE", "LICENSE.httprouter", "NOTICE"])
        self.assertIn("Required notice", documents[-1][1])

    def test_license_directory_is_collected_and_symlinks_cannot_escape(self):
        directory = self.root / "license-directory"
        directory.mkdir()
        grants = directory / "LICENSES"
        grants.mkdir()
        (grants / "MIT.txt").write_text("Full grant\n")
        self.assertEqual(licenses.license_documents(directory), [("LICENSES/MIT.txt", "Full grant\n")])
        (directory / "NOTICE").symlink_to(self.root / "Cargo.lock")
        with self.assertRaises(licenses.LicenseError):
            licenses.license_documents(directory)

    def test_explicit_license_file_is_included(self):
        directory = self.root / "explicit"
        directory.mkdir()
        (directory / "grant.txt").write_text("License grant\r\n")
        self.assertEqual(licenses.license_documents(directory, "grant.txt"),
                         [("grant.txt", "License grant\n")])

    def test_missing_or_empty_license_fails_closed(self):
        directory = self.root / "missing"
        directory.mkdir()
        with self.assertRaises(licenses.LicenseError):
            licenses.license_documents(directory)
        (directory / "LICENSE").write_text("  \n")
        with self.assertRaises(licenses.LicenseError):
            licenses.license_documents(directory)

    def test_license_path_must_stay_inside_package(self):
        with self.assertRaises(licenses.LicenseError):
            licenses.license_documents(self.root / "web", "../Cargo.lock")

    def test_no_npm_runtime_dependencies_requires_no_node_modules(self):
        self.assertEqual(licenses.npm_packages(self.root), [])

    def add_npm_package(self, path="node_modules/library"):
        lock_path = self.root / "web/package-lock.json"
        lock = json.loads(lock_path.read_text())
        lock["packages"][path] = {"version": "2.0.0", "license": "MIT"}
        lock_path.write_text(json.dumps(lock))
        directory = self.root / "web" / path
        directory.mkdir(parents=True)
        (directory / "package.json").write_text(json.dumps({
            "name": "library", "version": "2.0.0", "license": "MIT"}))
        (directory / "LICENSE").write_text("Npm license grant\n")
        return directory

    def test_npm_runtime_licenses_are_read_from_matching_installed_package(self):
        self.add_npm_package()
        packages = licenses.npm_packages(self.root)
        self.assertEqual(packages[0]["name"], "library")
        self.assertEqual(packages[0]["version"], "2.0.0")
        self.assertIn("Npm license grant", packages[0]["documents"][0][1])

    def test_missing_npm_runtime_package_fails_closed(self):
        directory = self.add_npm_package()
        (directory / "package.json").unlink()
        with self.assertRaises(licenses.LicenseError):
            licenses.npm_packages(self.root)

    def test_mismatched_npm_runtime_package_fails_closed(self):
        directory = self.add_npm_package()
        (directory / "package.json").write_text('{"name":"library","version":"3.0.0"}')
        with self.assertRaises(licenses.LicenseError):
            licenses.npm_packages(self.root)

    def test_npm_links_and_escaping_paths_fail_closed(self):
        self.add_npm_package()
        lock_path = self.root / "web/package-lock.json"
        lock = json.loads(lock_path.read_text())
        lock["packages"]["node_modules/library"]["link"] = True
        lock_path.write_text(json.dumps(lock))
        with self.assertRaises(licenses.LicenseError):
            licenses.npm_packages(self.root)
        lock["packages"]["../outside"] = lock["packages"].pop("node_modules/library")
        lock_path.write_text(json.dumps(lock))
        with self.assertRaises(licenses.LicenseError):
            licenses.npm_packages(self.root)

    def test_invalid_npm_lock_and_missing_license_declaration_fail_closed(self):
        lock_path = self.root / "web/package-lock.json"
        lock_path.write_text('{"lockfileVersion":2}')
        with self.assertRaises(licenses.LicenseError):
            licenses.npm_packages(self.root)
        lock_path.write_text('{"lockfileVersion":3,"packages":{}}')
        directory = self.add_npm_package()
        manifest = directory / "package.json"
        manifest.write_text('{"name":"library","version":"2.0.0"}')
        lock = json.loads(lock_path.read_text())
        del lock["packages"]["node_modules/library"]["license"]
        lock_path.write_text(json.dumps(lock))
        with self.assertRaises(licenses.LicenseError):
            licenses.npm_packages(self.root)

    def test_cli_fails_closed_when_sources_are_unavailable(self):
        with patch.object(licenses, "generate", side_effect=licenses.LicenseError("cache missing")):
            self.assertEqual(licenses.main(["--root", str(self.root)]), 1)
        self.assertFalse((self.root / "THIRD_PARTY_LICENSES.txt").exists())

    def test_generation_is_deterministic_and_contains_full_license(self):
        metadata = self.graph()
        with patch.object(licenses, "cargo_metadata", return_value=metadata), \
                patch.object(licenses, "cargo_runtime_tree", return_value="app v1.0.0\nruntime v1.0.0\n"):
            first = licenses.generate(self.root)
            self.assertEqual(first, licenses.generate(self.root))
        self.assertIn("Copyright Example\nPermission granted.", first)
        self.assertIn("No npm runtime dependencies", first)
        self.assertNotIn(str(self.root), first)

    def test_manifest_metadata_and_development_dependencies_do_not_change_output(self):
        metadata = self.graph()
        with patch.object(licenses, "cargo_metadata", return_value=metadata), \
                patch.object(licenses, "cargo_runtime_tree", return_value="app v1.0.0\nruntime v1.0.0\n"):
            original = licenses.generate(self.root)
            for relative in ("Cargo.toml", "Cargo.lock", "web/package.json", "web/package-lock.json"):
                path = self.root / relative
                before = path.read_text()
                path.write_text(before + "\n")
                self.assertEqual(original, licenses.generate(self.root))
                path.write_text(before)
            lock_path = self.root / "web/package-lock.json"
            lock = json.loads(lock_path.read_text())
            lock["packages"]["node_modules/compiler"]["version"] = "2.0.0"
            lock_path.write_text(json.dumps(lock))
            self.assertEqual(original, licenses.generate(self.root))
        self.assertNotIn("SHA-256", original)

    def test_runtime_version_and_license_text_changes_update_output(self):
        metadata = self.graph()
        with patch.object(licenses, "cargo_metadata", return_value=metadata), \
                patch.object(licenses, "cargo_runtime_tree", return_value="app v1.0.0\nruntime v1.0.0\n") as tree:
            original = licenses.generate(self.root)
            package = metadata["packages"][1]
            source = Path(package["manifest_path"]).parent / "LICENSE"
            source.write_text("Updated license grant\n")
            self.assertNotEqual(original, licenses.generate(self.root))
            source.write_text("Copyright Example\nPermission granted.\n")
            package["version"] = "2.0.0"
            tree.return_value = "app v1.0.0\nruntime v2.0.0\n"
            self.assertNotEqual(original, licenses.generate(self.root))

    def test_application_version_does_not_change_runtime_notices(self):
        metadata = self.graph()
        with patch.object(licenses, "cargo_metadata", return_value=metadata), \
                patch.object(licenses, "cargo_runtime_tree", return_value="app v1.0.0\nruntime v1.0.0\n") as tree:
            original = licenses.generate(self.root)
            metadata["packages"][0]["version"] = "2.0.0"
            tree.return_value = "app v2.0.0\nruntime v1.0.0\n"
            self.assertEqual(original, licenses.generate(self.root))

    def test_cli_writes_generated_output(self):
        metadata = self.graph()
        with patch.object(licenses, "cargo_metadata", return_value=metadata), \
                patch.object(licenses, "cargo_runtime_tree", return_value="app v1.0.0\nruntime v1.0.0\n"):
            self.assertEqual(licenses.main(["--root", str(self.root)]), 0)
            output = self.root / "THIRD_PARTY_LICENSES.txt"
            self.assertEqual(output.read_text(), licenses.generate(self.root))


if __name__ == "__main__":
    unittest.main()

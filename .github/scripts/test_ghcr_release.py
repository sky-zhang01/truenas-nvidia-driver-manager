"""Offline controls for publication policy and registry failure handling."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError, URLError

IMAGE = "ghcr.io/example-owner/driver-manager"


def load_release(environment):
    spec = importlib.util.spec_from_file_location("ghcr_release", Path(__file__).with_name("ghcr_release.py"))
    module = importlib.util.module_from_spec(spec)
    with patch.dict(os.environ, environment, clear=True):
        spec.loader.exec_module(module)
    return module


r = load_release({"GHCR_IMAGE": IMAGE})
SHA = "a" * 40
CONFIG = "sha256:" + "b" * 64
DIGEST = "sha256:" + "c" * 64


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.cwd = os.getcwd()
        os.chdir(self.directory.name)
        self.addCleanup(os.chdir, self.cwd)
        Path("Cargo.toml").write_text('[package]\nversion = "0.2.0"\n')
        self.environment = patch.dict(os.environ, {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/tags/v0.2.0",
            "GITHUB_SHA": SHA, "GITHUB_RUN_ID": "123", "GITHUB_OUTPUT": "outputs",
            "GHCR_TOKEN": "test-token", "GITHUB_ACTOR": "test-actor", "GHCR_IMAGE": IMAGE}, clear=True)
        Path("nvm-build-metadata.json").write_text(json.dumps({"containerimage.config.digest": CONFIG,
            "buildx.build.provenance": {"buildType": "https://mobyproject.org/buildkit@v1"}}))
        Path("nvm-image.spdx.json").write_text('{"spdxVersion":"SPDX-2.3","packages":[{}]}')
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def test_image_configuration_selects_the_repository_package(self):
        for image in (IMAGE, "ghcr.io/fork-owner/driver_manager.test--image", "ghcr.io/a/b__c"):
            with self.subTest(image=image):
                module = load_release({"GHCR_IMAGE": image})
                self.assertEqual(module.IMAGE, image)
                self.assertEqual(module.PACKAGE, image.removeprefix("ghcr.io/"))
                self.assertEqual(module.API, "https://ghcr.io/v2/" + image.removeprefix("ghcr.io/"))

    def test_image_configuration_rejects_missing_and_invalid_names(self):
        cases = ({}, *({"GHCR_IMAGE": image} for image in (
            "", "GHCR.io/owner/repository", "ghcr.io/Owner/repository", "ghcr.io/owner/Repository",
            "example.invalid/owner/repository", "https://ghcr.io/owner/repository",
            "ghcr.io/owner", "ghcr.io//repository", "ghcr.io/owner/", "ghcr.io/owner/repository/extra",
            "ghcr.io/owner/repository:latest", "ghcr.io/owner/repository@" + DIGEST,
            "ghcr.io/owner/../repository", "ghcr.io/owner/.repository", "ghcr.io/owner/repository.",
            "ghcr.io/-owner/repository", "ghcr.io/owner-/repository", "ghcr.io/owner/repo___name",
            "ghcr.io/owner/repository?tag=latest", "ghcr.io/owner/repository#fragment",
            "user@" + IMAGE, "ghcr.io:443/owner/repository",
            " ghcr.io/owner/repository", "ghcr.io/owner/repository ", "ghcr.io/owner/repository\n",
            "ghcr.io/owner/repo name", "ghcr.io/owner/répository", "ghcr.io/owner/repo%2fname")))
        for environment in cases:
            with self.subTest(environment=environment), self.assertRaisesRegex(ValueError, "GHCR_IMAGE"):
                load_release(environment)

    @patch.object(r, "request", return_value=(b'{"token":"test-registry-token"}', {}))
    def test_registry_uses_the_configured_package(self, request):
        registry = r.Registry()
        self.assertEqual(request.call_args.args[0],
                         "https://ghcr.io/token?service=ghcr.io&scope=repository:example-owner/driver-manager:pull,push")
        registry.get("/manifests/0.2.0")
        self.assertEqual(request.call_args.args[0], "https://ghcr.io/v2/example-owner/driver-manager/manifests/0.2.0")

    def test_semver_order(self):
        self.assertFalse(r.highest_stable("0.2.0", ["0.10.0"]))
        self.assertFalse(r.highest_stable("0.2.0-rc.1", ["0.1.0"]))
        self.assertTrue(r.highest_stable("0.2.0", ["latest", "invalid", "0.2.0", "0.3.0-rc.1"]))
        self.assertTrue(r.highest_stable("1.0.0", ["0.99.99"]))

    def test_reject_invalid_versions(self):
        for version in ("01.2.3", "1.2", "1.2.3+build", "1.2.3-01", "1.2.3-a..b", "1.2.3\n", "1.2.3-" + "x" * 128):
            with self.subTest(version=version), self.assertRaises(ValueError):
                r.version_key(version)

    def test_package_version_uses_the_package_section(self):
        for declaration in ('version = "0.2.0"', "version = '0.2.0' # release"):
            Path("Cargo.toml").write_text('[package] # application\nname = "example"\n' + declaration +
                '\n[package.metadata]\nversion = "9.0.0"\n[dependencies.example]\nversion = "8.0.0"\n')
            self.assertEqual(r.package_version(), "0.2.0")

    def test_package_version_rejects_ambiguous_or_unsupported_input(self):
        for text in ('[workspace.package]\nversion = "0.2.0"\n',
                     '[package]\nname = "example"\n',
                     '[package]\nversion.workspace = true\n',
                     '[package]\nversion = "0.2.0"\nversion = "0.3.0"\n',
                     '[package]\nversion = "0.2.0"\n[package]\nname = "duplicate"\n',
                     '[package]\nversion = 2\n',
                     '[package]\nversion = "0.2.0" + "suffix"\n',
                     '[package]\ndescription = """\nversion = "0.2.0"\n"""\n',
                     '[package]\nversion = "0.2.0"\nversion.workspace = true\n',
                     'description = """\n[package]\nversion = "0.2.0"\n[placeholder]\n"""\n["package"]\nversion = "0.3.0"\n'):
            Path("Cargo.toml").write_text(text)
            with self.subTest(text=text), self.assertRaises(ValueError):
                r.package_version()

    @patch.object(r.subprocess, "run")
    @patch.object(r, "run", return_value=SHA)
    def test_tag_commit_is_on_main(self, git, process):
        self.assertEqual(r.validate(), "0.2.0")
        process.assert_called_once_with(("git", "merge-base", "--is-ancestor", SHA, "origin/main"), check=True)
        self.assertEqual(git.call_args_list[1].args, ("git", "rev-parse", "refs/tags/v0.2.0^{commit}"))

    @patch.object(r.subprocess, "run")
    @patch.object(r, "run", return_value=SHA)
    def test_invalid_tag_event_version_and_commit_fail(self, git, process):
        cases = ({"GITHUB_EVENT_NAME": "workflow_dispatch"}, {"GITHUB_REF": "refs/heads/main"},
                 {"GITHUB_REF": "refs/tags/v0.3.0"}, {"GITHUB_SHA": "d" * 40})
        for change in cases:
            with self.subTest(change=change), patch.dict(os.environ, change), self.assertRaises(ValueError):
                r.validate()
        git.side_effect = [SHA, "d" * 40]
        with self.assertRaises(ValueError):
            r.validate()
        process.assert_not_called()

    @patch.object(r.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "git"))
    @patch.object(r, "run", return_value=SHA)
    def test_off_main_fails(self, git, process):
        with self.assertRaises(subprocess.CalledProcessError):
            r.validate()

    def registry(self):
        registry = object.__new__(r.Registry)
        registry.headers = {}
        return registry

    def http_error(self, code):
        error = HTTPError("https://ghcr.io/test", code, "test", {}, io.BytesIO())
        self.addCleanup(error.close)
        return error

    def test_only_404_means_absent(self):
        registry = self.registry()
        for code in (401, 403, 429, 500, 502, 503, 504):
            with self.subTest(code=code), patch.object(r, "request", side_effect=self.http_error(code)), self.assertRaises(HTTPError):
                registry.manifest("0.2.0", missing=True)
        with patch.object(r, "request", side_effect=self.http_error(404)):
            self.assertIsNone(registry.manifest("0.2.0", missing=True))
            with self.assertRaises(HTTPError):
                registry.manifest("0.2.0")

    def test_registry_digest_checked_from_bytes(self):
        body = b'{"config":{"digest":"' + CONFIG.encode() + b'"}}'
        digest = "sha256:" + hashlib.sha256(body).hexdigest()
        registry = self.registry()
        with patch.object(r, "request", return_value=(body, {"Docker-Content-Digest": digest})):
            self.assertEqual(r.readback(registry, "0.2.0", CONFIG), digest)
        with patch.object(r, "request", return_value=(body, {"Docker-Content-Digest": DIGEST})), self.assertRaises(ValueError):
            r.readback(registry, "0.2.0", CONFIG)

    def test_readback_rejects_other_config_and_wrong_platform(self):
        registry = Mock()
        registry.manifest.return_value = ({"config": {"digest": CONFIG}}, DIGEST)
        with self.assertRaises(ValueError):
            r.readback(registry, "0.2.0", DIGEST)
        for manifests in ([], [{"digest": CONFIG, "platform": {"os": "linux", "architecture": "arm64"}}],
                          [{"digest": CONFIG, "platform": {"os": "linux", "architecture": "amd64"}}] * 2):
            registry.manifest.return_value = ({"manifests": manifests}, DIGEST)
            with self.subTest(manifests=manifests), self.assertRaises(ValueError):
                r.readback(registry, "0.2.0", CONFIG)

    def test_index_readback_and_child_digest(self):
        registry = Mock()
        index = {"manifests": [{"digest": CONFIG, "platform": {"os": "linux", "architecture": "amd64"}},
            {"annotations": {"vnd.docker.reference.type": "attestation-manifest"}}]}
        registry.manifest.side_effect = [(index, DIGEST), ({"config": {"digest": CONFIG}}, CONFIG)]
        self.assertEqual(r.readback(registry, "0.2.0", CONFIG), DIGEST)
        registry.manifest.side_effect = [(index, DIGEST), ({"config": {"digest": CONFIG}}, DIGEST)]
        with self.assertRaises(ValueError):
            r.readback(registry, "0.2.0", CONFIG)

    def test_tag_pages_and_reject_foreign_pagination(self):
        registry = self.registry()
        with patch.object(registry, "get", side_effect=[(json.dumps({'name': r.PACKAGE, 'tags': ['0.2.0']}).encode(), {"Link": '</v2/' + r.PACKAGE + '/tags/list?last=0.2.0>; rel="next"'}),
                                                       (json.dumps({'name': r.PACKAGE, 'tags': ['0.10.0']}).encode(), {})]):
            self.assertEqual(registry.tags(), ["0.2.0", "0.10.0"])
        for link in ('<https://example.invalid/tags/list>; rel="next"', "broken", '</v2/' + r.PACKAGE + '/tags/list?n=100>; rel="next"'):
            with self.subTest(link=link), patch.object(registry, "get", return_value=(json.dumps({'name': r.PACKAGE, 'tags': []}).encode(), {"Link": link})), self.assertRaises(ValueError):
                registry.tags()
        with patch.object(registry, "get", return_value=None):
            self.assertEqual(registry.tags(), [])

    def test_invalid_registry_tag_inventory_fails_closed(self):
        registry = self.registry()
        for page in ({}, {'name': 'other/package', 'tags': []}, {'name': r.PACKAGE, 'tags': '0.2.0'},
                     {'name': r.PACKAGE, 'tags': [None]}):
            with self.subTest(page=page), patch.object(registry, "get", return_value=(json.dumps(page).encode(), {})), self.assertRaises(ValueError):
                registry.tags()

    @patch.object(r.time, "sleep")
    def test_bounded_registry_retries(self, sleep):
        response = Mock()
        response.__enter__ = Mock(return_value=response)
        response.__exit__ = Mock(return_value=False)
        response.read.return_value = b"ok"
        response.headers = {}
        with patch.object(r, "urlopen", side_effect=[self.http_error(503), response]) as opening:
            self.assertEqual(r.request("https://ghcr.io/test", {}), (b"ok", {}))
            self.assertEqual(opening.call_count, 2)
        with patch.object(r, "urlopen", side_effect=URLError("offline")) as opening, self.assertRaises(URLError):
            r.request("https://ghcr.io/test", {})
        self.assertEqual(opening.call_count, 5)
        with patch.object(r, "urlopen", side_effect=self.http_error(403)) as opening, self.assertRaises(HTTPError):
            r.request("https://ghcr.io/test", {})
        self.assertEqual(opening.call_count, 1)

    def image_archive(self, store="classic"):
        """Write an archive as docker save does; return the tested ID, config and manifest digests."""
        config = json.dumps({"os": "linux", "architecture": "amd64"}).encode()
        config_digest = "sha256:" + hashlib.sha256(config).hexdigest()
        manifest = json.dumps({"mediaType": "application/vnd.oci.image.manifest.v1+json",
                               "config": {"digest": config_digest}, "layers": []}).encode()
        manifest_digest = "sha256:" + hashlib.sha256(manifest).hexdigest()
        self.write_archive({"manifest.json": json.dumps([{"Config": "blobs/sha256/" + config_digest[7:]}]).encode(),
                            "index.json": json.dumps({"manifests": [{"digest": manifest_digest}]}).encode(),
                            "blobs/sha256/" + config_digest[7:]: config,
                            "blobs/sha256/" + manifest_digest[7:]: manifest})
        provenance = {"buildType": "https://mobyproject.org/buildkit@v1"}
        if store == "classic":
            image_id = config_digest
            metadata = {"containerimage.config.digest": config_digest, "containerimage.digest": DIGEST,
                        "buildx.build.provenance": provenance}
        else:
            image_id = manifest_digest
            metadata = {"containerimage.digest": manifest_digest, "buildx.build.provenance": provenance}
        Path("nvm-image.id").write_text(image_id)
        Path("nvm-build-metadata.json").write_text(json.dumps(metadata))
        return image_id, config_digest, manifest_digest

    @staticmethod
    def write_archive(members):
        with tarfile.open("nvm-image.tar", "w") as archive:
            for name, body in members.items():
                entry = tarfile.TarInfo(name)
                entry.size = len(body)
                archive.addfile(entry, io.BytesIO(body))

    @staticmethod
    def docker(*loaded):
        """Answer docker load with the given image IDs and docker image inspect with its argument."""
        return lambda *args: ("\n".join("Loaded image ID: " + value for value in loaded)
                              if args[1] == "load" else args[-1])

    def test_artifact_binds_either_store_and_loads_into_either_store(self):
        for store in ("classic", "containerd"):
            image_id, config_digest, manifest_digest = self.image_archive(store)
            for loaded in (config_digest, manifest_digest):
                with self.subTest(store=store, loaded=loaded), patch.object(r, "run", side_effect=self.docker(loaded)):
                    self.assertEqual(r.artifact(), (loaded, config_digest))

    def test_artifact_rejects_archive_not_matching_tested_id_before_load(self):
        for store in ("classic", "containerd"):
            self.image_archive(store)
            Path("nvm-image.id").write_text(CONFIG)
            with self.subTest(store=store), patch.object(r, "run") as docker:
                with self.assertRaisesRegex(ValueError, "archive.*tested image"):
                    r.artifact()
                docker.assert_not_called()

    def test_artifact_ignores_index_entries_whose_bytes_differ(self):
        _, config_digest, _ = self.image_archive("containerd")
        manifest = json.dumps({"config": {"digest": config_digest}}).encode()
        self.write_archive({"manifest.json": json.dumps([{"Config": "blobs/sha256/" + config_digest[7:]}]).encode(),
                            "index.json": json.dumps({"manifests": [{"digest": DIGEST}]}).encode(),
                            "blobs/sha256/" + config_digest[7:]: json.dumps({"os": "linux", "architecture": "amd64"}).encode(),
                            "blobs/sha256/" + DIGEST[7:]: manifest})
        Path("nvm-image.id").write_text(DIGEST)
        Path("nvm-build-metadata.json").write_text(json.dumps({"containerimage.digest": DIGEST,
            "buildx.build.provenance": {"buildType": "https://mobyproject.org/buildkit@v1"}}))
        with patch.object(r, "run") as docker, self.assertRaisesRegex(ValueError, "archive.*tested image"):
            r.artifact()
        docker.assert_not_called()

    def test_artifact_rejects_a_loaded_image_outside_the_archive(self):
        _, config_digest, manifest_digest = self.image_archive("containerd")
        for loaded in ((DIGEST,), (), (config_digest, manifest_digest)):
            with self.subTest(loaded=loaded), patch.object(r, "run", side_effect=self.docker(*loaded)):
                with self.assertRaisesRegex(ValueError, "loaded image"):
                    r.artifact()
        with patch.object(r, "run", side_effect=lambda *args: "Loaded image ID: " + config_digest if args[1] == "load" else DIGEST):
            with self.assertRaisesRegex(ValueError, "loaded image"):
                r.artifact()

    def test_artifact_requires_matching_metadata_and_native_provenance(self):
        provenance = {"buildType": "https://mobyproject.org/buildkit@v1"}
        for store in ("classic", "containerd"):
            image_id, config_digest, manifest_digest = self.image_archive(store)
            for metadata in (None, [], {"buildx.build.provenance": provenance},
                             {"containerimage.config.digest": CONFIG, "containerimage.digest": image_id,
                              "buildx.build.provenance": provenance},
                             {"containerimage.digest": DIGEST, "buildx.build.provenance": provenance},
                             {"containerimage.config.digest": config_digest},
                             {"containerimage.config.digest": config_digest, "buildx.build.provenance": {}},
                             {"containerimage.digest": manifest_digest, "buildx.build.provenance": []}):
                Path("nvm-build-metadata.json").write_text(json.dumps(metadata))
                with self.subTest(store=store, metadata=metadata), patch.object(r, "run") as docker:
                    with self.assertRaises(ValueError):
                        r.artifact()
                    docker.assert_not_called()

    def test_receipt_binds_evidence_bytes_and_describes_record_mode(self):
        self.assertEqual(r.attestation_mode(), "github")
        for mode in ("github", "build-record"):
            with self.subTest(mode=mode), patch.dict(os.environ, {"GHCR_ATTESTATION_MODE": mode}):
                r.receipt("0.2.0", DIGEST, False)
                receipt = json.loads(Path("ghcr-receipt.json").read_text())
                self.assertEqual(receipt["image"], IMAGE)
                self.assertEqual(receipt["attestation_mode"], mode)
                self.assertEqual(receipt["github_attestations"],
                                 "requested" if mode == "github" else "not-requested")
                self.assertEqual(receipt["build_record"]["sha256"],
                                 hashlib.sha256(Path("nvm-build-metadata.json").read_bytes()).hexdigest())
                self.assertFalse(receipt["build_record"]["github_signed"])
                self.assertEqual(receipt["sbom"]["sha256"],
                                 hashlib.sha256(Path("nvm-image.spdx.json").read_bytes()).hexdigest())

    @patch.object(r, "validate")
    @patch.object(r, "artifact", return_value=(CONFIG, CONFIG))
    @patch.object(r, "Registry")
    def test_artifact_command_checks_a_build_without_a_tag(self, registry, artifact, validate):
        with patch.dict(os.environ, {"GITHUB_EVENT_NAME": "pull_request", "GITHUB_REF": "refs/pull/1/merge"}):
            r.main("artifact")
        artifact.assert_called_once_with()
        validate.assert_not_called()
        registry.assert_not_called()

    def test_artifact_command_rejects_an_image_id_outside_the_archive(self):
        self.image_archive("containerd")
        Path("nvm-image.id").write_text(DIGEST)
        with patch.object(r, "run") as docker, self.assertRaisesRegex(ValueError, "archive.*tested image"):
            r.main("artifact")
        docker.assert_not_called()

    @patch.object(r, "validate", return_value="0.2.0")
    @patch.object(r, "artifact", return_value=(CONFIG, CONFIG))
    @patch.object(r, "Registry")
    def test_publication_requires_the_tested_sbom(self, registry, artifact, validate):
        for body in ('{"spdxVersion":"SPDX-2.3","packages":[]}', '{"packages":[{}]}'):
            Path("nvm-image.spdx.json").write_text(body)
            with self.subTest(body=body), self.assertRaisesRegex(ValueError, "SBOM"):
                r.main("publish")
        registry.assert_not_called()

    @patch.object(r, "validate", return_value="0.2.0")
    @patch.object(r, "artifact", return_value=(CONFIG, CONFIG))
    @patch.object(r, "Registry")
    def test_unknown_attestation_mode_cannot_publish(self, registry, artifact, validate):
        with patch.dict(os.environ, {"GHCR_ATTESTATION_MODE": "unsigned"}), self.assertRaises(ValueError):
            r.main("publish")
        registry.assert_not_called()

    @patch.object(r, "validate", return_value="0.2.0")
    @patch.object(r, "artifact", return_value=(CONFIG, CONFIG))
    @patch.object(r, "Registry")
    @patch.object(r.subprocess, "run")
    def test_publish_refuses_existing_version(self, process, registry, artifact, validate):
        registry.return_value.manifest.return_value = ({"config": {"digest": DIGEST}}, DIGEST)
        with self.assertRaises(ValueError):
            r.main("publish")
        process.assert_not_called()

    @patch.object(r, "validate", return_value="0.2.0")
    @patch.object(r, "artifact", return_value=(CONFIG, CONFIG))
    @patch.object(r, "Registry")
    @patch.object(r.subprocess, "run")
    def test_publish_resumes_identical_version_without_push(self, process, registry, artifact, validate):
        registry.return_value.manifest.return_value = ({"config": {"digest": CONFIG}}, DIGEST)
        r.main("publish")
        process.assert_not_called()
        self.assertEqual(Path("outputs").read_text(), "digest=" + DIGEST + "\n")

    @patch.object(r, "validate", return_value="0.2.0")
    @patch.object(r, "artifact", return_value=(CONFIG, CONFIG))
    @patch.object(r, "Registry")
    @patch.object(r.subprocess, "run")
    @patch.object(r, "readback", return_value=DIGEST)
    def test_publish_uses_loaded_image_and_records_digest(self, readback, process, registry, artifact, validate):
        registry.return_value.manifest.return_value = None
        r.main("publish")
        self.assertEqual([c.args[0] for c in process.call_args_list], [
            ("docker", "tag", CONFIG, r.IMAGE + ":0.2.0"), ("docker", "push", r.IMAGE + ":0.2.0")])
        self.assertEqual(Path("outputs").read_text(), "digest=" + DIGEST + "\n")
        self.assertFalse(json.loads(Path("ghcr-receipt.json").read_text())["latest"])

    @patch.object(r, "validate", return_value="0.2.0")
    @patch.object(r, "artifact", return_value=(CONFIG, CONFIG))
    @patch.object(r, "Registry")
    @patch.object(r.subprocess, "run")
    @patch.object(r, "run", return_value="v0.1.0")
    @patch.object(r, "readback", return_value=DIGEST)
    def test_latest_only_for_highest_published_stable(self, readback, git, process, registry, artifact, validate):
        os.environ["GHCR_DIGEST"] = DIGEST
        registry.return_value.tags.return_value = ["0.2.0", "0.10.0"]
        r.main("promote")
        process.assert_not_called()
        registry.return_value.tags.return_value = ["0.1.0", "0.2.0"]
        r.main("promote")
        self.assertEqual(process.call_args_list[-1].args[0], ("docker", "push", r.IMAGE + ":latest"))
        readback.side_effect = [DIGEST, CONFIG]
        with self.assertRaises(ValueError):
            r.main("promote")


if __name__ == "__main__":
    unittest.main()

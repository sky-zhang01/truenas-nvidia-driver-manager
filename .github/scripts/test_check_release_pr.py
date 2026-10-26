"""Offline checks for the public tag publication gate."""
import importlib.util
import io
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("check_release_pr", Path(__file__).with_name("check_release_pr.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
SHA = "a" * 40
REPO = "example/driver-manager"
TAG = "v1.2.3"


def pull(**changes):
    value = {"number": 7, "state": "closed", "merged_at": "2026-01-01T00:00:00Z",
             "merge_commit_sha": SHA, "base": {"ref": "main", "repo": {"full_name": REPO}},
             "head": {"ref": "sync/" + TAG, "repo": {"full_name": REPO}}}
    value.update(changes)
    return value


class ReleasePullRequestTests(unittest.TestCase):
    def test_only_the_exact_merged_main_result_qualifies(self):
        self.assertTrue(gate.merged_into_main(pull(), SHA, REPO, TAG))
        for value in (pull(state="open"), pull(merged_at=None), pull(merge_commit_sha="b" * 40),
                      pull(base={"ref": "other", "repo": {"full_name": REPO}}),
                      pull(base={"ref": "main", "repo": {"full_name": "other/repo"}}), {}, None):
            with self.subTest(value=value):
                self.assertFalse(gate.merged_into_main(value, SHA, REPO, TAG))

    def test_only_same_repository_sync_branch_for_this_tag_qualifies(self):
        for head in ({"ref": "feature/release", "repo": {"full_name": REPO}},
                     {"ref": "sync/v1.2.4", "repo": {"full_name": REPO}},
                     {"ref": "sync/" + TAG, "repo": {"full_name": "fork/driver-manager"}},
                     {"ref": "sync/" + TAG, "repo": None}, None, {}):
            with self.subTest(head=head):
                self.assertFalse(gate.merged_into_main(pull(head=head), SHA, REPO, TAG))

    @patch.object(gate, "urlopen")
    def test_commit_association_alone_does_not_authorize_publication(self, request):
        response = io.BytesIO(json.dumps([pull(merged_at=None)]).encode())
        request.return_value = response
        with self.assertRaisesRegex(ValueError, "merged pull request"):
            gate.check(REPO, SHA, "fixture-token", TAG)

    @patch.object(gate, "urlopen")
    def test_pagination_and_authorization(self, request):
        request.side_effect = [io.BytesIO(json.dumps([pull(merged_at=None)] * 100).encode()),
                               io.BytesIO(json.dumps([pull()]).encode())]
        self.assertEqual(gate.check(REPO, SHA, "fixture-token", TAG), 7)
        self.assertEqual(request.call_count, 2)
        req = request.call_args.args[0]
        self.assertEqual(req.full_url, "https://api.github.com/repos/" + REPO + "/commits/" + SHA + "/pulls?per_page=100&page=2")
        self.assertEqual(req.get_header("Authorization"), "Bearer fixture-token")

    @patch.object(gate, "urlopen")
    def test_invalid_inputs_and_api_errors_fail_closed(self, request):
        for repo, sha, token in (("../repo", SHA, "token"), (REPO, "bad", "token"), (REPO, SHA, "")):
            with self.subTest(repo=repo, sha=sha), self.assertRaises(ValueError):
                gate.check(repo, sha, token, TAG)
        request.assert_not_called()
        request.return_value = io.BytesIO(b'{"message":"failure"}')
        with self.assertRaisesRegex(ValueError, "list"):
            gate.check(REPO, SHA, "fixture-token", TAG)

    @patch.object(gate, "urlopen")
    def test_ordinary_merged_pr_cannot_publish_and_invalid_tag_never_requests(self, request):
        request.return_value = io.BytesIO(json.dumps([pull(head={"ref": "feature/release", "repo": {"full_name": REPO}})]).encode())
        with self.assertRaisesRegex(ValueError, "merged pull request"):
            gate.check(REPO, SHA, "fixture-token", TAG)
        request.reset_mock()
        for tag in ("", "main", "v1.2.3\ninjected", "v1.2.3/other"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                gate.check(REPO, SHA, "fixture-token", tag)
        request.assert_not_called()

    @patch.object(gate, "check", return_value=7)
    @patch.object(gate.subprocess, "check_output", return_value=SHA + "\n")
    def test_main_binds_api_gate_to_the_tag_checkout(self, command, check):
        environment = {"GITHUB_REPOSITORY": REPO, "GITHUB_SHA": SHA, "GH_TOKEN": "fixture-token", "GITHUB_REF_NAME": TAG}
        with patch.dict(os.environ, environment, clear=True):
            gate.main()
            check.assert_called_once_with(REPO, SHA, "fixture-token", TAG)
            command.return_value = "b" * 40 + "\n"
            with self.assertRaisesRegex(ValueError, "checkout"):
                gate.main()


if __name__ == "__main__":
    unittest.main()

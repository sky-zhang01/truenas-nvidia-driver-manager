#!/usr/bin/env python3
"""Require the public release commit to be a merged same-repository sync PR."""
import json
import os
import re
import subprocess
import sys
from urllib.request import Request, urlopen


def merged_into_main(pull, sha, repository, tag):
    if not isinstance(pull, dict):
        return False
    base, head = pull.get("base"), pull.get("head")
    if not isinstance(base, dict) or not isinstance(head, dict):
        return False
    base_repo, head_repo = base.get("repo"), head.get("repo")
    if not isinstance(base_repo, dict) or not isinstance(head_repo, dict):
        return False
    base_name, head_name = base_repo.get("full_name"), head_repo.get("full_name")
    return (pull.get("state") == "closed" and bool(pull.get("merged_at"))
            and pull.get("merge_commit_sha") == sha and base.get("ref") == "main"
            and isinstance(base_name, str) and base_name.lower() == repository.lower()
            and head.get("ref") == "sync/" + tag
            and isinstance(head_name, str) and head_name.lower() == repository.lower()
            and isinstance(pull.get("number"), int))


def check(repository, sha, token, tag):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) or ".." in repository:
        raise ValueError("invalid GitHub repository")
    if not re.fullmatch(r"[a-f0-9]{40}", sha) or not token:
        raise ValueError("release SHA and API token are required")
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", tag):
        raise ValueError("release tag name is required")
    for page in range(1, 101):
        url = f"https://api.github.com/repos/{repository}/commits/{sha}/pulls?per_page=100&page={page}"
        request = Request(url, headers={"Authorization": "Bearer " + token,
                          "Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"})
        with urlopen(request, timeout=60) as response:
            pulls = json.load(response)
        if not isinstance(pulls, list):
            raise ValueError("commit pull request response must be a list")
        for pull in pulls:
            if merged_into_main(pull, sha, repository, tag):
                return pull["number"]
        if len(pulls) < 100:
            break
    raise ValueError("release commit is not the result of a merged pull request from this repository's sync/" + tag + " into main")


def main():
    sha = os.environ["GITHUB_SHA"]
    head = subprocess.check_output(("git", "rev-parse", "HEAD"), text=True).strip()
    if sha != head:
        raise ValueError("release checkout does not match the tag SHA")
    number = check(os.environ["GITHUB_REPOSITORY"], sha, os.environ["GH_TOKEN"], os.environ["GITHUB_REF_NAME"])
    print(f"release commit {sha} was merged into main by pull request #{number}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print("release-pr: " + str(error), file=sys.stderr)
        sys.exit(1)

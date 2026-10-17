#!/usr/bin/env python3
"""Remove old test resources without touching concurrent Compose runs."""

from datetime import datetime, timedelta, timezone
import json
import re
import subprocess
import sys

PREFIX = "nvmct-"
MAX_AGE = timedelta(hours=6)
KINDS = ("container", "volume", "network")
PROJECT_LABEL = "com.docker.compose.project"


class CleanupError(Exception):
    def __init__(self, kind, operation, returncode=None):
        detail = "" if returncode is None else f" (rc={returncode})"
        super().__init__(f"stale cleanup: {kind} {operation} failed{detail}")


def docker(kind, operation, *args):
    try:
        result = subprocess.run(
            ["docker", kind, operation, *args], capture_output=True,
            check=False,
        )
    except OSError as error:
        raise CleanupError(kind, operation) from error
    if result.returncode:
        raise CleanupError(kind, operation, result.returncode)
    return result.stdout


def listing(kind):
    args = ["--all", "--no-trunc"] if kind == "container" else []
    if kind == "network":
        args.append("--no-trunc")
    name_field = "Names" if kind == "container" else "Name"
    template = "{{.Name}}" if kind == "volume" else "{{.ID}}\t{{." + name_field + "}}"
    output = docker(kind, "ls", *args, "--format", template)
    resources = []
    names, identities = {}, {}
    for line in output.split(b"\n"):
        if not line:
            continue
        fields = line.split(b"\t")
        raw_name = fields[0] if kind == "volume" or len(fields) == 1 else fields[1]
        owned = raw_name.startswith(PREFIX.encode("utf-8"))
        try:
            if len(fields) != (1 if kind == "volume" else 2):
                raise ValueError("invalid fields")
            name = raw_name.decode("utf-8")
            identity = name if kind == "volume" else fields[0].decode("utf-8")
            # Only safe single Docker tokens can be passed back to the CLI.
            if any(re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]*", token) is None
                   for token in (name, identity)):
                raise ValueError("invalid identity")
        except (ValueError, UnicodeError) as error:
            if owned:
                raise CleanupError(kind, "ls") from error
            continue
        # Duplicate foreign records are harmless, but ambiguity involving a
        # candidate must stop the preflight before any resource is removed.
        if ((name in names or identity in identities)
                and (owned or names.get(name) or identities.get(identity))):
            raise CleanupError(kind, "ls")
        names[name] = owned
        identities[identity] = owned
        resources.append((name, identity))
    return resources


def inspect(kind, name, identity):
    output = docker(kind, "inspect", identity)
    try:
        rows = json.loads(output.decode("utf-8"))
        if not isinstance(rows, list) or len(rows) != 1:
            raise ValueError("invalid inspect result")
        row = rows[0]
        actual_name = row["Name"]
        if kind == "container":
            actual_name = actual_name.removeprefix("/")
        if actual_name != name or (kind != "volume" and row["Id"] != identity):
            raise ValueError("identity changed")
        created = row["CreatedAt" if kind == "volume" else "Created"]
        # Docker emits nanoseconds; older supported Python parsers only accept
        # microseconds. Truncate rather than round across the safety cutoff.
        created = re.sub(r"\.(\d+)(?=Z|[+-]\d{2}:\d{2}$)",
                         lambda match: "." + match.group(1)[:6].ljust(6, "0"), created)
        created = datetime.fromisoformat(created.replace("Z", "+00:00"))
        if created.tzinfo is None:
            raise ValueError("timestamp lacks timezone")
        labels = row["Config"].get("Labels") if kind == "container" else row.get("Labels")
        if labels is None:
            labels = {}
        if not isinstance(labels, dict):
            raise ValueError("invalid labels")
        project = labels.get(PROJECT_LABEL)
        if project is not None and (not isinstance(project, str) or not project):
            raise ValueError("invalid project")
        active = False
        if kind == "container":
            status = row["State"]["Status"]
            if not isinstance(status, str) or not status:
                raise ValueError("invalid state")
            # Unknown/transitional states are preserved along with active ones.
            active = status not in ("created", "exited", "dead")
        return created, project, active
    except (ValueError, TypeError, KeyError, AttributeError) as error:
        raise CleanupError(kind, "inspect") from error


def main():
    cutoff = datetime.now(timezone.utc) - MAX_AGE
    counts = dict.fromkeys(KINDS, 0)
    try:
        # Complete preflight before removing anything. Recognizable foreign
        # containers can still protect their active or recent Compose siblings.
        resources = {kind: listing(kind) for kind in KINDS}
        candidates = {kind: [] for kind in KINDS}
        protected_projects = set()
        for kind in KINDS:
            for name, identity in resources[kind]:
                owned = name.startswith(PREFIX)
                if not owned and kind != "container":
                    continue
                try:
                    created, project, active = inspect(kind, name, identity)
                except CleanupError:
                    if not owned:
                        continue
                    # Concurrent jobs can remove resources between ls and
                    # inspect. Only a successful exact readback proves absence.
                    remaining = listing(kind)
                    if any(name == other_name or identity == other_id
                           for other_name, other_id in remaining):
                        raise
                    continue
                if kind == "container" and project and (active or created >= cutoff):
                    protected_projects.add(project)
                if owned and created < cutoff and not active:
                    candidates[kind].append((name, identity, project))
        for kind in KINDS:
            for name, identity, project in candidates[kind]:
                if project in protected_projects:
                    continue
                # Never force container removal: a container started after
                # inspect must cause rm to fail rather than killing that run.
                docker(kind, "rm", identity)
                remaining = listing(kind)
                if any(name == other_name or identity == other_id for other_name, other_id in remaining):
                    raise CleanupError(kind, "readback")
                counts[kind] += 1
    except CleanupError as error:
        print(str(error), file=sys.stderr)
        return 1
    print("stale cleanup: " + " ".join(f"{kind}s={counts[kind]}" for kind in KINDS))
    return 0


if __name__ == "__main__":
    sys.exit(main())

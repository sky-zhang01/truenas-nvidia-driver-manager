# truenas-nvidia-driver-manager

A web UI for choosing, building, replacing, and restoring the NVIDIA driver on a TrueNAS SCALE host.

[简体中文](README.zh-CN.md)

Choose a version from NVIDIA's download catalog, upload a driver installer with its four
`nvidia-container-toolkit` packages, or switch to a saved local driver. The manager builds a
`systemd-sysext` image when needed, saves the stock driver when requested, replaces the image at
its permanent path, and checks the loaded driver. If the post-replacement driver check fails, it attempts recovery
with the saved stock driver when one is usable. Local drivers and task logs remain available in
the UI. Every replacement requires a person's confirmation; there are no scheduled updates.

## Compatibility

The only tested platform is **TrueNAS SCALE 25.10.7 on amd64 (Linux x86_64)**. Other TrueNAS
versions and architectures are untested. A listed driver version is not a guarantee that it
works with every GPU, kernel, or workload. Read the plan's GPU checks and warnings before confirming.

## Security model

- The App is a **privileged container** with `pid: host`. It reads the host through
  `/proc/1/root`, enters host namespaces to run commands, and starts each confirmed replacement
  in a host systemd unit. It can change host driver files and unload/reload kernel modules.
  Treat access to the manager, its image, and its data directory as host administrator access.
- The server defaults to **plain HTTP on `0.0.0.0:8199` inside the container**. The default
  bridge mapping, `8199:8199`, publishes it on all NAS interfaces. macvlan and ipvlan are also
  available. Restrict access to a trusted management LAN; do not expose the port to the internet.
- One administrator account starts from `NVM_INITIAL_ADMIN_PASSWORD` (username defaults to
  `admin`). First sign-in requires a different password before changing actions are allowed.
  Failed password and SSH-key sign-ins share rate limiting.
- Authenticated API requests require both a signed, 12-hour `HttpOnly`, `SameSite=Strict`
  session cookie and the session's `X-NVM-Token`. Only non-`GET`/`HEAD` requests undergo Origin
  checks. Treat both values as secrets. The application does **not** add `Secure` to its cookie;
  see below.
- Agents can sign a one-time challenge with a dedicated SSH key registered by the administrator.
  Only the public key and signature reach the manager. This grants the same administrator
  session, without opening an SSH service or creating a restricted agent role. Removing a key
  invalidates existing sessions. An agent must present the exact plan and obtain human
  confirmation for every replacement, retry, or return to stock; the API does not prove that
  a human supplied that confirmation. See the [agent guide](docs/agent.en.md).

## Known limitations

- Driver replacement can interrupt GPU workloads. The manager reports GPU users and refuses
  unsafe plans; it does not stop Apps for you. A saved original is required for automatic
  recovery, and recovery can fail. Back up the data directory and arrange host console access.
- Builds run in disposable containers with networking disabled, all capabilities dropped, and
  `no-new-privileges`, but **no CPU, memory, or process-count limits are set**. A build can
  compete with NAS services. Downloading and building share a time limit of 30 minutes by default.
- TLS is not built in. An HTTPS reverse proxy must preserve the request host/origin and protect
  the backend connection. The application does not infer a `Secure` cookie from forwarded
  headers; whether the browser receives that attribute depends on the deployment's cookie
  rewriting. TLS termination alone does not add it.
- Uploaded installers run during the build and produce code loaded into the host kernel.
  Use only trusted files and compare their SHA-256 values with a trusted source. A checksum
  identifies bytes; it does not establish who supplied them.
- Online builds depend on NVIDIA downloads, Docker Hub, and Debian snapshot packages. For the
  official `470.256.02` driver on `6.12` kernels, the host also downloads a community patch archive
  from `deb.debian.org`; allow this additional source through the outbound firewall. Failed
  downloads or checksum checks stop preparation; inspect the task log and retry after fixing
  the cause. Offline builds require the pinned build image and package set in advance.
- The community-patched driver does not support user I/O memory mappings and cannot load with
  IBT enabled; display hotplug behavior may differ. Kepler loading, display, and CUDA workloads
  have not been verified. Follow the warnings for the selected source.

## Install and upgrade

Use **Apps → Discover → Install via YAML** in TrueNAS with [`deploy/compose.yaml`](deploy/compose.yaml).
Set a dedicated data directory and a strong initial password. Released images use:

```text
ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest
ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:0.2.0
```

Use a version tag to select a particular release. Upgrade by editing the App's image tag, or
use TrueNAS **Update** when tracking `latest` and an update is offered. Keep the data directory
and back it up before upgrading. Reverting the App image and restoring the host driver are
separate operations; see the [deployment guide](docs/deploy.en.md) and
[operations guide](docs/operations.en.md).

The runtime image uses `scratch` and carries no shell or build tools. Use the task page and
host-side container logs for diagnostics; the App's **Shell** button has no shell to start.

## Documentation and contributions

- [Deployment](docs/deploy.en.md) — installation, networking, upgrades, and offline preparation.
- [Operations](docs/operations.en.md) — stock-driver restore, password reset, backup, and troubleshooting.
- [Agent operation](docs/agent.en.md) and [agent skill](skills/nvidia-driver-replacement/SKILL.md).
- [Security reporting](SECURITY.md), [contributing and local development](CONTRIBUTING.md), and
  [changelog](CHANGELOG.md).

For agents such as Claude Code or Codex, install the skill and its English guide from the
repository root. For Claude Code, use the following destination; for Codex, set `DEST` to
`"$HOME/.agents/skills/nvidia-driver-replacement"`. Repeat after upgrading the skill.

```bash
DEST="$HOME/.claude/skills/nvidia-driver-replacement"
mkdir -p "$DEST/docs"
cp skills/nvidia-driver-replacement/SKILL.md "$DEST/SKILL.md"
cp docs/agent.en.md "$DEST/docs/agent.en.md"
```

## License and trademarks

Project source code is MIT-licensed; see [`LICENSE`](LICENSE) and
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) for third-party notices.

This project is **not affiliated with or endorsed by iXsystems or NVIDIA**. TrueNAS is a
trademark of iXsystems, Inc.; NVIDIA is a trademark of NVIDIA Corporation. Brand assets and
embedded wordmarks are not MIT-licensed; rights remain with their respective owners.

The project image does not bundle the NVIDIA driver installer. For an official-source update,
the user's host downloads it from NVIDIA; uploaded installers are supplied by the user.
NVIDIA driver software remains subject to NVIDIA's applicable license terms, separate from
this project's MIT license.

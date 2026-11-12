# Contributing

[简体中文](CONTRIBUTING.zh-CN.md)

## Issues and pull requests

Bug reports, documentation corrections, and focused pull requests are welcome. Include the
application version, TrueNAS version, expected and observed behavior, and reproducible steps.
For driver problems, include the selected source, GPU model, kernel version, task status, and
redacted check results. Never attach credentials, session tokens, private keys, or raw host data
without checking it. Report vulnerabilities through [SECURITY.md](SECURITY.md).

Public `main` is published by the maintainer as an export from the upstream development
repository. Accepted contributions are integrated upstream by the maintainer and appear in the
next public export. **Public PRs are not merged directly** into public `main`; their closure
does not by itself mean the change was rejected. Discuss substantial changes in an issue first,
keep the diff focused, and update both English and Chinese documentation when changing behavior.

## Local development

Use Rust **1.87+**, Node.js **24**, npm, Git, Bash, and Python 3. A Linux development environment
provides the fullest host-tool coverage; some tests exercise `dpkg-deb` when available. Backend
tests use a fake TrueNAS host rather than managing a real NAS, but also use local temporary
files and processes. Do not run development servers or tests on a production NAS.

From the repository root:

```bash
cargo test --locked
npm --prefix web ci
npm --prefix web run build
npm --prefix web run test:unit
```

These commands run backend tests, install frontend development dependencies, build static web
assets into `web/dist`, and run frontend unit tests. The unit tests also bind the API and warning
text in `docs/agent.*` to the source; update the guides together with relevant behavior.

For end-to-end tests, first run the frontend build above, then:

```bash
cargo build --locked --release --example fake_host_server
(cd web && npx playwright install --with-deps chromium)
export NVM_E2E_DATA_DIR="$(mktemp -d)"
CI=1 npm --prefix web run e2e
```

Playwright starts `fake_host_server` automatically on port **18109** and serves `web/dist`.
Prebuilding the release example avoids exceeding Playwright's 120-second webServer startup wait
while compiling the release binary. `CI=1` requires a fresh server instead of reusing an existing
one. The server uses a mutable fake host, so
browser tests run with one worker. Although Playwright connects through localhost, this test
server binds **`0.0.0.0`** and exposes test-only controls without production authentication;
run it only in an isolated development environment. `NVM_E2E_DATA_DIR` must exist and holds
temporary test files.

To build the application image locally with Docker:

```bash
docker build --platform linux/amd64 -t truenas-nvidia-driver-manager:local .
```

To run the container tests against that image:

```bash
NVM_IMAGE=truenas-nvidia-driver-manager:local NVM_CURL_IMAGE=<image with curl> bash ci/container/test.sh
```

The script starts the image from `deploy/compose.yaml` on the current Docker daemon. The
startup check treats PID 1 of the daemon's PID namespace as the TrueNAS host; with a rootful
daemon that is the machine itself, checked as it is. A rootless daemon, whether plain rootless
Docker or rootless Docker-in-Docker, must run RootlessKit in its own PID, UTS and IPC namespaces
(`DOCKERD_ROOTLESS_ROOTLESSKIT_FLAGS="--pidns --utsns --ipcns"`); before the tests the script
adds `/usr/bin/docker` and `/var/run/docker.sock` inside that daemon's namespace, and the curl
image then also needs `nsenter`.

Apart from the VM test below, do not deploy this privileged App just to run tests. For Compose changes, run
`bash ci/check-compose-test.sh` (Docker Compose enables all rendered-config controls).
Run `bash ci/guards.sh` for tracked-file coordinate checks; inspect new files yourself too.
These checks do not replace reviewing logs and fixtures for sensitive data.

## GPU unlock test in a TrueNAS VM

`tests/gpu_unlock_vm.rs` deploys the application as an app in a TrueNAS test VM and runs
updates with the optional GPU unlock through the program's own tasks: the build and test load,
the load order kmod uses, a reboot, GPU isolation, and the switch back to a saved original. The
checks are the rows of `tests/fixtures/gpu-unlock-vm.tsv`; the case column says whether a row
runs with GPU isolation configured, without it, both, or once for the VM. The VM has no NVIDIA
GPU. While a plan is made, the test mounts a copy of `/sys/bus/pci/devices` with one extra
entry, an RTX 3090 at `0000:00:1f.7`, so the program plans the unlock for an address where the
kernel has no device.

The test needs a disposable TrueNAS 25.10 or 27 VM with a pool, Apps enabled, Internet access
and an SSH account with passwordless sudo. It deletes and recreates its own app and data
directory, replaces the VM's NVIDIA driver, reboots the VM and turns its NVIDIA switch off and
on. Never run it against a NAS in use. Build the image, load it in the VM, then run the test
from the repository root:

```bash
docker build --platform linux/amd64 -t truenas-nvidia-driver-manager:local .
docker save truenas-nvidia-driver-manager:local | ssh <vm> sudo docker load
NVM_VM_SSH='ssh -o BatchMode=yes -o ServerAliveInterval=10 <vm>' \
  NVM_VM_IMAGE=truenas-nvidia-driver-manager:local \
  NVM_VM_DATA=/mnt/<pool>/nvm-vm-test NVM_VM_OUT="$HOME/nvm-vm-evidence" \
  cargo test --locked --test gpu_unlock_vm -- --ignored --nocapture
```

`NVM_VM_DATA` must be a plain directory, not a dataset, that does not exist yet or that this test
created earlier. Run the test once per TrueNAS version. One run, with three builds, nine tasks
and four reboots, took 15 to 20 minutes on the test VMs. `NVM_VM_OUT` receives every task
record, the readings behind each check and `results.tsv`.

## Writing the changelog

Write `CHANGELOG.md` and `CHANGELOG.zh-CN.md` for the person who installs the app.

- One bullet is one thing the user can see or must do. Put what the user must do first.
- Use no abbreviations, hashes, check counts, stage names, retry intervals or API field names. Say plainly what has not been verified.
- Write Chinese as Chinese, not as a word-for-word translation of the English.
- Keep developer-only changes to one last bullet that points to `docs/agent.*`. The English section of a release is also its GitHub Release text, so it must read well on its own.
- Sort the facts into "the user sees", "the user must do" and "developers only" first, then cut, and reword last.

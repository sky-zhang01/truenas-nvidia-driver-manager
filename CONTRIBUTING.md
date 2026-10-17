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

Use Rust **1.85+**, Node.js **24**, npm, Git, Bash, and Python 3. A Linux development environment
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

Do not deploy this privileged App just to run tests. For Compose changes, run
`bash ci/check-compose-test.sh` (Docker Compose enables all rendered-config controls).
Run `bash ci/guards.sh` for tracked-file coordinate checks; inspect new files yourself too.
These checks do not replace reviewing logs and fixtures for sensitive data.

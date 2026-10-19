# Changelog

[简体中文](CHANGELOG.zh-CN.md)

## 0.2.1

- Create the administrator with a one-time setup code from the App logs and a password chosen
  on the setup page. Deployment YAML and container environment no longer contain an initial
  password. Codes expire after 24 hours or a restart; expiry renews on the next setup request.
- `NVM_INITIAL_ADMIN_PASSWORD` is no longer a recognized setting; remove it from App configuration.
  Returning to 0.2.0 after creating an account through setup is unsupported.
- Reset access by deleting the credentials file, reloading the page, and reading a new code
  from the App logs. SSH keys can be registered immediately after setup.
- Make task status and stage easier to scan, explain unfinished and unavailable readings, and
  show the task details heading only when details exist.
- Show concise local-driver column names and sort arrows with accessible sort controls; keep
  values readable, fit action buttons on desktop, and allow horizontal scrolling on narrow screens.
- Rewrite the README as a step-by-step guide for first-time users, with screenshots of each step;
  the security model and known limitations move to `docs/security.en.md`.

## 0.2.0 — First public release

- Manage NVIDIA drivers through an English or Chinese web UI on the tested TrueNAS SCALE
  25.10.7 / amd64 platform, with light and dark themes.
- Choose official driver downloads, upload an installer with matching toolkit packages, or
  switch between saved local drivers. Review GPU and kernel checks before confirming each plan.
- Build driver system extensions in disposable offline build containers using a pinned build
  environment; support offline preparation of images and packages.
- Save the stock driver, replace the permanent driver image atomically, and verify loaded
  versions. Attempt recovery to a usable saved stock driver if the replacement check fails.
- Inspect task progress, logs, interrupted-task guidance, and local driver storage; release
  recovery inputs only after safety checks.
- Sign in with an administrator password or a registered SSH key. Agent operation uses the
  existing authenticated API and requires explicit confirmation of every replacement plan.
- Install the App from GHCR with bridge, macvlan, or ipvlan networking. Public documentation
  covers deployment, operations, security reporting, and contributions.

# Changelog

[简体中文](CHANGELOG.zh-CN.md)

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

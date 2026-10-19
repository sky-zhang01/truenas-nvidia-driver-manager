# Security policy

[简体中文](SECURITY.zh-CN.md)

## Supported versions and environment

Security fixes target the latest published patch in the **0.2.x** series, starting with the
first public release, **0.2.0**. Earlier development versions are unsupported. The tested
platform is **TrueNAS SCALE 25.10.7 on amd64**; other TrueNAS versions and architectures are
untested. Use the latest supported release and read the [security model](docs/security.en.md).

The App runs privileged with host PID access and can replace host kernel driver code. It is
intended for a trusted management LAN. It serves HTTP, and its session cookie does not include
`Secure`; HTTPS and any required cookie rewriting are deployment responsibilities.

## Report privately

Use this repository's **Security → Advisories → Report a vulnerability**:
[private vulnerability report](https://github.com/sky-zhang01/truenas-nvidia-driver-manager/security/advisories/new).
Please do not open a public issue or PR with an unpatched vulnerability or exploit details.

Include the application version or image digest, TrueNAS version, affected component, impact,
prerequisites, and reproducible steps or a small proof of concept. Redact credentials, cookies,
`X-NVM-Token`, private keys, host addresses, and unrelated personal data from logs and attachments.
Do not test against systems you do not own or have permission to assess.

The maintainer aims to acknowledge reports within **7 days** and provide an initial assessment
or progress update within **30 days**. This is a volunteer project; these are targets, not a
response or fix guarantee. Coordinate disclosure in the private advisory while a fix is assessed.

## Scope

Reports about this project's code, authentication, API, upload handling, build execution,
and distributed container image are in scope. TrueNAS and NVIDIA software vulnerabilities
belong to their respective vendors; this project cannot issue fixes for those products. If the
boundary is unclear, describe the interaction in a private report. Ordinary usage questions and
non-security defects belong in public issues after sensitive data is removed.

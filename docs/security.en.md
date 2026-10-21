# Security guide

[简体中文](security.zh-CN.md)

## How it works

The manager runs in one privileged container. It runs the web page, reads the host through `/proc/1/root`
and starts each confirmed update as a transient `systemd` unit on the host. That unit runs the
same program in its `task run` mode and, when a build is needed, starts a disposable build
container. The driver is a `systemd-sysext` image: a packed driver that TrueNAS merges into
`/usr`. The app replaces the image file at its permanent path
(`/usr/share/truenas/sysext-extensions/nvidia.raw`), unmerges and merges it once, and checks the
loaded driver version. Task completion is determined by the task state file, not systemd.
The runtime image is empty-base (`scratch`): it has no shell or build tools, so the app's
Shell button has nothing to start.

## Security model

- `pid: host` and privileged execution allow host namespace access, driver-file changes and
  kernel-module unloading/reloading. Treat access to the manager, its image and data directory
  as host administrator access.
- The server defaults to plain HTTP on `0.0.0.0:8199` inside the container. The default
  bridge mapping, `8199:8199`, publishes it on all host interfaces. macvlan and ipvlan are also
  available. Restrict access to a trusted management LAN; do not expose the port to the internet.
- One administrator account. While no account exists, the app writes a one-time setup code to
  its own log. Whoever opens the manager page and enters the code chooses the administrator
  password and is signed in; the username defaults to `admin`. The YAML holds no password. The
  code grants first-account creation to its holder; finish setup promptly and keep app logs
  private. It appears only in app logs, works once, expires after 24 hours, and a restart invalidates
  it and makes a new one. A live code is not replaced by a wrong guess. Wrong setup codes and
  failed password and SSH-key sign-ins share one rate limit. The code and password travel over
  plain HTTP unless you add a TLS front end. To reset a lost password, delete the credentials
  file and set up again; see the [operations guide](operations.en.md).
- Authenticated API requests require both a signed, 12-hour `HttpOnly`, `SameSite=Strict`
  session cookie and the session's `X-NVM-Token`. Only non-`GET`/`HEAD` requests undergo Origin
  checks. Treat both values as secrets. The application does not add `Secure` to its cookie;
  see below.
- Agents can sign a one-time challenge with a dedicated SSH key registered by the administrator.
  Only the public key and signature reach the manager. This grants the same administrator
  session, without opening an SSH service or creating a restricted agent role. Removing a key
  invalidates existing sessions. An agent must present the exact plan and obtain human
  confirmation for every replacement, retry, or return to the original driver; the API does not prove that
  a human supplied that confirmation. See the [agent guide](agent.en.md).

## Known limitations

- Driver replacement can interrupt GPU workloads. The manager reports GPU users and refuses
  unsafe plans; it does not stop apps for you. A saved original driver is required for automatic
  recovery, and recovery can fail. Back up the data directory and arrange host console access.
- Builds run in disposable containers with networking disabled, all capabilities dropped, and
  `no-new-privileges`, but no CPU, memory, or process-count limits are set. A build can
  compete with host services. Downloading and building share a time limit of 30 minutes by default.
- TLS is not built in. An HTTPS reverse proxy must preserve the request host/origin and protect
  the backend connection. The application does not infer a `Secure` cookie from forwarded
  headers; whether the browser receives that attribute depends on the deployment's cookie
  rewriting. TLS termination alone does not add it.
- Uploaded installers run during the build and produce code loaded into the host kernel.
  Only upload trusted files and compare their SHA-256 values with a trusted source. A checksum
  identifies bytes; it does not establish who supplied them.
- Online builds depend on NVIDIA downloads, Docker Hub, and Debian snapshot packages. For the
  official `470.256.02` driver on `6.12` kernels, the host also downloads a community patch archive
  from `deb.debian.org`; allow this additional source through the outbound firewall. Failed
  downloads or checksum checks stop preparation; inspect the task log and retry after fixing
  the cause. Offline builds require the pinned build image and package set in advance.
- The community-patched driver does not support user I/O memory mappings and cannot load with
  IBT enabled; display hotplug behavior may differ. Kepler loading, display, and CUDA workloads
  have not been verified. Follow the warnings for the selected source.

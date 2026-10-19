# Deployment guide

[简体中文](deploy.zh-CN.md)

The tested platform is **TrueNAS SCALE 25.10.7 / amd64**. Other versions and architectures are
untested. This App controls the host driver from a privileged container with `pid: host`.
Read the [security guide](security.en.md) before installing. Arrange a maintenance
window and host console access; replacing drivers can interrupt GPU workloads.

## 1. Install the App

1. In TrueNAS, open **Apps → Discover Apps**, then the ⋮ **More Options** next to **Custom App** → **Install via YAML** and paste
   [`deploy/compose.yaml`](../deploy/compose.yaml).
2. Keep `image: ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest`, or choose a published
   version for this release. Public GHCR images can be pulled without registry credentials.
3. Replace `/mnt/<pool>/<dataset>` with a dedicated host directory or dataset. Keep the container
   side `/var/lib/nvm`. Do not put child dataset mounts below this directory: replacement uses
   renames which must stay on the same filesystem. The dataset must allow execution (`exec=on`).
   Paths must not contain `:`, `,`, `"`, or control characters.
4. Enable exactly one network section and save the App. The default is bridge.
   The YAML contains no password; you choose it on the manager page after reading a setup code.

Optional settings are `NVM_INITIAL_ADMIN_USERNAME=admin`, `NVM_PORT=8199`, and
`NVM_BUILD_TIMEOUT_MINUTES=30`. If you change `NVM_PORT`, change both port mapping numbers to
match (for example, `18199:18199`). TrueNAS removes comments when saving; keep these notes.

## 2. Connect and sign in

- Default bridge: open `http://<nas-host>:8199` from a trusted management LAN. The server listens
  on `0.0.0.0` in its container; the template publishes the port on all NAS interfaces. Restrict
  reachability with your network controls. Do not forward it to the internet.
- macvlan or ipvlan: remove the bridge port mapping and enable both the service and end-of-file
  blocks for exactly one mode. Fill in the interface, subnet, gateway, and container address.
  Open `http://<container-static-ip>:8199` from another LAN machine. macvlan can have its own
  `mac_address`; ipvlan shares the host interface's MAC.
- The service provides HTTP, not TLS. If using an HTTPS reverse proxy, preserve the host/origin,
  protect the backend connection, and configure cookie rewriting if you require `Secure`.
  The application never sets that flag itself, even behind a TLS proxy.

1. In TrueNAS, open **Apps → Installed → this App**. On the **Workloads** card, click
   the container row's document icon, **View Logs**.
2. Find the newest JSON log message containing `SETUP CODE:` and select the whole code from `msg`, such as
   `7KQ2-M9XD-4TWB-HE6R`, including all four groups.
3. Open the manager page. Enter the setup code, choose a password of at least 8 characters,
   repeat it, and click **Create administrator**. The administrator is `admin` unless you set
   `NVM_INITIAL_ADMIN_USERNAME`. You are signed in immediately.

Each code works once and expires after 24 hours or when the App restarts. Expiry is checked on
use: reopening the manager page or submitting setup after expiry writes a new code to the logs.
A wrong code does not replace a live code. If the message is missing, restart the App, copy the
code from `msg` in the newest log message containing `SETUP CODE:`, and reload the manager page.
The code is held only in memory; App log readers and log collectors can see the live code. Finish setup promptly and keep App logs private.
The code and password travel over HTTP unless you provide a TLS front end.

Later visits use your administrator password or a registered SSH key. Keys can be registered
right after setup; see the [agent guide](agent.en.md). See [account reset](operations.en.md)
if you lose access.

## 3. Replace a driver or return to stock

Choose an official driver version, upload trusted files, or select a usable local driver.
Read the generated plan and all warnings, choose whether to save the original, and confirm.
The manager reports GPU users; stop affected Apps yourself only when you are ready to do so.

Prefer saving the stock driver before the first replacement. Skipping it leaves no automatic
recovery copy. Automatic restoration is attempted when the post-replacement driver check fails
and a usable original exists; earlier failures do not all trigger restoration. Recovery can
also fail. Inspect the final task and actual driver readings rather than assuming success.

To return to stock later, choose the usable **original** row in the local driver table and
confirm its plan. This uses the saved file without downloading or building. Without a usable
original for the current host, the manager cannot offer that restoration. See
[operations](operations.en.md) for task failures and interrupted updates.

## 4. Upgrade or revert the App image

Back up the data directory, record the current image tag, and wait for any task to finish before
changing the App. Set `image:` to the desired published GHCR version and save the App. When
tracking `latest`, use **Update** if TrueNAS offers it, or edit the tag to select a release.
Keep the same data mount: credentials, task records, and local drivers live there.

To revert the application, set `image:` to the previously recorded version tag and redeploy.
This changes the manager application; it does not restore the NVIDIA driver. No general
backward-readability guarantee is made for data written by a newer application; keep the
pre-upgrade backup. Restoring a backup discards newer state, so do it only when no task is
running and after preserving any recovery material you still need.

## 5. Offline installation and builds

On an online amd64 computer, pull the public application image and save it:

```bash
docker pull --platform linux/amd64 ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest
docker save -o truenas-nvidia-driver-manager.tar ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest
```

Transfer that archive to the NAS and load it in the **host** shell:

```bash
docker load -i truenas-nvidia-driver-manager.tar
```

Use the loaded repository tag (or image ID) in the App YAML. An archive loaded by `docker load`
need not retain a registry digest reference; do not rely on `@sha256` for that loaded App image.
The runtime image uses `scratch`: it has no shell, distribution userland, or build tools.
The App's **Shell** button cannot start a shell. Its size differs from the much larger driver
`nvidia.raw` image; use your downloaded artifact's actual size for storage planning.

Switching to a saved local driver or restoring stock requires no build environment. An offline
build from an uploaded installer requires all of the following:

1. The pinned amd64 Debian build image. On an online computer, prepare it with:

   ```bash
   set -e
   BUILD_IMAGE='debian@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587'
   docker pull --platform linux/amd64 "$BUILD_IMAGE"
   BUILD_IMAGE_ID=$(docker image inspect -f '{{.Id}}' "$BUILD_IMAGE")
   case "$BUILD_IMAGE_ID" in
     sha256:d4c45f66efc59f89b0597d343a9bd72808493c0eacecde5285961743252cee23|sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587) ;;
     *) printf 'Unexpected build image ID: %s\n' "$BUILD_IMAGE_ID" >&2; exit 1 ;;
   esac
   docker save -o debian-bookworm-slim-amd64.tar "$BUILD_IMAGE_ID"
   ```

   The check accepts the two content IDs used by classic and containerd image stores, and saves
   the actual local ID. Transfer the archive to the NAS and run:

   ```bash
   docker load -i debian-bookworm-slim-amd64.tar
   docker image inspect -f '{{.Id}}' sha256:d4c45f66efc59f89b0597d343a9bd72808493c0eacecde5285961743252cee23 ||
     docker image inspect -f '{{.Id}}' sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
   ```

   The first lookup may report a missing image; the second then checks the other ID. Stop if
   both fail. The printed ID must be `sha256:d4c45f66efc59f89b0597d343a9bd72808493c0eacecde5285961743252cee23` or
   `sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587`; stores can report different IDs for the same pinned content.
   The manager checks the reference and both known IDs before trying a pull.
2. The fixed Debian package set from this release. At the repository root on an online computer:

   ```bash
   tools/build-kit.sh fetch ./build-kit
   ```

   Transfer the resulting `.deb` files to `<data-directory>/manager/build-kit/` on the NAS.
   The tool and manager verify file names, byte counts, and SHA-256 against the bundled manifest.
   The manager also reuses verified files in `manager/build-kit-cache/`. Missing or mismatched
   files cause a download attempt; prepare all packages before taking the host offline.
3. One trusted `NVIDIA-Linux-x86_64-<version>.run` installer and four toolkit `.deb` packages:
   `libnvidia-container1`, `libnvidia-container-tools`, `nvidia-container-toolkit`, and
   `nvidia-container-toolkit-base`. Match their package versions and use the official
   `-no-compat32` installer or a trusted rebuild as appropriate. Before uploading, calculate
   checksums on the computer holding the files and compare them with your trusted source:

   ```bash
   sha256sum NVIDIA-Linux-x86_64-*.run *.deb
   ```

   Upload the five files on **Update driver**. A matching checksum alone does not prove trust:
   the installer executes during the build and its output is loaded into the host kernel.
   Kernel headers, module files, and kernel configuration come from the NAS itself.

For online preparation, the host downloads the driver from NVIDIA, pulls the pinned build image
from Docker Hub when missing, and downloads missing packages from snapshot.debian.org. Toolkit
packages come from NVIDIA's package repository. For the official `470.256.02` driver on `6.12`
kernels, the host also downloads a community patch archive from `deb.debian.org`; allow this
additional source through the outbound firewall. Only build-kit package fetches follow HTTPS
redirects (at most five); driver, toolkit, and community-patch fetches require a direct successful
response. Download or verification failure stops that stage and is reported in the task log.
Fix connectivity or supply the verified offline inputs, then
create and confirm a new plan. The disposable build container itself has no network and has no
CPU, memory, or process-count limits.

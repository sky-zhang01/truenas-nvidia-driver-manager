# Deployment guide

[简体中文](deploy.zh-CN.md)

TrueNAS SCALE 25.10.7 and TrueNAS 27.0.0-RC.1 / amd64 are supported and tested; other platforms are untested. The privileged container uses `pid: host` to control the host driver. Read the [security guide](security.en.md), arrange a maintenance window and retain host console access: replacement can interrupt GPU workloads.

Enable the NVIDIA driver in TrueNAS before using the manager:

- 25.10.7: on Apps → Installed, click Configuration → Settings, tick Install NVIDIA Drivers and click Save.
- 27.0.0-RC.1: open System → Advanced Settings, click Configure on the NVIDIA Drivers card, tick Enable NVIDIA GPU Support and click Save.

## 1. Install the app

1. In TrueNAS, open Apps → Discover Apps, then the ⋮ More Options next to Custom App → "Install via YAML" and paste
   [`deploy/compose.yaml`](../deploy/compose.yaml).
2. Keep `image: ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest`, or choose a published
   version for this release. Public GHCR images can be pulled without registry credentials.
3. Replace `/mnt/<pool>/<dataset>` with a dedicated host directory or dataset. Keep the container
   side `/var/lib/nvm`. Do not put child dataset mounts below this directory: replacement uses
   renames which must stay on the same filesystem. The dataset must allow execution (`exec=on`).
   Paths must not contain `:`, `,`, `"`, or control characters.
4. Enable exactly one network section and save the app. The default is bridge.
   The YAML contains no password; you choose it on the manager page after reading a setup code.

Optional settings are `NVM_INITIAL_ADMIN_USERNAME=admin`, `NVM_PORT=8199`, and
`NVM_BUILD_TIMEOUT_MINUTES=30`. If you change `NVM_PORT`, change both port mapping numbers to
match (for example, `18199:18199`). TrueNAS removes comments when saving; keep these notes.

## 2. Connect and sign in

- Default bridge: open `http://<nas-host>:8199` from a trusted management LAN. The server listens
  on `0.0.0.0` in its container; the template publishes the port on all host interfaces. Restrict
  reachability with your network controls. Do not forward it to the internet.
- macvlan or ipvlan: remove the bridge port mapping and enable both the service and end-of-file
  blocks for exactly one mode. Fill in the interface, subnet, gateway, and container address.
  Open `http://<container-static-ip>:8199` from another LAN machine. macvlan can have its own
  `mac_address`; ipvlan shares the host interface's MAC.
- The service provides HTTP, not TLS. If using an HTTPS reverse proxy, preserve the host/origin,
  protect the backend connection, and configure cookie rewriting if you require `Secure`.
  The application never sets that flag itself, even behind a TLS proxy.

1. In TrueNAS, open Apps → Installed → this app. On the Workloads card, click
   the container row's document icon, View Logs.
2. Find the newest JSON log message containing `SETUP CODE:` and select the complete setup code (four groups of characters) from `msg`, such as
   `7KQ2-M9XD-4TWB-HE6R`.
3. Open the manager page. Enter the setup code, choose a password of at least 8 characters,
   repeat it, and click Create administrator. The administrator is `admin` unless you set
   `NVM_INITIAL_ADMIN_USERNAME`. You are signed in immediately.

Each code works once and expires after 24 hours or when the app restarts. Expiry is checked on
use: reopening the manager page or submitting setup after expiry writes a new code to the logs.
A wrong code does not replace a live code. If the message is missing, restart the app, copy the
code from `msg` in the newest log message containing `SETUP CODE:`, and reload the manager page.
The code is held in memory only; app log readers and log collectors can see the live code. Finish setup promptly and keep app logs private.
The code and password travel over HTTP unless you provide a TLS front end.

Later visits use your administrator password or a registered SSH key. Keys can be registered
right after setup; see the [agent guide](agent.en.md). See [account reset](operations.en.md)
if you lose access.

## 3. Replace a driver or return to the original driver

Choose an official driver version, upload trusted files, or select a usable local driver.
Read the generated plan and all warnings, choose whether to save the original, and confirm.
The manager reports GPU users; stop affected apps yourself only when you are ready to do so.

Prefer saving the original driver before the first replacement. Skipping it leaves no automatic
recovery copy. Automatic restoration is attempted when the post-replacement driver check fails
and a usable original exists; earlier failures do not all trigger restoration. Recovery can
also fail. Inspect the final task and actual driver readings before reporting success.

To return to the original driver later, choose the usable original row in the local driver table and
confirm its plan. This uses the saved file without downloading or building. Without a usable
original for the current host, the manager cannot offer that restoration. See
[operations](operations.en.md) for task failures and interrupted updates.

## 4. Upgrade or revert the app image

Back up the data directory, record the current image tag, and wait for any task to finish before
changing the app. Set `image:` to the desired published GHCR version and save the app. When
tracking `latest`, use Update if TrueNAS offers it, or edit the tag to select a release.
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

Transfer that archive to the host and load it in the host shell:

```bash
docker load -i truenas-nvidia-driver-manager.tar
```

Use the loaded repository tag (or image ID) in the app YAML. An archive loaded by `docker load`
need not retain a registry digest reference; do not rely on `@sha256` for that loaded app image.
The runtime image uses `scratch`: it has no shell, distribution userland, or build tools.
The app's Shell button cannot start a shell. Its size differs from the much larger driver
`nvidia.raw` image; use your downloaded artifact's actual size for storage planning.

Switching to a saved local driver or restoring the original driver requires no build environment. An offline
build from an uploaded installer requires all of the following:

1. One pinned amd64 Debian 13 (`trixie-slim`) build image for both kernel compiler majors.
   Its fixed package set includes gcc-14 14.2.0-19 and gcc-12 12.4.0-5; the runner selects
   by the kernel's compiler major before downloading any build inputs. This accepts the
   25.10 kernel's gcc 12.2.0 requirement with gcc 12.4.0. On an online computer, prepare it with:

   ```bash
   set -e
   BUILD_IMAGE='debian@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f'
   docker pull --platform linux/amd64 "$BUILD_IMAGE"
   BUILD_IMAGE_ID=$(docker image inspect -f '{{.Id}}' "$BUILD_IMAGE")
   case "$BUILD_IMAGE_ID" in
     sha256:567e3c17b188e21cd42aa5e1c55e16d8270f880ed8ea321dbd40f993467efc4c|sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f) ;;
     *) printf 'Unexpected build image ID: %s\n' "$BUILD_IMAGE_ID" >&2; exit 1 ;;
   esac
   docker save -o debian-trixie-slim-amd64.tar "$BUILD_IMAGE_ID"
   ```

   The check accepts the two content IDs used by classic and containerd image stores, and saves
   the actual local ID. Transfer the archive to the host and run:

   ```bash
   docker load -i debian-trixie-slim-amd64.tar
   docker image inspect -f '{{.Id}}' sha256:567e3c17b188e21cd42aa5e1c55e16d8270f880ed8ea321dbd40f993467efc4c ||
     docker image inspect -f '{{.Id}}' sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f
   ```

   The first lookup may report a missing image; the second then checks the other ID. Stop if
   both fail. The printed ID must be `sha256:567e3c17b188e21cd42aa5e1c55e16d8270f880ed8ea321dbd40f993467efc4c` or
   `sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f`; stores can report different IDs for the same pinned content.
   The manager checks the reference and both known IDs before trying a pull.
2. The fixed Debian package set from this release. At the repository root on an online computer:

   ```bash
   tools/build-kit.sh fetch ./build-kit
   ```

   Transfer the resulting `.deb` files to `<data-directory>/manager/build-kit/` on the host.
   The tool and manager verify file names, byte counts, and SHA-256 against the bundled manifest.
   The manager also reuses verified files in `manager/build-kit-cache/`. Missing or mismatched
   files cause a download attempt; prepare all packages before taking the host offline.
3. One trusted `NVIDIA-Linux-x86_64-<version>-no-compat32.run` installer and four toolkit `.deb` packages:
   `libnvidia-container1`, `libnvidia-container-tools`, `nvidia-container-toolkit`, and
   `nvidia-container-toolkit-base`. All four must be amd64 packages of the same version; skip `-dev`, `-dbg` and similar packages.
   The `-no-compat32` installer leaves out 32-bit libraries TrueNAS does not need; the full
   installer without that suffix also works but is larger. Each row in the manual-upload
   window provides a download link that opens in a new tab. The toolkit version is the newest
   version with all four packages in NVIDIA's official Packages index, with `1.20.1-1` used
   if the index cannot be read. Driver 615 or later needs toolkit 1.20.1 or newer. Before uploading, calculate
   checksums on the computer holding the files and compare them with your trusted source:

   ```bash
   sha256sum NVIDIA-Linux-x86_64-*.run *.deb
   ```

   Debian community compatibility patches are applied only when building official 470.256.02: manually uploaded 470 installers do not receive them. The patched official 470.256.02 build needs to download the patch archive over the network; it cannot be placed in `manager/build-kit/` in advance like the build packages.

   On Update driver, choose Upload files manually… and select or drop the five files
   (extensions must be `.run` or `.deb`). Upload starts immediately, with `.deb` files before
   `.run`; content determines each file's checklist row. The window shows progress and errors,
   with the ready count while files are incomplete and blocking reason at the bottom, followed by a completion summary once ready. Canceling
   an upload asks for confirmation and keeps files already received. With all five ready,
   choose Check and continue. The confirmation window opens only after the check succeeds.
   Back to selection returns to the checklist with the files kept. Once the task starts, its uploaded files cannot be replaced.

   The manager reads the installer header, Linux x86_64 marker and version, reads package names,
   architectures and versions, and runs the installer's own integrity check at build time. It
   does not verify NVIDIA signatures or compare official checksums. Fingerprints and checksums
   appear under File details in the confirmation window. A matching checksum alone does not
   prove trust: the installer executes during the build and its output is loaded into the host
   kernel. Kernel headers, module files, and kernel configuration come from the host itself.

For online preparation, the host downloads the driver from NVIDIA, pulls the pinned build image
from Docker Hub when missing, and downloads missing packages from snapshot.debian.org. Toolkit
packages come from NVIDIA's package repository. For the official `470.256.02` driver on `6.12`
or `6.18` kernels, the host also downloads a community patch archive from snapshot.debian.org,
the same source as the build packages. Build-kit package and community-patch fetches follow HTTPS
redirects (at most five); driver and toolkit fetches require a direct successful response. Download
or verification failure stops that stage and is reported in the task log.
Fix connectivity or supply the verified offline inputs, then
create and confirm a new plan. The disposable build container itself has no network and has no
CPU, memory, or process-count limits.

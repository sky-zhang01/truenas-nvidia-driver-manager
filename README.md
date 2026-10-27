<p>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/lockup-dark.svg">
    <img src="docs/images/lockup-light.svg" alt="TrueNAS NVIDIA Driver Manager" width="420">
  </picture>
</p>

Choose an NVIDIA driver in your browser, confirm the plan, and check the installed version on TrueNAS.

Jump to: [Install](#install) · [Update your driver](#update-your-driver) · [Troubleshooting](#troubleshooting) · [Stay safe](#stay-safe) · [FAQ](#faq) · [简体中文](README.zh-CN.md)

<img src="docs/images/home-landing-en.png" width="900" alt="The manager's Home page after sign-in: header, navigation, the driver loaded in the kernel, the GPU and the saved original driver.">

Choose an official NVIDIA version, uploaded files, or a local driver. Every update requires confirmation and checks the loaded version. Save the "original driver" before your first update: a failed post-update check can then attempt recovery. The original is the driver file at the permanent path when you first choose to save it for the current kernel and TrueNAS version; the manager does not determine whether TrueNAS supplied it. Recovery can also fail.

Works on TrueNAS SCALE 25.10.7 and TrueNAS 27.0.0-RC.1 (amd64).

## Install

1. Prepare TrueNAS.

- The host needs an NVIDIA GPU with the TrueNAS NVIDIA driver enabled. Home reports a missing driver.
  - 25.10.7: Apps → Installed → Configuration → Settings, tick Install NVIDIA Drivers, Save.
  - 27.0.0-RC.1: System → Advanced Settings, Configure on the NVIDIA Drivers card, tick Enable NVIDIA GPU Support, Save.
- Create a dataset just for the manager: Datasets, select your pool, Add Dataset, name it, for example `nvm` (folder `/mnt/<pool>/nvm`). Under Advanced Options it must allow running programs (`exec=on`). See the [deployment guide](docs/deploy.en.md) for the exact requirements.
- The host needs internet access (NVIDIA, Docker Hub, Debian package archive). For offline use, see the [deployment guide](docs/deploy.en.md).

2. Install the app from YAML. In Apps → Discover Apps, next to "Custom App" click ⋮ and choose "Install via YAML". Give the app any name. Paste this and change only the data folder (keep `:/var/lib/nvm`), then save:

```yaml
services:
  nvidia-driver-manager:
    image: ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest
    restart: unless-stopped
    privileged: true
    pid: host
    runtime: runc
    volumes:
      - /mnt/<pool>/<dataset>:/var/lib/nvm
    ports:
      - "8199:8199"
```

> [!WARNING]
> `privileged: true` and `pid: host` give the app control of the host itself; it needs them to change the host's driver. Read [Stay safe](#stay-safe) before you install.

The manager is then on port 8199 of your host. For macvlan or ipvlan, another port, or offline use, see [`deploy/compose.yaml`](deploy/compose.yaml) and the [deployment guide](docs/deploy.en.md). TrueNAS removes comments when it saves; that is normal.

3. Copy the setup code. Wait until the app shows as running. In Apps → Installed → your app → Workloads, click the document icon (View Logs) on the container's row. The newest entry is at the bottom; if the log is empty, wait a minute and open it again. Find the latest `setup_code_issued` event. Each log entry is JSON, with `SETUP CODE:` inside its `msg` text. Copy only the setup code (four groups of characters) after `SETUP CODE:`, without quotes or the full stop. Your code differs from this example (other fields and the rest of the message are left out; the real log entry is a single line):

```json
{ "event": "setup_code_issued", "msg": "SETUP CODE: 7KQ2-M9XD-4TWB-HE6R. ..." }
```

4. Create your administrator. Open `http://<nas-ip>:8199/` (`<nas-ip>` is the address you use for TrueNAS; with macvlan, use the app's own address), enter the code, keep the user name `admin`, choose a password of at least 8 characters, type it twice and click Create administrator. You are signed in.

<img src="docs/images/setup-en.png" width="900" alt="The setup page, where you enter the setup code and choose the administrator password.">

*Example only. Your code is in the app logs; enter it and choose the administrator password.*

The code works once and stays valid for 24 hours; restarting the app before setup makes a new one, so use the latest. After setup, sign in with `admin` and your password; no setup code is needed. For rejected codes, see the [FAQ](#faq).

If a tool you used before restores its own driver at boot, a restart may switch the driver in use again; this manager does not change that tool's boot settings, and you decide whether to disable it.

## Update your driver

> [!WARNING]
> Changing the driver interrupts every app that uses the GPU. Pick an idle period, save the original driver on your first update, and keep direct access to the host (the TrueNAS Shell, or a keyboard and screen) in case the GPU does not come back.

1. Open Update driver, tick your GPU(s), choose an official NVIDIA version and click Check and continue.
2. Stop apps marked "Using the GPU now" while waiting; the check continues automatically. The manager does not stop apps for you.
3. Choose "Save original driver and start", then confirm the update.
4. Wait for the task. It keeps running if you close the browser.
5. Confirm Succeeded and the new version on the task page and on Home.
6. Restart GPU apps started before the change, including those listed on Home or reporting "Driver/library version mismatch".

Screenshots for each step are in the update guide below.

## Update guide

<details>
<summary>1. Choose GPUs and a driver</summary>

1. Open Update driver in the menu on the left (at the bottom on a narrow screen). If you have more than one NVIDIA GPU, tick the GPUs that will use the driver.
2. Open the "Official NVIDIA versions" dropdown and pick a version. It lists the latest version of each branch that builds on this host; for another version, choose the last item, "Other version…", and enter the version number.
3. If unsure, open "NVIDIA website ↗", find "Latest Production Branch", and choose that version here if listed.
4. Click Check and continue.

For offline use or your own files (including community-modified installers), choose Upload files manually… and follow the five-file checklist in [Driver sources](#driver-sources).

Debian community compatibility patches are applied only when building official 470.256.02: manually uploaded 470 installers do not receive them. The patched official 470.256.02 build needs to download the patch archive over the network; it cannot be placed in `manager/build-kit/` in advance like the build packages.

<img src="docs/images/update-en.png" width="900" alt="The Update driver page: choose an official NVIDIA driver version and click Check and continue.">

</details>

<details>
<summary>2. Check GPU use</summary>

During the check, a list of apps using or requesting the GPU refreshes automatically. Stop apps marked "Using the GPU now"; the check continues automatically. If GPU use continues after about 30 seconds, the result window includes the snapshot time; stop the apps and choose "Check again".

</details>

<details>
<summary>3. Confirm the update</summary>

Check the source, version and system in the confirmation window. Choose Save original driver and start when offered, or Start update when the original is already saved.

<img src="docs/images/plan-en.png" width="900" alt="The Confirm the update window for an official NVIDIA version, with the buttons Save original driver and start, Start without saving original driver and Back to selection.">

</details>

<details>
<summary>4. Follow the task</summary>

The task page shows the current stage; the live log is further down the page. Only one update runs at a time, and it keeps running if you close the browser. Downloading and building share a default limit of 30 minutes; if it expires before the driver file is replaced, the update stops and the file stays unchanged.

<img src="docs/images/running-en.png" width="900" alt="A task in progress, with the current stage.">

</details>

<details>
<summary>5. Verify the result</summary>

The task ends as Succeeded, and the post-update check shows your new version. Home shows the same version.

<img src="docs/images/success-en.png" width="900" alt="A finished task that reports Update complete.">

</details>

<details>
<summary>6. Restart GPU apps</summary>

Restart GPU apps started before the driver change. Home lists affected apps under "Driver version changed"; apps reporting "Driver/library version mismatch" also need a restart.

</details>

## Local drivers

<details>
<summary>Local drivers: saved versions and available actions</summary>

The Local drivers page lists the drivers the manager has built or saved. You can switch between them without downloading again.

<img src="docs/images/drivers-en.png" width="900" alt="The Local drivers page with the driver in use marked.">

</details>

## Driver sources

<details>
<summary>Three driver sources</summary>

You can choose a driver from three places:

- The official NVIDIA list. The host downloads the selected driver from NVIDIA.
- Files you upload. Choose Upload files manually… and follow the five-row checklist: one driver installer and four toolkit packages. Each row has a download link that opens in a new tab. Use `NVIDIA-Linux-x86_64-<version>-no-compat32.run` to leave out the 32-bit libraries TrueNAS does not need; all four `.deb` files must be amd64 packages of the same version. The toolkit version comes from NVIDIA's official index, with `1.20.1-1` used when it cannot be read.
- A driver you saved before. Switching to it needs no download.

Choosing files starts the upload immediately: `.deb` files first, then the larger `.run`. File content determines the checklist row (extensions must be `.run` or `.deb`). Each row shows progress and errors; the footer shows the ready count while files are incomplete, then a completion summary. Canceling an upload asks for confirmation and keeps files already received. Once all five are ready, choose Check and continue. The confirmation window opens only after the check succeeds; Back to selection returns to the checklist with the files kept. Once the task starts, its uploaded files cannot be replaced. Fingerprints and checksums are under File details in the confirmation window. The manager checks file content and package versions and runs the installer's own integrity check at build time. The program does not verify NVIDIA signatures. If the build fails, the driver file stays unchanged.

</details>

## Troubleshooting

After a failure, check Home to see which driver is loaded.

<details>
<summary>Troubleshooting and recovery</summary>

- The update fails before the driver file is replaced. The driver file is unchanged. Read the message on the task page, fix the cause, and update again. The driver modules may already have been unloaded, so check that your GPU apps still start, and restart them if they do not.
- If the new driver fails its check and an original driver is saved, restoration is attempted. A task showing "Original driver restored" means the requested update failed.
- No original is saved. The manager can only report the failure. Save the original before updating.
- You want the original driver back. Open Local drivers, choose the row whose source is Original, and click Restore this version. This needs no internet.
- The setup code is rejected, expired or missing. See the FAQ entry [Setup code recovery](#faq)
- A task shows "Interrupted" (power loss, host restart). Open Home. It shows the loaded driver version and which step stopped. Check the version, then update again. If your apps still cannot use the GPU, switch to the Original entry in Local drivers. Then, in an idle period, restart the host so TrueNAS loads the current driver again. A restart is not a guarantee of recovery.

If recovery fails, you need direct access to the host. Logs are on each task page. The [operations guide](docs/operations.en.md) lists every case in detail.

</details>

## Stay safe

- The manager can change your host. It has the same power as the TrueNAS administrator. Anyone who can sign in to it controls the driver and the host.
- Restrict access to a trusted management network. The page uses plain HTTP and is reachable from your whole network by default. Do not forward its port to the internet.
- Finish setup soon after the first start. Until then, anyone who can read the app logs can use the setup code.
- Choose a strong password and upload only files you trust. An uploaded installer runs on your host and becomes part of the system.
- Save the original driver and back up the data folder. The data folder holds your account, task history and saved drivers, including the original.

Everything else, such as resource use while building, is in the [security guide](docs/security.en.md).

## FAQ

<details>
<summary>Does it update by itself?</summary>

Every change requires confirmation in the window.

</details>

<details>
<summary>Which systems are supported?</summary>

TrueNAS SCALE 25.10.7 and TrueNAS 27.0.0-RC.1 on amd64 are supported and tested. Other versions and architectures are untested. Maxwell, Pascal and Volta GPUs have not been tested on TrueNAS 27. A version in the list does not promise it works with your GPU or apps. Read the checks and warnings before you confirm.

</details>

<details>
<summary>Which driver does my GPU use?</summary>

The manager chooses the kernel module type from the GPUs you tick. For driver versions 555 and later, when at least one GPU is selected, it builds proprietary modules if any selected GPU requires them (including Maxwell, Pascal and Volta); otherwise it builds open modules. The last driver branch supporting Maxwell, Pascal and Volta is 580.

For Kepler GPUs (including some GTX 600/700 models), choose official 470.256.02. It is offered on TrueNAS 25.10 (kernel 6.12) and TrueNAS 27 (kernel 6.18); the manager downloads and applies Debian's community compatibility patches. Patching and compilation have been tested against the kernels of TrueNAS 25.10.7 and 27.0.0-RC.1. Loading, display and CUDA on real Kepler hardware have not been verified. NVIDIA no longer maintains 470 or publishes security fixes for it; use it only for GPUs with no other supported driver.

These Debian patches make 470 refuse to load when the kernel has IBT (Indirect Branch Tracking) enabled. The patch notes identify Intel 11th-generation and newer CPUs. In the TrueNAS Shell, run `grep '^flags[[:space:]]*:' /proc/cpuinfo | grep -c -w ibt` to count lines reporting `ibt`. A result of 0 only means this command found no match; it does not establish that 470 can load. With no match, the command exits with status 1.

</details>

<details>
<summary>Why is the version list empty or a download failing?</summary>

The host needs to reach NVIDIA's download site, Docker Hub and Debian's package archive. Check your firewall and DNS, then look at the task log. The versions shown also depend on the GPU you ticked. An old GPU only sees the versions it can use.

</details>

<details>
<summary>What should I do if an update stops halfway?</summary>

Open Home and the task page. They show which step stopped and the loaded driver version. The [operations guide](docs/operations.en.md) explains each stage.

</details>

<details>
<summary>What should I do if the setup code is rejected, expired or missing?</summary>

1. Reload the manager page. An expired code is replaced by a new one in the logs.
2. Open the app logs again and copy the code from the newest line that contains `SETUP CODE:`. The newest line is at the bottom. Copy only the setup code (four groups of characters), without quotes.
3. If there is no such line, restart the app from Apps → Installed, wait a minute, and read the logs again.
4. If the page says to try again later, wait the number of seconds shown. Wrong codes and wrong passwords share one limit.

</details>

<details>
<summary>How do I reset the account?</summary>

1. Open the Shell of the host itself in TrueNAS (System → Shell). The manager's own Shell button shows nothing, so do not use it.
2. Delete the account file `web-credentials.json` in the data folder. Replace `<data folder>` with the path you typed in the YAML, the part before `:/var/lib/nvm`. This removes the account and the saved SSH keys, and signs out every device. Drivers and tasks stay:

   ```sh
   rm '<data folder>/manager/secrets/web-credentials.json'
   ```

3. Reload the page. It now shows the setup page again, and the app writes a new code to its logs.
4. In TrueNAS, open Apps → Installed, click your app, and on the Workloads card click the document icon (View Logs) on the container's row. Copy the code from the latest message that contains `SETUP CODE:`. If no new message appears, reload the page once more, then restart the app.
5. Enter the code and choose a new password. You do not need to restart the app. Register your SSH keys again if you use them.

The [operations guide](docs/operations.en.md) has the details.

</details>

<details>
<summary>Why does the app's Shell button in TrueNAS show nothing?</summary>

The manager's image has no shell inside. That is expected. Use the task page for diagnostics.

</details>

<details>
<summary>How do I upgrade or remove the manager?</summary>

Back up the data folder first. With the `latest` tag, open Apps → Installed, click your app, and on the Application Info card choose ⋮ → Update when a new image is offered. To move to a fixed version instead, click Edit, change the image tag to the version number, and save. Changing the app does not change the NVIDIA driver on the host. If you want the original driver back, do that in Local drivers first. The [deployment guide](docs/deploy.en.md) has the details. A step-by-step guide for removing the manager is not written yet.

The image tag in the YAML is `latest`. To stay on a fixed version, change it to `0.3.2`: `ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:0.3.2`.

</details>

## For advanced users

<details>
<summary>Guides, agent access and image verification</summary>

- [Deployment guide](docs/deploy.en.md): networks (bridge, macvlan, ipvlan), app upgrade and revert, offline install and offline builds, online sources.
- [Operations guide](docs/operations.en.md): return to the original driver, troubleshooting, SSH key sign-in, password reset, backup, interrupted tasks, logs.
- [Security guide](docs/security.en.md): how the manager works, what access it needs, session handling, TLS and known limits.
- [Agent guide](docs/agent.en.md) and the [agent skill](skills/nvidia-driver-replacement/SKILL.md): let an AI agent such as Claude Code or Codex do an update. The agent is instructed to ask you before every replacement, but the manager cannot verify that it did. A signed-in agent has full administrator power.
- [Security reporting](SECURITY.md), [contributing and local development](CONTRIBUTING.md), [changelog](CHANGELOG.md).

<details>
<summary>Optional: verify that an image comes from this repository</summary>

```bash
gh attestation verify oci://ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:0.3.2 --repo sky-zhang01/truenas-nvidia-driver-manager
```

</details>

</details>

## Help

Open an issue and include: the manager version, your TrueNAS and kernel versions, the GPU model, the driver source and target version, the task status and the task log. Remove addresses, paths, passwords and keys from the log first. Report security problems privately, as described in [SECURITY.md](SECURITY.md).

## Acknowledgements

The 470 compatibility patches come from Debian's `nvidia-graphics-drivers-tesla-470` packaging (470.256.02-10), maintained by Debian's NVIDIA maintainers; most are by Andreas Beckmann, with contributions from several others. Debian marks these patches with a public-domain dedication that waives copyright. The manager downloads them from snapshot.debian.org at build time and applies them unchanged; this repository does not contain the patches. Parts of the build script come from `biohazardious/truenas-nvidia-driver-updater`. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## License and trademarks

The source code is MIT-licensed. See [`LICENSE`](LICENSE) and [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

This project is not affiliated with or endorsed by iXsystems or NVIDIA. TrueNAS is a trademark of iXsystems, Inc. NVIDIA is a trademark of NVIDIA Corporation. The logo and brand material, including the embedded wordmarks, are not MIT-licensed. Their rights stay with their owners.

The manager's image does not contain the NVIDIA driver. For an official update, your host downloads it from NVIDIA. Uploaded installers come from you. NVIDIA driver software follows NVIDIA's license terms, apart from this project's MIT license.

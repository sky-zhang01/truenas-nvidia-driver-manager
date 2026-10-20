<p>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/lockup-dark.svg">
    <img src="docs/images/lockup-light.svg" alt="TrueNAS NVIDIA Driver Manager" width="420">
  </picture>
</p>

TrueNAS has no button for changing the NVIDIA driver. This App adds one: pick a version in your browser, confirm, and the manager installs the driver and checks it.

**Jump to:** [Install](#install) · [Update your driver](#update-your-driver) · [Troubleshooting](#troubleshooting) · [Stay safe](#stay-safe) · [FAQ](#faq) · [简体中文](README.zh-CN.md)

<img src="docs/images/home-landing-en.png" width="900" alt="The manager's Home page after sign-in: header, navigation, the driver loaded in the kernel, the GPU and the saved original driver.">

- **Pick a version.** An official NVIDIA driver, one you uploaded, or one you saved before.
- **One confirmation.** Nothing updates by itself; every change waits for your click.
- **A checked result.** The manager confirms that the new driver is really loaded.
- **A way back.** It keeps the driver that came with TrueNAS (the "original driver") and tries to restore it if the check fails. Recovery can fail too, so always check Home afterwards.

Works on TrueNAS SCALE **25.10.7** and TrueNAS **27.0.0-RC.1** (amd64), both tested on real hosts.

## Install

**1. Prepare TrueNAS.**

- The NAS needs an NVIDIA GPU. Switch on the NVIDIA driver in TrueNAS; the manager replaces it, so it has to exist first. If it is missing, Home tells you.
  - **25.10.7:** **Apps → Installed → Configuration → Settings**, tick **Install NVIDIA Drivers**, **Save**.
  - **27.0.0-RC.1:** **System → Advanced Settings**, **Configure** on the **NVIDIA Drivers** card, tick **Enable NVIDIA GPU Support**, **Save**.
- Create a dataset just for the manager: **Datasets**, select your pool, **Add Dataset**, name it, for example `nvm` (folder `/mnt/<pool>/nvm`). Under **Advanced Options** it must allow running programs (`exec=on`). See the [deployment guide](docs/deploy.en.md) for the exact requirements.
- The NAS needs internet access (NVIDIA, Docker Hub, Debian package archive). Offline? See the [deployment guide](docs/deploy.en.md).

**2. Install the App from YAML.** In **Apps → Discover Apps**, next to "Custom App" click **⋮** and choose **Install via YAML**. Give the App any name. Paste this and change only the data folder (keep `:/var/lib/nvm`), then save:

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
> `privileged: true` and `pid: host` give the App control of the NAS itself; it needs them to change the host's driver. Read [Stay safe](#stay-safe) before you install.

The manager is then on port 8199 of your NAS. For macvlan or ipvlan, another port, or offline use, see [`deploy/compose.yaml`](deploy/compose.yaml) and the [deployment guide](docs/deploy.en.md). TrueNAS removes comments when it saves; that is normal.

**3. Copy the setup code.** Wait until the App shows as running. In **Apps → Installed → your App → Workloads**, click the document icon (**View Logs**) on the container's row. The newest entry is at the bottom; if the log is empty, wait a minute and open it again. Find the latest `setup_code_issued` event. Each log entry is JSON, with `SETUP CODE:` inside its `msg` text. Copy only the four groups after `SETUP CODE:`, without quotes or the full stop. Your code differs from this example (other fields and the rest of the message are left out; the real log entry is a single line):

```json
{ "event": "setup_code_issued", "msg": "SETUP CODE: 7KQ2-M9XD-4TWB-HE6R. ..." }
```

**4. Create your administrator.** Open `http://<nas-ip>:8199/` (`<nas-ip>` is the address you use for TrueNAS; with macvlan, use the App's own address), enter the code, keep the user name `admin`, choose a password of at least 8 characters, type it twice and click **Create administrator**. You are signed in.

<img src="docs/images/setup-en.png" width="900" alt="The setup page, where you enter the setup code and choose the administrator password.">

*Example only. Your code is in the App logs; enter it and choose the administrator password.*

The code works once and stays valid for 24 hours; restarting the App before setup makes a new one, so always use the latest. The code is only for the first start. Later visits use `admin` and your password; restarting the App after setup needs no code. Code rejected? See the [FAQ](#faq).

## Update your driver

> [!WARNING]
> Changing the driver interrupts every App that uses the GPU. Pick an idle period, save the original driver on your first update, and keep direct access to the NAS (the TrueNAS Shell, or a keyboard and screen) in case the GPU does not come back.

1. Open **Update driver**, tick your GPU(s), choose an official NVIDIA version and click **Check and continue**.
2. Stop any App the check lists as using the GPU. The manager never stops your Apps.
3. Choose **Save original and start** and confirm.
4. Wait for the task. It keeps running if you close the browser.
5. Confirm **Succeeded** and the new version on the task page and on Home.
6. **Restart your GPU Apps**, especially those under "Driver version changed" on Home or failing with "Driver/library version mismatch".

Screenshots for each step are in the update guide below.

## Update guide

<details>
<summary>1. Choose your GPUs and an official NVIDIA driver version, then click <strong>Check and continue</strong> (with screenshot)</summary>

1. Open **Update driver** in the menu on the left (at the bottom on a narrow screen). If you have more than one NVIDIA GPU, tick the GPUs that will use the driver.
2. Click the "Official NVIDIA versions" dropdown to choose a version.
3. If unsure, open the "NVIDIA website link", find "Latest Production Branch", and choose that version here if listed.
4. Click **Check and continue**.

The **Upload .run** button is for an installer you downloaded yourself; see [Expand Driver sources](#driver-sources) below.

<img src="docs/images/update-en.png" width="900" alt="The Update driver page: choose an official NVIDIA driver version and click Check and continue.">

*Choose an official NVIDIA version, then check it before continuing.*

</details>

<details>
<summary>2. Read the check and stop any Apps it lists as still using the GPU before trying again</summary>

Before asking for confirmation, the manager checks your GPU, kernel and Apps. If it lists Apps still using the GPU, stop those Apps yourself and try again. The manager never stops your Apps for you.

</details>

<details>
<summary>3. Check the version and system, save the original driver, and confirm the update (with screenshot)</summary>

The confirmation window shows the source, version and your system. If it asks whether to save the original driver, choose **Save original and start**; this is the safest choice. If an original is already saved, the button reads **Start update**.

<img src="docs/images/plan-en.png" width="900" alt="The Confirm the update window for an official NVIDIA version, with the buttons Save original and start, Start without saving and Back to selection.">

*Check the version and system, and save the original driver before starting.*

</details>

<details>
<summary>4. Wait for the task to finish; it keeps running if you close the browser (with screenshot)</summary>

The task page shows the current stage; the live log is further down the page. Only one update runs at a time, and it keeps running if you close the browser. Downloading and building share a default limit of 30 minutes; if it expires before the driver file is replaced, the update stops and the file stays unchanged.

<img src="docs/images/running-en.png" width="900" alt="A task in progress, with the current stage.">

*The task page highlights the current stage.*

</details>

<details>
<summary>5. Confirm <strong>Succeeded</strong> and check that the task and Home show your new driver version (with screenshot)</summary>

The task ends as **Succeeded**, and the post-update check shows your new version. Home shows the same version.

<img src="docs/images/success-en.png" width="900" alt="A finished task that reports Update complete.">

*Confirm both the successful task status and the version in the post-update check.*

</details>

<details>
<summary>6. Restart your GPU Apps, including those listed under "Driver version changed" or failing with "Driver/library version mismatch"</summary>

After an update, restart the Apps that use the GPU. If the Home page lists Apps under "Driver version changed", restart those. If an App fails to start with a message such as "Driver/library version mismatch", restart that App too.

</details>

## Local drivers

<details>
<summary>Local drivers: saved versions and available actions</summary>

The **Local drivers** page lists the drivers the manager has built or saved. You can switch between them without downloading again.

<img src="docs/images/drivers-en.png" width="900" alt="The Local drivers page with the driver in use marked.">

*Local drivers marks the version in use and keeps saved versions available.*

</details>

## Driver sources

<details>
<summary>Expand the three driver sources</summary>

You can choose a driver from three places:

- **The official NVIDIA list.** The NAS downloads the driver from NVIDIA. This is the way described above, and the right one for most people.
- **A file you upload.** For this, download NVIDIA's Linux installer (a `.run` file) yourself and add the four helper packages the Update driver page names.
- **A driver you saved before.** Switching to it needs no download.

</details>

## Troubleshooting

After any failure, always check Home to see which driver is loaded.

<details>
<summary>Expand troubleshooting and recovery guidance</summary>

- **The update fails before the driver file is replaced.** The driver file is unchanged. Read the message on the task page, fix the cause, and update again. The driver modules may already have been unloaded, so check that your GPU Apps still start, and restart them if they do not. The Home page shows which driver is loaded.
- **The new driver fails its check, and an original is saved.** The manager tries to put the original driver back. If the task reads "Original driver restored", the recovery worked. The update itself did not.
- **No original is saved.** The manager can only report the failure. This is why we suggest saving the original first.
- **You want the original driver back.** Open **Local drivers**, choose the row whose source is **Original**, and click **Check this version and continue**. This needs no internet.
- **The setup code is rejected, expired or missing.** See the FAQ entry [Expand "The setup code is rejected, expired or missing."](#faq)
- **A task shows "Interrupted"** (power loss, host restart). Open Home. It shows the loaded driver version and which step stopped. Check the version, then update again. If your Apps still cannot use the GPU, switch to the **Original** entry in **Local drivers**. Then, in an idle period, restart the NAS so TrueNAS loads the current driver again. A restart is not a guarantee of recovery.

If recovery fails, you need direct access to the NAS. Logs are on each task page. The [operations guide](docs/operations.en.md) lists every case in detail.

</details>

## Stay safe

- **The manager can change your NAS.** It has the same power as the TrueNAS administrator. Anyone who can sign in to it controls the driver and the host.
- **Keep it on your home network.** The page uses plain HTTP and is reachable from your whole network by default. Do not forward its port to the internet.
- **Finish setup soon after the first start.** Until then, anyone who can read the App logs can use the setup code.
- **Choose a strong password** and upload only files you trust. An uploaded installer runs on your NAS and becomes part of the system.
- **Save the original driver and back up the data folder.** The data folder holds your account, task history and saved drivers, including the original.

Everything else, such as resource use while building, is in the [security guide](docs/security.en.md).

## FAQ

<details>
<summary>Does it update by itself?</summary>

No. Every change needs your click on the confirmation window.

</details>

<details>
<summary>Will it work on other TrueNAS versions or on ARM?</summary>

TrueNAS SCALE 25.10.7 and TrueNAS 27.0.0-RC.1 on amd64 are supported and tested. Other versions and architectures are untested. Maxwell, Pascal and Volta GPUs have not been tested on TrueNAS 27. A version in the list does not promise it works with your GPU or Apps. Read the checks and warnings before you confirm.

</details>

<details>
<summary>The version list is empty, or the download fails.</summary>

The NAS needs to reach NVIDIA's download site, Docker Hub and Debian's package archive. Check your firewall and DNS, then look at the task log. The versions shown also depend on the GPU you ticked. An old GPU only sees the versions it can use.

</details>

<details>
<summary>An update stopped halfway.</summary>

Open Home and the task page. They show which step stopped and the loaded driver version. The [operations guide](docs/operations.en.md) explains each stage.

</details>

<details>
<summary>The setup code is rejected, expired or missing.</summary>

1. Reload the manager page. An expired code is replaced by a new one in the logs.
2. Open the App logs again and copy the code from the newest line that contains `SETUP CODE:`. The newest line is at the bottom. Copy only the four groups, without quotes.
3. If there is no such line, restart the App from **Apps → Installed**, wait a minute, and read the logs again.
4. If the page says to try again later, wait for the time it shows. Wrong codes and wrong passwords share one limit.

</details>

<details>
<summary>I forgot the password.</summary>

1. Open the Shell of the NAS itself in TrueNAS (**System → Shell**). The manager's own Shell button shows nothing, so do not use it.
2. Delete the account file `web-credentials.json` in the data folder. Replace `<data folder>` with the path you typed in the YAML, the part before `:/var/lib/nvm`. This removes the account and the saved SSH keys, and signs out every device. Drivers and tasks stay:

   ```sh
   rm '<data folder>/manager/secrets/web-credentials.json'
   ```

3. Reload the page. It now shows the setup page again, and the App writes a new code to its logs.
4. In TrueNAS, open **Apps → Installed**, click your App, and on the **Workloads** card click the document icon (**View Logs**) on the container's row. Copy the code from the latest message that contains `SETUP CODE:`. If no new message appears, reload the page once more, then restart the App.
5. Enter the code and choose a new password. You do not need to restart the App. Register your SSH keys again if you use them.

The [operations guide](docs/operations.en.md) has the details.

</details>

<details>
<summary>The App's Shell button in TrueNAS shows nothing.</summary>

The manager's image has no shell inside. That is expected. Use the task page for diagnostics.

</details>

<details>
<summary>How do I update or remove the manager itself?</summary>

Back up the data folder first. With the `latest` tag, open **Apps → Installed**, click your App, and on the **Application Info** card choose **⋮ → Update** when a new image is offered. To move to a fixed version instead, click **Edit**, change the image tag to the version number, and save. Changing the App does not change the NVIDIA driver on the NAS. If you want the original driver back, do that in **Local drivers** first. The [deployment guide](docs/deploy.en.md) has the details. A step-by-step guide for removing the manager is not written yet.

The image tag in the YAML is `latest`. To stay on a fixed version, change it to `0.3.0`: `ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:0.3.0`.

</details>

## For advanced users

<details>
<summary>Expand guides, agent access and image verification</summary>

- [Deployment guide](docs/deploy.en.md): networks (bridge, macvlan, ipvlan), App upgrade and revert, offline install and offline builds, online sources.
- [Operations guide](docs/operations.en.md): return to the original driver, troubleshooting, SSH key sign-in, password reset, backup, interrupted tasks, logs.
- [Security guide](docs/security.en.md): how the manager works, what access it needs, session handling, TLS and known limits.
- [Agent guide](docs/agent.en.md) and the [agent skill](skills/nvidia-driver-replacement/SKILL.md): let an AI agent such as Claude Code or Codex do an update. The agent is instructed to ask you before every replacement, but the manager cannot verify that it did. A signed-in agent has full administrator power.
- [Security reporting](SECURITY.md), [contributing and local development](CONTRIBUTING.md), [changelog](CHANGELOG.md).

<details>
<summary>Optional: verify that an image comes from this repository</summary>

```bash
gh attestation verify oci://ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:0.3.0 --repo sky-zhang01/truenas-nvidia-driver-manager
```

</details>

</details>

## Help

Open an issue and include: the manager version, your TrueNAS and kernel versions, the GPU model, the driver source and target version, the task status and the task log. Remove addresses, paths, passwords and keys from the log first. Report security problems privately, as described in [SECURITY.md](SECURITY.md).

## License and trademarks

The source code is MIT-licensed. See [`LICENSE`](LICENSE) and [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

This project is **not affiliated with or endorsed by iXsystems or NVIDIA**. TrueNAS is a trademark of iXsystems, Inc. NVIDIA is a trademark of NVIDIA Corporation. The logo and brand material, including the embedded wordmarks, are not MIT-licensed. Their rights stay with their owners.

The manager's image does not contain the NVIDIA driver. For an official update, your NAS downloads it from NVIDIA. Uploaded installers come from you. NVIDIA driver software follows NVIDIA's license terms, apart from this project's MIT license.

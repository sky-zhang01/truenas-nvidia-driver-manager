# Changelog

[简体中文](CHANGELOG.zh-CN.md)

Each version starts with one line on what it means for you. Under it, "Before you update" comes first (only when you have something to do), then "New", "Changed" and "Fixed". The last bullet, when there is one, is for developers.

## 0.4.0

You can now turn on an optional full-speed GPU unlock, and the page tells you which driver each card needs.

New:

- A full-speed GPU unlock (gpu-unlock). It lifts the half-speed limit that GeForce RTX 30, 40 and 20 series cards have on some AI calculations. For an official download or an upload it is off by default, and you tick it for each update. A saved local driver keeps the unlock setting it was saved with. It works only with driver 610.43.03 and takes effect after the next boot. RTX 30 cards are stable, RTX 40 and RTX 20 cards are experimental, and RTX 50 cards are not supported. This project has not measured how much faster your card gets.
- Home shows a "Full-speed GPU unlock" result for each card the unlock is meant for: unlocked, needs a full power-off, installed and waiting for the next start, or not unlocked together with the line from the system log. Saved drivers built with the unlock are marked "· with full-speed unlock". The success page tells you whether the next boot will unlock.
- The Update driver page says which driver a card needs, or the newest one it can use, for example "RTX 5090 needs driver 580 or newer."

Changed:

- GPUs you set aside for virtual machines stay with the virtual machines. If the unlock would break that, the update is undone and your saved original driver comes back.
- The unlock files are made on your own machine from the NVIDIA 610.43.03 installer that the update already uses, and are kept for later updates. Nothing extra is downloaded, and the manager does not carry any unlock files. If the installer cannot be used, the update fails and the driver stays as it was. An update with the unlock shows two extra steps on the task page: making the unlock files, and a trial load that unlocks no card.
- The documentation and the image description use the current TrueNAS product name.
- For developers: the API changes are listed in the [agent guide](docs/agent.en.md).

## 0.3.2

Driver 470.256.02 for older Kepler cards can now be chosen on TrueNAS 27 as well, and the page explains which driver versions those cards cannot use. It has not yet been tested on real Kepler cards.

New:

- Official driver 470.256.02, for Kepler cards, can be chosen on TrueNAS 27 as well as on TrueNAS 25.10. It uses compatibility patches from Debian. It has not yet been tested on real Kepler cards for loading, display and CUDA (GPU computing).
- Update driver has an "Other version…" choice for typing an official version that is not in the list. Versions that NVIDIA's website does not have are refused before the update starts.
- Help text explains how to choose a driver for older GPUs, what the saved original driver is, and how another tool's boot settings can change the driver after a restart. The source of the Debian patches is credited.

Changed:

- On TrueNAS 27 the list no longer offers versions that cannot be used with its system: every 550, 565 and 575 version, and some 570 versions.
- When an official version cannot be used, the page says why and, if one exists, names another version from the same series.

## 0.3.1

You can see when an update is waiting for a busy GPU, and manual upload is easier to follow.

New:

- You can cancel the wait for a busy GPU. If the GPU is still in use at the end, the final notice lets you check again.
- The GPU hint names the lowest driver series the card can use on this machine, for example "supported from driver 580".

Changed:

- When the GPU is in use, the page lists the apps that asked for it within about a second. Each app is marked "Using the GPU now", requested only, or unknown. The list refreshes while the update waits up to about 30 seconds, and the update goes on by itself once the GPU is free. The manager looks only at those apps' own processes and open GPU files, and it never stops an app.
- For cards that use NVIDIA's open-source driver variant, official 580 versions before 580.126.09 are refused. They cannot be used with newer system versions, including TrueNAS 25.10.7.
- Manual upload is now a checklist of the five files it needs. Each row has a direct download link and says when its file is ready or what to fix. Uploads show progress and can be cancelled, and going back from the confirmation window keeps your files.
- Page text and guides are shorter, with one word for each idea. Links to other websites open in a new tab.

## 0.3.0

TrueNAS 27 now works as well as TrueNAS 25.10, and short GPU use by other apps no longer stops an update.

New:

- TrueNAS 27 is supported next to TrueNAS 25.10. If you prepare for offline use, you need only one set of build files for both.
- The update stops at once, before the original driver is saved or anything is downloaded, if the build tools cannot match your system.
- The task page shows the GPUs the system reports. This is for diagnosis only and does not change the result of the task.

Changed:

- The page lists the TrueNAS container instances that use the GPU on both TrueNAS 27 and TrueNAS 25.10.
- Short GPU use by other apps no longer blocks an update. Starting an app or running its health check can open the GPU for a second or two. Before the manager decides the GPU is in use, and when the old driver cannot be removed because something still holds it, it looks again for up to about 30 seconds. The Update button and the task page show a short waiting line, and the final notice says how long the manager waited.
- The README starts with installing. The step-by-step guides, troubleshooting and FAQ follow in folded sections, and every screenshot was retaken at one size.

Fixed:

- After the new driver is loaded, the manager checks which version is really running. On TrueNAS, starting any app that uses the GPU can bring the old driver back. If that happens, the manager loads the new driver once more before the final check. This happens at most once per update or restore and shows as its own step.
- The manager always loads the three main parts of the NVIDIA driver again, plus any other part that was loaded when the task started. A main part that an earlier task left unloaded comes back.

## 0.2.1

Setup now uses a one-time code instead of an initial password.

Before you update:

- If you set `NVM_INITIAL_ADMIN_PASSWORD` in the app configuration, remove it. It is no longer used. Going back to 0.2.0 after creating an account through setup is not supported.

New:

- To reset your account, delete the credentials file, reload the page and read a new code from the app logs. SSH keys can be registered right after setup.

Changed:

- You create the administrator with a one-time setup code from the app logs and a password you choose on the setup page. The install YAML and the app settings no longer contain an initial password. A code expires after 24 hours or a restart, and a new one is issued on the next setup request.
- Task status and the current step are easier to scan. Readings that are unfinished or unavailable now say so, and the task details heading appears only when there are details.
- The Local drivers table has short column names and sort arrows, with sort controls that also work with screen readers. Values stay readable, the action buttons fit on a desktop screen, and a narrow screen scrolls sideways.
- The README is a step-by-step guide for first-time users, with a screenshot of each step. The security model and known limits are in the [security guide](docs/security.en.md).

## 0.2.0

First public release.

New:

- Manage NVIDIA drivers from a web page in English or Chinese, with light and dark themes. Tested on TrueNAS SCALE 25.10.7 on a 64-bit Intel or AMD computer.
- Choose an official driver download, upload an installer with its matching container toolkit packages (the NVIDIA parts that let apps use the GPU), or switch between saved drivers. You see the GPU and system checks before you confirm each plan.
- Drivers are built in disposable build containers that have no network, using a fixed build environment. You can prepare the images and packages ahead for offline builds.
- The original driver is saved, the driver file is replaced in one step, and the loaded versions are checked. If that check fails, the manager tries to restore the saved original.
- You can follow task progress and logs, read guidance for interrupted tasks and see how much space saved drivers use. Recovery files are released only after safety checks.
- Sign in with an administrator password or a registered SSH key. An AI agent signs in the same way, and every replacement plan must be confirmed explicitly.
- Install the app from GitHub's container registry (GHCR) with bridge, macvlan or ipvlan networking (the three ways an app can get its network address). The documentation covers deployment, operation, security reporting and contributing.

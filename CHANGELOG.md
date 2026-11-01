# Changelog

[简体中文](CHANGELOG.zh-CN.md)

## 0.4.0

- Add an optional full-speed GPU unlock (gpu-unlock) for GeForce RTX 30, 40 and 20 series cards. It lifts the half-rate limit on FP16, BF16 and TF32 matrix math with FP32 accumulation, using the GPL-2.0-only kernel module of Zagitta/gpu-unlock 2.0.0, vendored unchanged. It is off by default, chosen per update for official downloads and uploads, supported only with driver 610.43.03, and takes effect from the next boot. The RTX 30 series is stable, the RTX 40 and RTX 20 series are experimental, and the RTX 50 series is not supported. The throughput gain has not been measured by this project.
- Generate the unlock firmware on the host from the NVIDIA 610.43.03 installer the task already uses, check it against 17 fixed SHA-256 values and cache it for later updates; nothing extra is downloaded and the program ships no firmware. An unusable installer fails the task with the driver untouched.
- Add two task stages for an update with the unlock: "Generating the unlock files" and "Test-loading the unlock program (no card is unlocked)", which loads the module without targets before the driver file is replaced.
- The success check requires vfio-pci to load before the unlock module and the driver, so GPUs isolated for virtual machines stay on vfio-pci; otherwise the task switches back to the saved original. The success page says whether the next boot will unlock.
- Home shows a "Full-speed GPU unlock" reading per target card (unlocked, needs a full power-off, the unlock is installed and waiting for the next startup, or not unlocked with the system log line). Local driver rows built with the unlock are marked "· with full-speed unlock".
- Agent API: `source.gpu_unlock` on plan requests, `gpus[].gpu_unlock` in versions, `rows[].gpu_unlock_targets` in drivers, `gpu_unlock` in home, `notice.unlock` in task details, and the `plan.source_invalid.gpu_unlock_unsupported` refusal.
- The driver hints on the update page now say which driver a card needs, or the newest one it can use, for example "RTX 5090 needs driver 580 or newer."
- The documentation and image description now use the current TrueNAS product name.

## 0.3.2

- Offer official 470.256.02 on TrueNAS 27 as well as TrueNAS 25.10, applying all 54 compatibility patches from Debian tesla-470 packaging 470.256.02-10 at build time. Kepler loading, display and CUDA remain unverified on real hardware.
- On TrueNAS 27, omit all versions from branches 550, 565 and 575, and some 570 versions, because they cannot build against its kernel.
- When an official version cannot be built, explain the cause and name an alternative version of the same branch when one is available.
- Add "Other version…" to the update page to enter an official version outside the list.
- Reject versions absent from the NVIDIA website before starting the update.
- Explain driver choices for older GPUs, the saved original driver, and how a previous tool's boot settings can affect the driver after a restart; credit the Debian patch source.

## 0.3.1

- When the GPU is in use, the page lists the apps that requested it within about a
  second, marked as using the GPU, requested only, or unknown. The list refreshes while the
  program waits up to about 30 seconds, and the update continues once the GPU is free. The
  program reads only those containers' processes and open device files, and never stops an app.
- The wait can be cancelled, and the final GPU-in-use notice can check again.
- The GPU hint names the lowest driver branch the GPU can use on this host, for example
  "supported from driver 580".
- Official 580 releases before 580.126.09 are refused for GPUs that use open kernel modules:
  they cannot be built on kernels from 6.12.97, including TrueNAS 25.10.7.
- Manual upload is a checklist of the 5 files it needs. Each row has a direct download link and
  shows when its file is ready or what to fix. Uploads show progress and can be cancelled; going
  back from the confirmation keeps the files.
- Interface text and guides are shorter, with one term for each concept.
- Links to other websites open in a new tab.

## 0.3.0

- Support TrueNAS 27 alongside TrueNAS 25.10. Driver builds run in one pinned official Debian 13
  environment that carries gcc 14 (the compiler of the TrueNAS 27 kernel) and gcc 12 (for
  TrueNAS 25.10); offline preparation needs this one environment.
- Read the kernel's compiler before saving the original or downloading anything; a kernel built
  with a compiler the environment does not carry stops the task at once.
- List the container instances that use the GPU on TrueNAS 27 and on TrueNAS 25.10; the page
  calls them TrueNAS container instances.
- Show the GPUs that `nvidia-smi` reports on the task page as a diagnostic; it does not change
  the task result.
- Check the loaded driver version after reloading the modules. On TrueNAS, any container start
  (Docker's default NVIDIA runtime) and `nvidia-cdi-refresh` can load the modules of the image
  merged at that moment; when the loaded modules are stale, unload and reload them once more
  before the success check. Each update and restore does this at most once and shows it as its
  own step.
- When unloading a driver module is refused because something holds it briefly, wait for the
  hold to drop (checking about every 0.2 seconds, at most about 30 seconds per module) and unload
  it again. Container health checks through the NVIDIA runtime open an idle GPU for about two
  seconds at a time.
- Wait for a short GPU hold to end before deciding the GPU is in use. A container start or
  health check can open the GPU for a second or two; every occupancy reading (Update, before
  unloading, the module check after writing, and restoring the original) re-reads about every
  0.2 seconds for at most about 30 seconds. The Update button and the task page show a short
  waiting line, and the final notice says how long the program waited.
- Always reload `nvidia`, `nvidia_drm` and `nvidia_modeset`, as TrueNAS does when it enables
  the driver, plus any other driver module loaded at the start, so a module left unloaded by an
  earlier task is loaded again.
- Start the README with installing: a screenshot of Home after sign-in, four points, then four
  install steps with the YAML on the page. The step-by-step guides, troubleshooting and FAQ
  follow in folded sections; every screenshot is retaken at one size.

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

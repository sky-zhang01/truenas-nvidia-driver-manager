# Changelog

[简体中文](CHANGELOG.zh-CN.md)

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

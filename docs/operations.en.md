# Operations guide

[简体中文](operations.zh-CN.md)

## Return to the saved stock driver

Open the local drivers table and select a usable **original** row. Review and confirm its
plan just as for another replacement. It uses the saved image without downloading or building;
without a usable original for the current kernel and TrueNAS version, this action is unavailable.
A downloaded NVIDIA version is not a replacement for the saved TrueNAS stock image.

The automatic restore path runs after a replacement's driver check fails, when a usable stock
copy exists. It does not cover every download, build, unload, or write failure, and restoration
can fail too. `STOCK_RESTORED` means recovery succeeded, not that the requested update succeeded.
Inspect task check records and loaded versions; App availability must be checked separately.

## Troubleshooting and reporting a problem

First read the home-page blockers, task status, error, and check records. Download/build failures
can come from unreachable NVIDIA, Docker Hub, or Debian snapshot endpoints. For the official
`470.256.02` driver on `6.12` kernels, the host also downloads a community patch archive from
`deb.debian.org`; allow this additional source through the outbound firewall. Fix connectivity
or prepare the offline inputs in the [deployment guide](deploy.en.md), then confirm a new plan.
Do not repeatedly submit while a task is running or its state is unreadable.

For a public issue, include the manager version/image tag, TrueNAS and kernel versions, GPU
model, selected source and target, task status, and redacted logs. Remove host addresses, local
paths, credentials, cookies, tokens, and private keys. Report suspected vulnerabilities through
[SECURITY.md](../SECURITY.md). Keep unresolved recovery files until the manager's checks allow
release; do not delete task directories to bypass a block.

## SSH key sign-in

The administrator account can register SSH public keys. The login page then grows a
collapsed "Sign in with an SSH key" section: copy a one-time challenge, sign it with
your private key in your own terminal, paste the signature back and you are signed in
without typing the password. The password entrance always stays.

- Accepted key algorithms: `ssh-ed25519`, `ecdsa-sha2-nistp256`, `ecdsa-sha2-nistp384`
  (no RSA, sk-* or certificates). At most 5 keys.
- The private key never enters this program: the page only submits public keys and
  signatures. If private key material is pasted into the public key box, the comment
  box or the signature box, the client refuses it, clears that input and sends
  nothing to the server.
- Registration lives in the "SSH sign-in keys" section of the Account page; keys can be
  registered immediately after setup.
- The terminal signing command (replace `<challenge>` with the current challenge on
  the page):

  ```sh
  printf '%s' '<challenge>' | ssh-keygen -Y sign -f ~/.ssh/id_ed25519 -n nvm-login
  ```

  ssh-agent / 1Password work too (the export must be its own line):

  ```sh
  export SSH_AUTH_SOCK=~/.1password/agent.sock
  printf '%s' '<challenge>' | ssh-keygen -Y sign -f <(ssh-add -L) -n nvm-login
  ```

  With several keys in the agent, `<(ssh-add -L)` silently picks the first one; use
  the ready-made per-key command under each key on the account page, or point `-f`
  at the registered public key file. 1Password needs its SSH agent enabled in its
  settings.
- Deleting a key immediately ends every signed-in session (this browser receives a
  replacement cookie and stays online; other devices sign in again); password
  sign-in is unaffected.
- Failed SSH sign-ins, failed password sign-ins, and wrong setup codes share one rate-limit bucket.

## Forgot the password: resetting the administrator account

The credentials file lives in the data directory:
`<data-directory>/manager/secrets/web-credentials.json`. To reset it:

1. Open the **host's** Shell in the TrueNAS web UI (System → Shell) or log in to the host
   over SSH. The app's own **Shell** button cannot be used: the image is empty-base (`scratch`) and
   carries no shell (see the [deployment guide](deploy.en.md)).
2. Delete the credentials file. This resets the account and removes all registered SSH keys;
   it does not delete drivers or tasks:

   ```sh
   rm '<data-directory>/manager/secrets/web-credentials.json'
   ```

3. **Without restarting the App**, reload the manager page. It shows setup and writes a new
   code to the App logs. In TrueNAS, open **Apps → Installed → this App → Workloads** and click
   the container row's document icon, **View Logs**. Copy the whole code from `msg` in the newest
   JSON log message containing `SETUP CODE:`.
4. Enter that code on the manager page, choose and repeat a new password, and click
   **Create administrator**. Register any needed SSH public keys again.

Notes:

- Deleting this file removes the account, its SSH keys and all existing sessions. Task state,
  local driver records and the saved original are unaffected. The data folder is the host directory
  you entered in the YAML volume mapping; the login page shows a generic credentials path with
  a data-folder placeholder.
- A code works once, expires after 24 hours, and is invalidated by an App restart. After expiry,
  reopening the page or submitting setup writes a new code; copy the code from `msg` in the
  newest log message containing `SETUP CODE:`. A live code is never replaced just because
  someone submits a wrong code.
- The setup code appears only in the `msg` of the `setup_code_issued` App log event. It is never
  written to task logs or task state. App log readers and collectors can see it while it is live;
  redact setup-code messages before sharing logs. Passwords, hashes, challenges, signatures,
  full public keys and session tokens are not logged.
- Setup, password and SSH sign-in share rate limits. Once the quota is exhausted, a correct
  code or password is temporarily refused too; wait for the displayed retry time. There is no
  permanent lockout. A reachable attacker can delay sign-ins by exhausting the quota; restrict
  access to a trusted management network. Restarting clears the quota and invalidates any
  pending setup code.
- Setup codes and passwords need a trusted connection; plain HTTP exposes both in transit.
  Configure a TLS front end when a trusted management LAN is insufficient.

Returning to 0.2.0 after creating an account through setup is unsupported. Keep the pre-upgrade
backup if you need to return to that release.

## Backup and restore

- Back up the data directory: the credentials file, the task state and logs, and the local driver
  records (including the saved original driver file).
- `<data-dir>/manager/build-kit-cache/` holds the verified build packages (about 53 MiB). It can be
  deleted at any time; the next build downloads those packages again.
- To restore, put the whole data directory back in place and start the App with the same compose
  file. The credentials file decides the account and password in use; restore it too and you do not
  have to rebuild the account.

## Interrupted tasks

A replacement runs in a host systemd unit and can survive an App-container restart. If the host
restarts, loses power, or the runner ends before recording a terminal result, reconciliation can
mark the task `INTERRUPTED`. The notice at the top of the home page says what the step it stopped on
means, and the task page's details give that step and the task id.

- Stopped while reading the version, saving the original, downloading or building: the driver file on
  the permanent path was not replaced and the driver on the host did not change. Choose a version and
  update again.
- Stopped while unloading the driver modules: the driver file was not replaced, but driver modules
  may already be unloaded.
- Stopped while writing, refreshing or reloading, or during the matching unload, write, refresh or
  reload of a restore: this page cannot tell whether the driver file was replaced, and driver modules
  may be unloaded without having been loaded again.
- What to do: check the loaded driver version on the home page and the check records in the task
  details to see the host's current state, then choose a version and update again. If modules did not
  come back and applications cannot use the GPU, check the current image and arrange a maintenance
  window before considering a host restart
  so the system can merge the current sysext image and load its modules again. This is not a
  guarantee of recovery from a damaged or unsuitable image.
- An unsettled task keeps its directory and inputs until a later task finishes with a verified
  replacement or restore. During an idle refresh, the program releases them automatically only
  after checking that the later task's library image and permanent image still match, its target
  modules are loaded, both driver version readings match, and the host's writable mount and
  read-only properties are reset. A reboot or re-merge is allowed when those readings still match.
  Any retained task with host leftovers blocks release for all tasks and refuses a new update.
- A terminal task whose directory is retained offers **Release recovery material**. After
  confirmation it runs the same checks immediately and explains any refusal; it cannot force
  release. The state file and log stay as history, including the release time and the later task's
  id. If directory removal fails after the release is recorded, the next idle refresh retries it.
- The `<id>-build` image in the local driver library uses separate space. Releasing task inputs
  keeps that image; delete it separately in the local drivers table when it is no longer needed.

## Logs

- Task logs are at `manager/state/<task-id>.log` in the data directory; the task page shows their
  tail.
- The lines this program writes to `docker logs` are English, one JSON object per line, readable
  directly with `docker logs <container-name>`.
- `task_cleanup` (`task cleanup finished`) is written once when the task directory is deleted.
  Retaining a directory or repeating an already completed cleanup writes no completion line.
  `host_reset` records an observed host change with `task_id` and `step`: `delete_temp_file`,
  `unmount_writable_mount`, or `restore_readonly`. An unchanged host writes no reset line;
  failures are reported separately as `task_cleanup_step_failed` with the step and reason.

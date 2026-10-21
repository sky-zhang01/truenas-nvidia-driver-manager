#![allow(dead_code)]
//! An in-memory host model that intercepts web and runner commands and file operations, with a
//! controllable clock.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use truenas_nvidia_driver_manager::codes::{
    CANONICAL_PATH, RW_CANONICAL_PATH, RW_MOUNT, RW_TMP_PATH,
};
use truenas_nvidia_driver_manager::facts::{self, MountLine};
use truenas_nvidia_driver_manager::host::{
    At, Cmd, Create, Exit, Host, HostLock, LockId, Output, Stream, SyncWrite, Watch,
};
use truenas_nvidia_driver_manager::runner::build_env::BuildEnv;

pub const HOST_DATA_DIR: &str = "/mnt/<pool>/data";
pub const KERNEL: &str = "6.12.105-production+truenas";
pub const TRUENAS_VERSION: &str = "25.10.7";
/// The mode every fixture-created directory is given explicitly, whatever the process umask is.
const FIXTURE_DIR_MODE: u32 = 0o700;

/// `create_dir_all` with an explicit private mode (the process umask still applies, which can only
/// ever remove bits, never add group/other access).
fn make_private_dir(real: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(FIXTURE_DIR_MODE)
            .create(real)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(real)
    }
}

pub const DATASET: &str = "<pool>/ROOT/<env>/usr";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRecord {
    pub program: String,
    pub args: Vec<String>,
    pub at: At,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
}

impl CommandRecord {
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![self.program.clone()];
        argv.extend(self.args.clone());
        argv
    }
}

#[derive(Debug, Clone)]
pub struct FileOp {
    pub kind: &'static str,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct FakeOutput {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub delay: Duration,
}

impl FakeOutput {
    pub fn ok() -> FakeOutput {
        FakeOutput {
            exit: Exit::Code(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    pub fn code(n: i32) -> FakeOutput {
        FakeOutput {
            exit: Exit::Code(n),
            ..FakeOutput::ok()
        }
    }

    pub fn stdout(text: &str) -> FakeOutput {
        FakeOutput {
            stdout: text.as_bytes().to_vec(),
            ..FakeOutput::ok()
        }
    }

    pub fn with_stderr(mut self, text: &str) -> FakeOutput {
        self.stderr = text.as_bytes().to_vec();
        self
    }

    pub fn with_code(mut self, n: i32) -> FakeOutput {
        self.exit = Exit::Code(n);
        self
    }

    pub fn stderr(text: &str) -> FakeOutput {
        FakeOutput {
            stderr: text.as_bytes().to_vec(),
            ..FakeOutput::ok()
        }
    }
}

/// A gate a command waits on until the test releases it.
#[derive(Default)]
pub struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
    /// Set once a command is actually waiting on the gate, so a test can advance the clock after
    /// the command's deadline was computed.
    blocked: Mutex<bool>,
}

impl Gate {
    pub fn release(&self) {
        let mut open = self.open.lock().unwrap();
        *open = true;
        self.cv.notify_all();
    }

    pub fn is_open(&self) -> bool {
        *self.open.lock().unwrap()
    }

    /// True while a command is inside the gate wait.
    pub fn blocked(&self) -> bool {
        *self.blocked.lock().unwrap()
    }

    /// Wait for a command barrier, allowing a busy runner time to start.
    pub fn wait_blocked<T>(&self, runner: &std::thread::JoinHandle<T>) -> bool {
        let started = Instant::now();
        while !self.blocked() {
            if runner.is_finished() || started.elapsed() >= Duration::from_secs(30) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }

    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.cv.wait(open).unwrap();
        }
    }

    /// Wait on the gate and record that a caller really is inside the wait, so a test can use the
    /// gate as a deterministic barrier (the same flag the command path sets while a command waits).
    pub fn wait_observed(&self) {
        *self.blocked.lock().unwrap() = true;
        self.wait();
    }
}

#[derive(Clone)]
struct Preset {
    program: String,
    args: Vec<String>,
    output: FakeOutput,
    gate: Option<Arc<Gate>>,
    repeat: Option<u32>,
}

#[derive(Clone)]
struct CurlResponse {
    status: u16,
    body: Vec<u8>,
    exit: i32,
    stderr: String,
    /// Bytes real `curl` already wrote to `--output` before it failed.
    partial: Vec<u8>,
    /// `Location` of a 3xx response. Absent on an ordinary response.
    location: Option<String>,
}

/// The names directly inside a real directory, sorted.
fn real_names(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

struct Clock {
    wall_base_ms: u64,
    offset: Duration,
}

/// A repeating extra reference: busy while the phase is at most `high`, idle for the following `low`.
#[derive(Clone)]
struct PeriodicOpen {
    module: String,
    high: Duration,
    low: Duration,
    phase_at_zero: Duration,
}

/// One extra reference while the fake clock offset is in `[starts_at, starts_at + duration)`.
#[derive(Clone)]
struct SustainedOpen {
    module: String,
    starts_at: Duration,
    duration: Duration,
}

fn extra_refcount(
    at: Duration,
    module: &str,
    periodic: &[PeriodicOpen],
    sustained: &[SustainedOpen],
) -> u64 {
    let mut extra = 0u64;
    for open in periodic.iter().filter(|open| open.module == module) {
        let period = open.high.saturating_add(open.low);
        if period.is_zero() {
            continue;
        }
        let phase = at
            .as_millis()
            .saturating_add(open.phase_at_zero.as_millis())
            % period.as_millis();
        if phase <= open.high.as_millis() {
            extra += 1;
        }
    }
    for open in sustained.iter().filter(|open| open.module == module) {
        let end = open.starts_at.saturating_add(open.duration);
        if at >= open.starts_at && at < end {
            extra += 1;
        }
    }
    extra
}

/// One planned read modification: the `nth` read of the path returns a copy with one byte changed,
/// the file itself is untouched.
struct ReadChange {
    path: String,
    nth: u64,
    seen: u64,
}

struct Inner {
    /// Real sandbox root; host absolute paths are mapped under it.
    sandbox: PathBuf,
    ops: Mutex<Vec<FileOp>>,
    commands: Mutex<Vec<CommandRecord>>,
    presets: Mutex<Vec<Preset>>,
    clock: Mutex<Clock>,
    modules: Mutex<BTreeMap<String, facts::ModuleLine>>,
    refuse_unload: Mutex<BTreeSet<String>>,
    fail_reload: Mutex<BTreeSet<String>>,
    module_load_fail: Mutex<Option<String>>,
    host_mounts: Mutex<Vec<MountLine>>,
    /// The App container's own mount table, what `/proc/self/mountinfo` holds in the container; the
    /// host table above stays separate.
    container_mounts: Mutex<Vec<MountLine>>,
    readonly: Mutex<String>,
    sysext_since: Mutex<u64>,
    refresh_missing: Mutex<bool>,
    driver_version: Mutex<Option<String>>,
    /// Kernel reading frozen at an actual core-module load, independently of the merged image.
    loaded_version: Mutex<Option<String>>,
    loaded_text: Mutex<Option<String>>,
    merged_identity: Mutex<Option<String>>,
    autoload_phases: Mutex<BTreeSet<String>>,
    refuse_correction: Mutex<BTreeSet<String>>,
    periodic_opens: Mutex<Vec<PeriodicOpen>>,
    sustained_opens: Mutex<Vec<SustainedOpen>>,
    /// Consumed on the first `rmmod` of that module, before the in-use decision.
    unload_delays: Mutex<BTreeMap<String, Duration>>,
    /// Remove these modules at the start of the given 1-based `/proc/modules` generation.
    proc_modules_drops: Mutex<Vec<(String, u64)>>,
    /// Advance the fake clock at the start of the given 1-based `/proc/modules` generation,
    /// before that reading's reference counts are computed.
    proc_modules_advances: Mutex<Vec<(u64, Duration)>>,
    proc_modules_reads: Mutex<u64>,
    /// Called at the start of `Host::wait`, before the fake clock moves.
    on_wait: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    fail_state_phases: Mutex<BTreeSet<String>>,
    exe: Mutex<Vec<u8>>,
    uname: Mutex<String>,
    listings: Mutex<BTreeMap<String, String>>,
    modinfo: Mutex<BTreeMap<String, String>>,
    dpkg: Mutex<BTreeMap<String, String>>,
    midclt: Mutex<BTreeMap<String, FakeOutput>>,
    dpkg_passthrough: Mutex<bool>,
    curl: Mutex<BTreeMap<String, CurlResponse>>,
    docker_ps: Mutex<Vec<String>>,
    docker_inspect: Mutex<Vec<u8>>,
    docker_inspect_fails: Mutex<bool>,
    /// The modelled local images of the pinned build environment, reference -> local image ID
    /// (`docker image inspect --format {{.Id}}`). An empty map means the image is not on the host,
    /// so the product pulls it.
    images: Mutex<BTreeMap<String, String>>,
    /// The build environment a started runner uses instead of preparing the pinned one: the fake
    /// host has no official image and no real packages, so a compiling task needs a prepared
    /// environment. `None` runs the production path, which prepares it itself.
    build_env: Mutex<Option<BuildEnv>>,
    /// Injection switches of `docker pull` and `docker image rm`.
    docker_pull_fails: Mutex<bool>,
    docker_pull_id: Mutex<String>,
    /// A pull that prints an ID but leaves nothing local (the race the runner covers: the copy is
    /// gone again before the check after the pull).
    docker_pull_leaves_nothing: Mutex<bool>,
    docker_image_rm_fails: Mutex<bool>,
    /// The modelled containers, name -> running. A `docker run --name=<n>` inserts `true`, `docker
    /// rm` removes the entry, and a test may install stopped or orphaned containers directly. This
    /// models the name set only: no image, id, exit code, restart policy or label exists.
    containers: Mutex<BTreeMap<String, bool>>,
    /// Injection switches of the `docker ps -a --format {{.Names}}` answer.
    docker_ps_all_fails: Mutex<bool>,
    docker_ps_all_times_out: Mutex<bool>,
    docker_ps_all_partial: Mutex<bool>,
    /// Injection switches of `docker rm`.
    docker_rm_noop: Mutex<bool>,
    docker_rm_fails: Mutex<bool>,
    /// A removal that reports a failure although the container really is gone (the race against
    /// another remover; the listing decides).
    docker_rm_fails_after_removal: Mutex<bool>,
    unsquashfs_fails: Mutex<BTreeSet<String>>,
    fail_writes: Mutex<Vec<String>>,
    fail_fsync_dirs: Mutex<Vec<String>>,
    fail_rename_suffix: Mutex<Option<String>>,
    /// `(target suffix, allowed matches, seen matches)`: renames whose target ends with the suffix
    /// fail after `allowed` of them have passed.
    fail_rename_to: Mutex<Vec<(String, usize, usize)>>,
    /// Target names for which a rename of a `.<target>.tmp-…` file back onto the target must fail
    /// (the rollback path of a publish or slot restore).
    fail_rename_rollback: Mutex<Vec<String>>,
    /// Read failures (EACCES) for every path with one of these prefixes.
    fail_reads: Mutex<Vec<String>>,
    /// `(path prefix, marker file)`: the first read below the prefix creates the marker. It lets
    /// another process observe a read that happens inside a critical section the reader holds (test
    /// handshake only).
    read_markers: Mutex<Vec<(String, std::path::PathBuf)>>,
    /// `(path prefix, gate)`: the next file-side operation below the prefix waits for the gate. It
    /// makes a product step inside a critical section observable at exactly that step (test barrier
    /// only).
    path_gate: Mutex<Vec<(String, Arc<Gate>)>>,
    /// `remove_file` failures (EACCES) for these path prefixes.
    fail_removes: Mutex<Vec<String>>,
    /// `rename` failures (EACCES) when the source contains one of these.
    fail_rename_contains: Mutex<Vec<String>>,
    /// `sync_all` failures (EIO) for these path prefixes.
    fail_syncs: Mutex<Vec<String>>,
    /// Contents tests put over a generated `/proc` path.
    proc_overrides: Mutex<BTreeMap<String, Vec<u8>>>,
    /// `/proc` paths a test removed; a generated file then reads as missing.
    proc_removed: Mutex<BTreeSet<String>>,
    /// Planned one-byte-changed reads.
    read_changes: Mutex<Vec<ReadChange>>,
    /// `rename` returns EXDEV for every source ending with one of these.
    fail_rename_exdev: Mutex<Vec<String>>,
    /// Secrets that must never appear in any recorded host command.
    forbidden_in_argv: Mutex<Vec<String>>,
    same_namespace: Mutex<bool>,
    dpkg_default: Mutex<Option<String>>,
    /// Opt-in synthetic package content for browser upload fixtures only.
    dpkg_fixture_content: Mutex<bool>,
    units: Mutex<BTreeMap<String, UnitState>>,
    /// Injection switch of `systemctl list-units`, the drain's unit query.
    list_units_fails: Mutex<bool>,
    /// Injection switch of the host mount table read.
    mount_table_fails: Mutex<bool>,
    /// Directory listings that fail with EACCES, by path prefix.
    fail_read_dirs: Mutex<Vec<String>>,
    /// The host-root prefix `view` maps to. `None` is the host process itself (`/proc/1/root` is
    /// the container case).
    view_prefix: Mutex<Option<String>>,
    /// Lines of `systemctl list-jobs`, one job per line, exactly as a test installed them (e.g.
    /// `123 nvm-task-<id>.service start running`); the `jobs` field below holds something else: the
    /// runner threads.
    job_lines: Mutex<Vec<String>>,
    /// Injection switches of the `systemctl list-jobs` answer.
    list_jobs_fails: Mutex<bool>,
    list_jobs_times_out: Mutex<bool>,
    list_jobs_partial: Mutex<bool>,
    jobs: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// Kill switches of the runner threads `systemd-run` started, keyed by task id: `kill_unit`
    /// flips one so the thread stops at its next host boundary instead of continuing.
    kills: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
    hostname_id: Mutex<String>,
    /// The version the merged sysext image carries, once a merge (or a host reboot) read a
    /// link-target/canonical listing.
    merged_version: Mutex<Option<String>>,
    /// Whether the nvidia extension is merged; `false` makes `status` report no `/usr` extension.
    merged: Mutex<bool>,
    /// Threads the fake host started as task-runner threads: inside them `/proc/self/mounts` is the
    /// host table, not the App container's.
    runner_threads: Mutex<BTreeSet<std::thread::ThreadId>>,
    /// How many recorded commands the red-line assertion already judged. `Drop` judges only the
    /// commands recorded after this cursor, so an explicit early assertion cannot excuse a command
    /// recorded later while a captured violation is still not judged twice.
    red_lines_checked: Mutex<usize>,
    /// Keys (normalized host paths, see [`lock_key`]) of the modelled locks that a handle holds
    /// right now. This is a process-wide in-memory model, not a real OS lock; [`FakeLock`] states
    /// its limits.
    locks_held: Mutex<BTreeSet<String>>,
    /// Use the real kernel `flock` on the opened file instead of the in-process model. Two
    /// processes over one sandbox really contend then, which the cross-process lock windows need;
    /// the default keeps the deterministic in-process model for the single-process suites.
    real_flock: std::sync::atomic::AtomicBool,
    /// Makes `open_lock` fail for every path.
    lock_open_fails: Mutex<bool>,
    /// Makes `try_acquire` fail for every handle.
    lock_acquire_fails: Mutex<bool>,
    /// Overrides the mode `open_lock` creates the lock file with, so a test can model a file the
    /// production validation must refuse.
    lock_mode: Mutex<Option<u32>>,
}

/// Marks the current thread as running the task runner for the fake host; the previous state is
/// restored when the guard drops.
pub struct RunnerScope(bool);

impl Drop for RunnerScope {
    fn drop(&mut self) {
        RUNNER_THREAD.with(|cell| cell.set(self.0));
    }
}

thread_local! {
    /// True while this thread models the runner: `/proc/self/mounts` then reads the host table.
    static RUNNER_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Private unwinding payload of a simulated runner kill.
struct RunnerKilled;

/// Stops the current runner thread at the host boundary that noticed the kill. `resume_unwind`
/// continues the unwind without running the panic hook: a simulated kill is part of the model, not
/// a test failure.
fn stop_killed_runner() -> ! {
    std::panic::resume_unwind(Box::new(RunnerKilled));
}

thread_local! {
    /// The kill switch of the `systemd-run` unit this thread runs, set at the spawn boundary;
    /// `None` on every other thread.
    static RUNNER_KILL: std::cell::RefCell<Option<Arc<AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
}

impl Inner {
    /// Recorded commands must omit `--collect`, `CollectMode`, `docker.update` nvidia calls,
    /// setup-code environment variables, and values forbidden by a test. `concat!` keeps
    /// assertion needles out of the production-source scan.
    fn assert_commands(&self, commands: &[CommandRecord]) {
        let forbidden = self.forbidden_in_argv.lock().unwrap().clone();
        for c in commands {
            let argv = c.argv();
            let joined = argv.join(" ");
            assert!(
                !argv.iter().any(|a| a == concat!("--", "collect")),
                "red line: --collect in {joined}"
            );
            assert!(
                !joined.contains(concat!("Collect", "Mode")),
                "red line: CollectMode in {joined}"
            );
            assert!(
                !joined.contains(concat!("NVM_", "SETUP_CODE")),
                "red line: the setup code variable in {joined}"
            );
            for value in &forbidden {
                assert!(
                    !joined.contains(value.as_str())
                        && !c.env.values().any(|entry| entry.contains(value.as_str())),
                    "a forbidden value reached a host command"
                );
            }
            if c.program.contains("docker") {
                assert!(
                    !joined.contains("docker.update"),
                    "red line: docker.update in {joined}"
                );
                assert!(
                    !(argv.iter().any(|a| a == "update") && joined.contains("nvidia")),
                    "red line: docker update nvidia in {joined}"
                );
            }
        }
        assert!(
            !commands
                .iter()
                .any(|c| c.env.contains_key(concat!("NVM_", "SETUP_CODE"))),
            "a host command saw a setup code variable"
        );
    }

    /// Judge every recorded command and move the cursor past it.
    fn check_red_lines(&self) {
        let commands = self.commands.lock().unwrap().clone();
        *self.red_lines_checked.lock().unwrap() = commands.len();
        self.assert_commands(&commands);
    }

    /// Judge only the commands recorded since the last red-line assertion and move the cursor past
    /// them; the test-end guard uses this so a successful explicit assertion re-arms for commands
    /// that come afterwards.
    fn check_new_red_lines(&self) {
        let commands = self.commands.lock().unwrap().clone();
        let from = {
            let mut cursor = self.red_lines_checked.lock().unwrap();
            let from = *cursor;
            *cursor = commands.len();
            from
        };
        self.assert_commands(&commands[from.min(commands.len())..]);
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.sandbox);
        // Every fake host asserts the red lines when it goes away. Only the commands recorded since
        // the last assertion are judged: a host a test already checked explicitly is not judged
        // twice, while a command recorded after that check is still caught.
        if !std::thread::panicking() {
            self.check_new_red_lines();
        }
    }
}

static FAKE_HOST_SEQ: AtomicU64 = AtomicU64::new(0);

struct UnitState {
    active: String,
    sub: String,
    result: String,
    exec_main_status: i32,
}

#[derive(Clone)]
pub struct FakeHost {
    inner: Arc<Inner>,
}

impl Default for FakeHost {
    fn default() -> Self {
        FakeHost::new()
    }
}

impl FakeHost {
    pub fn new() -> FakeHost {
        let sandbox = std::env::temp_dir().join(format!(
            "nvm-fake-host-{}-{}",
            std::process::id(),
            FAKE_HOST_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&sandbox);
        FakeHost::with_sandbox(&sandbox)
    }

    /// A fixture over an **existing** sandbox directory. Two processes can share one virtual host
    /// this way: their file state is the same real directory (so a `flock` taken by one process
    /// really blocks the other), while each process keeps its own command model and clock. The
    /// directory is created when missing and never wiped, so the caller decides what the shared
    /// scene contains.
    pub fn with_sandbox(sandbox: &std::path::Path) -> FakeHost {
        let sandbox = sandbox.to_path_buf();
        std::fs::create_dir_all(&sandbox).expect("fake host sandbox");
        let host = FakeHost {
            inner: Arc::new(Inner {
                sandbox: sandbox.clone(),
                ops: Mutex::new(Vec::new()),
                commands: Mutex::new(Vec::new()),
                presets: Mutex::new(Vec::new()),
                clock: Mutex::new(Clock {
                    wall_base_ms: 1_790_000_000_000,
                    offset: Duration::ZERO,
                }),
                modules: Mutex::new(BTreeMap::new()),
                refuse_unload: Mutex::new(BTreeSet::new()),
                fail_reload: Mutex::new(BTreeSet::new()),
                module_load_fail: Mutex::new(None),
                host_mounts: Mutex::new(vec![
                    MountLine {
                        source: "/dev/sda2".into(),
                        mount_point: "/".into(),
                        fstype: "zfs".into(),
                        options: "rw".into(),
                    },
                    MountLine {
                        source: DATASET.into(),
                        mount_point: "/usr".into(),
                        fstype: "zfs".into(),
                        options: "ro".into(),
                    },
                ]),
                container_mounts: Mutex::new(vec![
                    MountLine {
                        source: "overlay".into(),
                        mount_point: "/".into(),
                        fstype: "overlay".into(),
                        options: "rw".into(),
                    },
                    MountLine {
                        source: HOST_DATA_DIR.into(),
                        mount_point: "/var/lib/nvm".into(),
                        fstype: "none".into(),
                        options: "rw".into(),
                    },
                ]),
                readonly: Mutex::new("on".into()),
                sysext_since: Mutex::new(1_000_000),
                refresh_missing: Mutex::new(false),
                driver_version: Mutex::new(Some("580.173.02".into())),
                loaded_version: Mutex::new(None),
                loaded_text: Mutex::new(None),
                merged_identity: Mutex::new(None),
                autoload_phases: Mutex::new(BTreeSet::new()),
                refuse_correction: Mutex::new(BTreeSet::new()),
                periodic_opens: Mutex::new(Vec::new()),
                sustained_opens: Mutex::new(Vec::new()),
                unload_delays: Mutex::new(BTreeMap::new()),
                proc_modules_drops: Mutex::new(Vec::new()),
                proc_modules_advances: Mutex::new(Vec::new()),
                proc_modules_reads: Mutex::new(0),
                on_wait: Mutex::new(None),
                fail_state_phases: Mutex::new(BTreeSet::new()),
                exe: Mutex::new(b"#!/bin/fake-runner".to_vec()),
                uname: Mutex::new(KERNEL.into()),
                listings: Mutex::new(BTreeMap::new()),
                modinfo: Mutex::new(BTreeMap::new()),
                dpkg: Mutex::new(BTreeMap::new()),
                midclt: Mutex::new(BTreeMap::new()),
                dpkg_passthrough: Mutex::new(false),
                curl: Mutex::new(BTreeMap::new()),
                docker_ps: Mutex::new(Vec::new()),
                docker_inspect: Mutex::new(b"[]".to_vec()),
                docker_inspect_fails: Mutex::new(false),
                images: Mutex::new(BTreeMap::new()),
                build_env: Mutex::new(None),
                docker_pull_fails: Mutex::new(false),
                docker_pull_id: Mutex::new(
                    truenas_nvidia_driver_manager::build_kit::BUILD_ENV_IMAGE_ID.to_string(),
                ),
                docker_pull_leaves_nothing: Mutex::new(false),
                docker_image_rm_fails: Mutex::new(false),
                containers: Mutex::new(BTreeMap::new()),
                docker_ps_all_fails: Mutex::new(false),
                docker_ps_all_times_out: Mutex::new(false),
                docker_ps_all_partial: Mutex::new(false),
                docker_rm_noop: Mutex::new(false),
                docker_rm_fails: Mutex::new(false),
                docker_rm_fails_after_removal: Mutex::new(false),
                unsquashfs_fails: Mutex::new(BTreeSet::new()),
                fail_writes: Mutex::new(Vec::new()),
                fail_fsync_dirs: Mutex::new(Vec::new()),
                fail_rename_suffix: Mutex::new(None),
                fail_rename_to: Mutex::new(Vec::new()),
                fail_rename_rollback: Mutex::new(Vec::new()),
                fail_reads: Mutex::new(Vec::new()),
                read_markers: Mutex::new(Vec::new()),
                path_gate: Mutex::new(Vec::new()),
                fail_removes: Mutex::new(Vec::new()),
                fail_rename_contains: Mutex::new(Vec::new()),
                fail_syncs: Mutex::new(Vec::new()),
                proc_overrides: Mutex::new(BTreeMap::new()),
                proc_removed: Mutex::new(BTreeSet::new()),
                read_changes: Mutex::new(Vec::new()),
                fail_rename_exdev: Mutex::new(Vec::new()),
                forbidden_in_argv: Mutex::new(Vec::new()),
                same_namespace: Mutex::new(false),
                dpkg_default: Mutex::new(None),
                dpkg_fixture_content: Mutex::new(false),
                units: Mutex::new(BTreeMap::new()),
                list_units_fails: Mutex::new(false),
                mount_table_fails: Mutex::new(false),
                fail_read_dirs: Mutex::new(Vec::new()),
                view_prefix: Mutex::new(None),
                job_lines: Mutex::new(Vec::new()),
                list_jobs_fails: Mutex::new(false),
                list_jobs_times_out: Mutex::new(false),
                list_jobs_partial: Mutex::new(false),
                jobs: Mutex::new(Vec::new()),
                kills: Mutex::new(BTreeMap::new()),
                hostname_id: Mutex::new("a".repeat(64)),
                merged_version: Mutex::new(None),
                merged: Mutex::new(true),
                runner_threads: Mutex::new(BTreeSet::new()),
                red_lines_checked: Mutex::new(0),
                locks_held: Mutex::new(BTreeSet::new()),
                real_flock: std::sync::atomic::AtomicBool::new(false),
                lock_open_fails: Mutex::new(false),
                lock_acquire_fails: Mutex::new(false),
                lock_mode: Mutex::new(None),
            }),
        };
        host.mkdir("/sys/bus/pci/devices");
        host.mkdir("/sys/class/drm");
        host.mkdir("/run");
        host.mkdir("/boot");
        host.put_text(
            &format!("/boot/config-{KERNEL}"),
            "CONFIG_CC_VERSION_TEXT=\"gcc (Debian 12.2.0-14+deb12u1) 12.2.0\"\n",
        );
        host.mkdir("/etc");
        host.mkdir("/proc");
        host.mkdir("/mnt/<pool>/data/manager/secrets");
        host.mkdir("/mnt/<pool>/data/manager/state");
        host.mkdir("/mnt/<pool>/data/manager/tasks");
        host.mkdir("/mnt/<pool>/data/manager/upload");
        host.mkdir("/mnt/<pool>/data/manager/trash");
        host.mkdir("/mnt/<pool>/data/manager/bin");
        host.mkdir("/mnt/<pool>/data/drivers");
        host
    }

    /// `/var/lib/nvm` is the bind mount of the data directory.
    pub fn translate(path: &str) -> String {
        if path == "/var/lib/nvm" {
            return HOST_DATA_DIR.to_string();
        }
        if let Some(rest) = path.strip_prefix("/var/lib/nvm/") {
            return format!("{HOST_DATA_DIR}/{rest}");
        }
        path.to_string()
    }

    pub fn sandbox_root(&self) -> PathBuf {
        self.inner.sandbox.clone()
    }

    pub fn set_same_namespace(&self, same: bool) {
        *self.inner.same_namespace.lock().unwrap() = same;
    }

    pub fn set_dpkg_default(&self, fields: &str) {
        *self.inner.dpkg_default.lock().unwrap() = Some(fields.to_string());
    }

    /// Only explicit browser-test scenarios may interpret the NVM_TEST_DEB fixture format.
    pub fn set_dpkg_fixture_content(&self, enabled: bool) {
        *self.inner.dpkg_fixture_content.lock().unwrap() = enabled;
    }

    // ---- sandbox file helpers ------------------------------------------

    /// The real file-system location of one host absolute path, for reads and writes that follow
    /// symlinks. `Err` when the path cannot be resolved inside the sandbox (a symlink loop or an
    /// unreadable link); the caller then refuses the operation instead of falling back to a fixed
    /// real node that could lie outside the sandbox.
    fn real_path(&self, path: &str) -> std::io::Result<PathBuf> {
        let translated = FakeHost::translate(path);
        self.resolve(&translated, true)
    }

    /// Like [`FakeHost::real_path`], but a symlink in the last component is not followed: the node
    /// itself is meant (`lstat`, `readlink`, unlink, link creation). The `Err` semantics are the
    /// same.
    fn real_path_nofollow(&self, path: &str) -> std::io::Result<PathBuf> {
        let translated = FakeHost::translate(path);
        self.resolve(&translated, false)
    }

    /// Resolve one host path inside the sandbox, following symlinks with the same virtual mapping:
    /// an absolute link target stays under the sandbox, never on the real host.
    fn resolve(&self, virtual_path: &str, follow_final: bool) -> std::io::Result<PathBuf> {
        let mut pending: Vec<String> = virtual_path
            .trim_start_matches('/')
            .split('/')
            .filter(|c| !c.is_empty())
            .map(|c| c.to_string())
            .collect();
        let mut base = self.inner.sandbox.clone();
        let mut index = 0usize;
        let mut links = 0u32;
        while index < pending.len() {
            let part = pending[index].clone();
            index += 1;
            if part == "." {
                continue;
            }
            if part == ".." {
                // The sandbox is the virtual root: `..` never climbs above it. Every `base` is the
                // sandbox or a descendant of it, so the root is the only place where a pop would
                // leave the sandbox; there it stays put.
                if base != self.inner.sandbox {
                    base.pop();
                }
                continue;
            }
            let is_last = index == pending.len();
            let next = base.join(&part);
            if is_last && !follow_final {
                base = next;
                continue;
            }
            match std::fs::symlink_metadata(&next) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    links += 1;
                    if links > 40 {
                        return Err(std::io::Error::other("too many symbolic links"));
                    }
                    let target = std::fs::read_link(&next)?;
                    let mut target_parts: Vec<String> = target
                        .to_string_lossy()
                        .split('/')
                        .filter(|c| !c.is_empty())
                        .map(|c| c.to_string())
                        .collect();
                    target_parts.extend(pending[index..].iter().cloned());
                    pending = target_parts;
                    index = 0;
                    // An absolute target restarts at the sandbox root; a relative one starts where
                    // the link lives (base is already that directory).
                    if target.is_absolute() {
                        base = self.inner.sandbox.clone();
                    }
                }
                _ => base = next,
            }
        }
        Ok(base)
    }

    fn ensure_parent(&self, real: &Path) {
        if let Some(parent) = real.parent() {
            // Fixture directories are private by default: a permissive process umask must never
            // widen `manager/control` or the global lock directory, whose modes the product really
            // checks. The production checks stay exactly as they are, and no test touches the
            // global umask.
            let _ = make_private_dir(parent);
        }
    }

    fn clear_node(&self, real: &Path) {
        match std::fs::symlink_metadata(real) {
            Ok(meta) if meta.is_dir() => {
                let _ = std::fs::remove_dir_all(real);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(real);
            }
            Err(_) => {}
        }
    }

    pub fn put_file(&self, path: &str, bytes: impl Into<Vec<u8>>) {
        let path = FakeHost::translate(path);
        if path.starts_with("/proc/") {
            self.inner
                .proc_overrides
                .lock()
                .unwrap()
                .insert(path.clone(), bytes.into());
            self.inner.proc_removed.lock().unwrap().remove(&path);
            return;
        }
        let real = self
            .real_path(&path)
            .unwrap_or_else(|e| panic!("fake host put_file refuses unresolvable path {path}: {e}"));
        self.ensure_parent(&real);
        self.clear_node(&real);
        std::fs::write(&real, bytes.into()).expect("fake host put_file");
    }

    pub fn put_text(&self, path: &str, text: &str) {
        self.put_file(path, text.as_bytes().to_vec());
    }

    pub fn put_link(&self, path: &str, target: &str) {
        let real = self
            .real_path_nofollow(path)
            .unwrap_or_else(|e| panic!("fake host put_link refuses unresolvable path {path}: {e}"));
        self.ensure_parent(&real);
        self.clear_node(&real);
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &real).expect("fake host put_link");
        #[cfg(not(unix))]
        std::fs::write(&real, b"").expect("fake host put_link");
    }

    pub fn mkdir(&self, path: &str) {
        let real = self
            .real_path(path)
            .unwrap_or_else(|e| panic!("fake host mkdir refuses unresolvable path {path}: {e}"));
        make_private_dir(&real).expect("fake host mkdir");
    }

    /// Create a FIFO at a host path. `lstat` reports it as a FIFO and any read of it blocks until a
    /// writer opens it, which is the node the product must refuse before reading.
    pub fn put_fifo(&self, path: &str) {
        let real = self
            .real_path_nofollow(path)
            .unwrap_or_else(|e| panic!("fake host put_fifo refuses unresolvable path {path}: {e}"));
        self.ensure_parent(&real);
        self.clear_node(&real);
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let c = std::ffi::CString::new(real.as_os_str().as_bytes())
                .expect("a fixture path carries no NUL");
            // SAFETY: `c` is a valid NUL-terminated path inside the sandbox.
            let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
            assert_eq!(rc, 0, "mkfifo {}", real.display());
        }
        #[cfg(not(unix))]
        panic!("a FIFO fixture needs a unix host");
    }

    pub fn exists(&self, path: &str) -> bool {
        let path = FakeHost::translate(path);
        if path.starts_with("/proc/") {
            return self
                .inner
                .proc_overrides
                .lock()
                .unwrap()
                .contains_key(&path)
                || !self.inner.proc_removed.lock().unwrap().contains(&path);
        }
        let Ok(real) = self.real_path_nofollow(&path) else {
            return false;
        };
        std::fs::symlink_metadata(real).is_ok()
    }

    pub fn children(&self, dir: &str) -> Vec<String> {
        let Ok(real) = self.real_path(dir) else {
            return Vec::new();
        };
        real_names(&real).unwrap_or_default()
    }

    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        let path = FakeHost::translate(path);
        if path.starts_with("/proc/") {
            return self
                .inner
                .proc_overrides
                .lock()
                .unwrap()
                .get(&path)
                .cloned();
        }
        let Ok(real) = self.real_path(&path) else {
            return None;
        };
        std::fs::read(real).ok()
    }

    pub fn file_text(&self, path: &str) -> Option<String> {
        self.file(path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    pub fn remove(&self, path: &str) {
        let path = FakeHost::translate(path);
        if path.starts_with("/proc/") {
            self.inner.proc_overrides.lock().unwrap().remove(&path);
            self.inner.proc_removed.lock().unwrap().insert(path);
            return;
        }
        if let Ok(real) = self.real_path_nofollow(&path) {
            self.clear_node(&real);
        }
    }

    pub fn remove_tree(&self, path: &str) {
        if let Ok(real) = self.real_path_nofollow(path) {
            let _ = std::fs::remove_dir_all(real);
        }
    }

    // ---- fault injection ---------------------------------

    /// Make every read of a path with this prefix fail with EACCES.
    pub fn fail_read(&self, path: &str) {
        let path = FakeHost::translate(path);
        self.inner.fail_reads.lock().unwrap().push(path);
    }

    /// The `nth` (1-based) read of this path returns one byte changed; the file itself is not
    /// modified.
    pub fn change_on_read(&self, path: &str, nth: u64) {
        let path = FakeHost::translate(path);
        self.inner.read_changes.lock().unwrap().push(ReadChange {
            path,
            nth: nth.max(1),
            seen: 0,
        });
    }

    /// Every `rename` whose source contains this text fails with EACCES (the suffix form cannot
    /// match a name ending in a random hex tail).
    pub fn fail_rename_contains(&self, needle: &str) {
        self.inner
            .fail_rename_contains
            .lock()
            .unwrap()
            .push(needle.to_string());
    }

    /// Every `remove_file` of a path with this prefix fails with EACCES.
    pub fn fail_remove(&self, path: &str) {
        let path = FakeHost::translate(path);
        self.inner.fail_removes.lock().unwrap().push(path);
    }

    /// Every `sync_all` of a file whose path has this prefix fails with EIO.
    pub fn fail_sync_file(&self, path: &str) {
        let path = FakeHost::translate(path);
        self.inner.fail_syncs.lock().unwrap().push(path);
    }

    /// Every `rename` whose source ends with this suffix fails with EXDEV.
    pub fn fail_rename_exdev(&self, suffix: &str) {
        self.inner
            .fail_rename_exdev
            .lock()
            .unwrap()
            .push(suffix.to_string());
    }

    /// Register a value that must never appear in a recorded host command.
    pub fn forbid_in_argv(&self, value: &str) {
        self.inner
            .forbidden_in_argv
            .lock()
            .unwrap()
            .push(value.to_string());
    }

    /// Route `dpkg-deb` to the real program of the caller's environment. Without this the preset
    /// answer is returned.
    pub fn set_dpkg_passthrough(&self, on: bool) {
        *self.inner.dpkg_passthrough.lock().unwrap() = on;
    }

    /// The `/proc` and mount-table files the host model generates instead of storing on disk.
    fn generated_proc(&self, path: &str) -> std::io::Result<Option<Vec<u8>>> {
        if path.starts_with("/proc/") {
            if self.inner.proc_removed.lock().unwrap().contains(path) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file",
                ));
            }
            if let Some(bytes) = self.inner.proc_overrides.lock().unwrap().get(path) {
                return Ok(Some(bytes.clone()));
            }
        }
        if path == "/proc/modules" {
            let nth = {
                let mut reads = self.inner.proc_modules_reads.lock().unwrap();
                *reads += 1;
                *reads
            };
            let dropping: Vec<String> = self
                .inner
                .proc_modules_drops
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, at)| *at == nth)
                .map(|(name, _)| name.clone())
                .collect();
            for name in dropping {
                self.unload_module(&name);
            }
            let advances: Vec<Duration> = self
                .inner
                .proc_modules_advances
                .lock()
                .unwrap()
                .iter()
                .filter(|(at, _)| *at == nth)
                .map(|(_, by)| *by)
                .collect();
            for by in advances {
                self.advance(by);
            }
            let at = self.offset();
            let periodic = self.inner.periodic_opens.lock().unwrap().clone();
            let sustained = self.inner.sustained_opens.lock().unwrap().clone();
            let modules = self.inner.modules.lock().unwrap();
            let mut text = String::new();
            for (name, m) in modules.iter() {
                let refcount = m.refcount + extra_refcount(at, name, &periodic, &sustained);
                text.push_str(&format!(
                    "{} {} {} {} Live 0x0000000000000000\n",
                    name,
                    m.size,
                    refcount,
                    if m.deps.is_empty() {
                        "-".to_string()
                    } else {
                        m.deps.join(",")
                    }
                ));
            }
            return Ok(Some(text.into_bytes()));
        }
        if path == "/proc/driver/nvidia/version" {
            let Some(text) = self.inner.loaded_text.lock().unwrap().clone() else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file",
                ));
            };
            return Ok(Some(text.into_bytes()));
        }
        if path == "/proc/self/mountinfo" {
            // The App container's own table; `mount`/`umount` never change it.
            let id = self.inner.hostname_id.lock().unwrap().clone();
            let mut text = String::new();
            for m in self.inner.container_mounts.lock().unwrap().iter() {
                text.push_str(&format!(
                    "1 0 0:1 {} {} {} - {} {} {}\n",
                    m.mount_point, m.mount_point, m.options, m.fstype, m.source, m.options
                ));
            }
            text.push_str(&format!(
                "9 1 0:9 /var/lib/docker/containers/{id}/hostname /etc/hostname rw - ext4 /dev/sda1 rw\n"
            ));
            return Ok(Some(text.into_bytes()));
        }
        if path == "/proc/1/mountinfo" {
            let mut text = String::new();
            for m in self.inner.host_mounts.lock().unwrap().iter() {
                text.push_str(&format!(
                    "1 0 0:1 {} {} {} - {} {} {}\n",
                    m.mount_point, m.mount_point, m.options, m.fstype, m.source, m.options
                ));
            }
            return Ok(Some(text.into_bytes()));
        }
        if path == "/proc/self/mounts" {
            // The runner thread reads the host table through this path; the web process reads the
            // App container's own table.
            let table = if RUNNER_THREAD.with(|cell| cell.get()) {
                self.inner.host_mounts.lock().unwrap().clone()
            } else {
                self.inner.container_mounts.lock().unwrap().clone()
            };
            let mut text = String::new();
            for m in &table {
                text.push_str(&format!(
                    "{} {} {} {} 0 0\n",
                    m.source, m.mount_point, m.fstype, m.options
                ));
            }
            return Ok(Some(text.into_bytes()));
        }
        Ok(None)
    }

    /// One planned read modification: the nth read returns a copy with one byte changed.
    fn apply_read_change(&self, path: &str, mut bytes: Vec<u8>) -> std::io::Result<Vec<u8>> {
        let mut changes = self.inner.read_changes.lock().unwrap();
        for change in changes.iter_mut() {
            if change.path != path {
                continue;
            }
            change.seen += 1;
            if change.seen == change.nth && !bytes.is_empty() {
                let last = bytes.len() - 1;
                bytes[last] ^= 0x01;
            }
        }
        Ok(bytes)
    }

    fn note(&self, kind: &'static str, path: &str) {
        self.inner.ops.lock().unwrap().push(FileOp {
            kind,
            path: path.to_string(),
        });
    }

    pub fn ops(&self) -> Vec<FileOp> {
        self.inner.ops.lock().unwrap().clone()
    }

    pub fn ops_for(&self, path: &str) -> Vec<&'static str> {
        let path = FakeHost::translate(path);
        self.inner
            .ops
            .lock()
            .unwrap()
            .iter()
            .filter(|o| o.path == path)
            .map(|o| o.kind)
            .collect()
    }

    pub fn any_op_for(&self, path: &str, kind: &str) -> bool {
        let path = FakeHost::translate(path);
        self.inner
            .ops
            .lock()
            .unwrap()
            .iter()
            .any(|o| o.path == path && o.kind == kind)
    }

    pub fn clear_ops(&self) {
        self.inner.ops.lock().unwrap().clear();
    }

    // ---- commands ------------------------------------------------------

    pub fn commands(&self) -> Vec<CommandRecord> {
        self.inner.commands.lock().unwrap().clone()
    }

    pub fn count(&self, program: &str) -> usize {
        self.inner
            .commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.program == program)
            .count()
    }

    pub fn argv_log(&self) -> Vec<Vec<String>> {
        self.inner
            .commands
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.argv())
            .collect()
    }

    pub fn preset(&self, program: &str, args: &[&str], output: FakeOutput) {
        self.inner.presets.lock().unwrap().push(Preset {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            output,
            gate: None,
            repeat: None,
        });
    }

    pub fn preset_once(&self, program: &str, args: &[&str], output: FakeOutput) {
        self.inner.presets.lock().unwrap().push(Preset {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            output,
            gate: None,
            repeat: Some(1),
        });
    }

    pub fn set_gate(&self, program: &str, args: &[&str]) -> Arc<Gate> {
        self.set_gate_output(program, args, FakeOutput::ok())
    }

    pub fn set_gate_output(&self, program: &str, args: &[&str], output: FakeOutput) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        self.inner.presets.lock().unwrap().push(Preset {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            output,
            gate: Some(gate.clone()),
            repeat: None,
        });
        gate
    }

    /// True when a recorded host command carries the setup-code variable in its environment;
    /// it must never do so.
    pub fn setup_code_in_env(&self) -> bool {
        let needle = concat!("NVM_", "SETUP_CODE");
        self.commands()
            .iter()
            .any(|c| c.env.keys().any(|k| k == needle))
    }

    /// The red-line assertion. Every fake host runs it when it is dropped, judging the commands
    /// recorded since the last assertion, so a test can assert early without excusing a command
    /// recorded later. A failing assertion moves the cursor too, so the same captured violation is
    /// not judged a second time when the host is dropped.
    pub fn assert_red_lines(&self) {
        self.inner.check_red_lines();
    }

    /// Mark the current thread as the runner for the fake host: while the returned guard lives,
    /// `/proc/self/mounts` reads the host mount table instead of the App container's. `systemd-run`
    /// uses it automatically; a test that calls `runner::…` directly enters it explicitly.
    pub fn enter_runner_scope(&self) -> RunnerScope {
        let previous = RUNNER_THREAD.with(|cell| cell.replace(true));
        RunnerScope(previous)
    }

    /// The virtual target of a host symlink, or `None` when the path is not a symlink.
    fn link_target(&self, path: &str) -> Option<String> {
        let real = self.real_path_nofollow(path).ok()?;
        let meta = std::fs::symlink_metadata(&real).ok()?;
        if !meta.file_type().is_symlink() {
            return None;
        }
        std::fs::read_link(&real)
            .ok()
            .map(|t| t.to_string_lossy().into_owned())
    }

    /// Whether a virtual host path exists (following symlinks).
    fn virtual_exists(&self, path: &str) -> bool {
        let Ok(real) = self.real_path(path) else {
            return false;
        };
        std::fs::symlink_metadata(real).is_ok()
    }

    /// The driver version carried by the image listing a test registered for `target`, if any.
    fn listing_version(&self, target: &str) -> Option<String> {
        let listing = self.inner.listings.lock().unwrap().get(target).cloned()?;
        let entries = facts::parse_unsquashfs_lls(&listing).ok()?;
        match facts::image_facts(&entries).file_version {
            facts::Reading::Value(v) => Some(v),
            facts::Reading::Unknown(_) => None,
        }
    }

    /// Refreshing userspace never changes a module already resident in the kernel.
    fn driver_reading(&self) -> Option<String> {
        self.inner.loaded_version.lock().unwrap().clone()
    }

    /// Inject an external module load at the persisted write phase, after the pass unloaded
    /// modules and before it changes the image. Each selected phase fires only once.
    pub fn autoload_on_write(&self, restore: bool) {
        self.inner
            .autoload_phases
            .lock()
            .unwrap()
            .insert(if restore { "restore_write" } else { "write" }.into());
    }

    pub fn refuse_correction_unload(&self, module: &str) {
        self.inner
            .refuse_correction
            .lock()
            .unwrap()
            .insert(module.into());
    }

    pub fn fail_state_phase(&self, phase: &str) {
        self.inner
            .fail_state_phases
            .lock()
            .unwrap()
            .insert(phase.into());
    }

    fn persisted_phase(&self) -> Option<String> {
        let states = self
            .real_path(&format!("{HOST_DATA_DIR}/manager/state"))
            .ok()?;
        for entry in std::fs::read_dir(states).ok()?.flatten() {
            if entry.path().extension().is_some_and(|ext| ext == "json") {
                if let Ok(bytes) = std::fs::read(entry.path()) {
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        if value["status"] == "RUNNING" {
                            return value["phase"].as_str().map(str::to_string);
                        }
                    }
                }
            }
        }
        None
    }

    /// Model a host reboot: units are gone, `/run` is cleared, the modelled locks and the container
    /// and job models are cleared (a reboot drops `/run` state and every job of the transaction),
    /// the host mount table is back to its boot state, and the modules come back from the file at
    /// the formal path -- a canonical image that carries a listing therefore decides the loaded
    /// version reading. Clearing the modelled containers says nothing about a real container's
    /// restart policy; the model has none. This models only the listed effects; it makes no claim
    /// about a real host.
    pub fn reboot_host(&self) {
        self.clear_units();
        self.inner.locks_held.lock().unwrap().clear();
        self.inner.containers.lock().unwrap().clear();
        self.inner.job_lines.lock().unwrap().clear();
        if let Ok(run) = self.real_path("/run") {
            let _ = std::fs::remove_dir_all(&run);
            let _ = std::fs::create_dir_all(&run);
        }
        *self.inner.host_mounts.lock().unwrap() = vec![
            MountLine {
                source: "/dev/sda2".into(),
                mount_point: "/".into(),
                fstype: "zfs".into(),
                options: "rw".into(),
            },
            MountLine {
                source: DATASET.into(),
                mount_point: "/usr".into(),
                fstype: "zfs".into(),
                options: "ro".into(),
            },
        ];
        self.inner.modules.lock().unwrap().clear();
        let version = if self.virtual_exists(CANONICAL_PATH) {
            self.listing_version(CANONICAL_PATH)
        } else {
            None
        };
        *self.inner.loaded_version.lock().unwrap() = None;
        *self.inner.loaded_text.lock().unwrap() = None;
        match version {
            Some(version) => {
                *self.inner.merged.lock().unwrap() = true;
                *self.inner.merged_version.lock().unwrap() = Some(version);
                *self.inner.merged_identity.lock().unwrap() =
                    self.file(CANONICAL_PATH).map(|b| super::sha256_hex(&b));
                self.load_module("nvidia", 0, &[]);
            }
            None => {
                *self.inner.merged.lock().unwrap() = false;
                *self.inner.merged_version.lock().unwrap() = None;
            }
        }
    }

    /// The App container's own mount table (read through `/proc/self/mountinfo`); `mount` and
    /// `umount` never change it.
    pub fn container_mount_table(&self) -> Vec<MountLine> {
        self.inner.container_mounts.lock().unwrap().clone()
    }

    // ---- clock ---------------------------------------------------------

    pub fn advance(&self, d: Duration) {
        self.inner.clock.lock().unwrap().offset += d;
    }

    fn offset(&self) -> Duration {
        self.inner.clock.lock().unwrap().offset
    }

    pub fn set_wall_ms(&self, ms: u64) {
        self.inner.clock.lock().unwrap().wall_base_ms = ms;
    }

    // ---- modules -------------------------------------------------------

    pub fn load_module(&self, name: &str, refcount: u64, deps: &[&str]) {
        if name == "nvidia" && !self.inner.modules.lock().unwrap().contains_key(name) {
            let version = self
                .inner
                .merged_version
                .lock()
                .unwrap()
                .clone()
                .or_else(|| self.inner.driver_version.lock().unwrap().clone());
            let identity = self
                .inner
                .merged_identity
                .lock()
                .unwrap()
                .clone()
                .or_else(|| self.file(CANONICAL_PATH).map(|b| super::sha256_hex(&b)))
                .unwrap_or_default();
            *self.inner.loaded_text.lock().unwrap() = version.as_ref().map(|v| format!(
                "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  {v}  Release Build\nBuild identity: {identity}\n"
            ));
            *self.inner.loaded_version.lock().unwrap() = version;
        }
        self.inner.modules.lock().unwrap().insert(
            name.to_string(),
            facts::ModuleLine {
                name: name.to_string(),
                size: 1,
                refcount,
                deps: deps.iter().map(|d| d.to_string()).collect(),
            },
        );
    }

    pub fn unload_module(&self, name: &str) {
        self.inner.modules.lock().unwrap().remove(name);
        if name == "nvidia" {
            *self.inner.loaded_version.lock().unwrap() = None;
            *self.inner.loaded_text.lock().unwrap() = None;
        }
    }

    pub fn open_device(&self, name: &str) {
        let mut modules = self.inner.modules.lock().unwrap();
        if let Some(m) = modules.get_mut(name) {
            m.refcount += 1;
        }
    }

    /// Raise the displayed reference count for about `high`, then drop it for about `low`, repeating.
    /// `phase_at_zero` is the phase when the fake clock offset is zero. Busy while the phase is
    /// at most `high`.
    pub fn periodic_open(
        &self,
        module: &str,
        high: Duration,
        low: Duration,
        phase_at_zero: Duration,
    ) {
        self.inner
            .periodic_opens
            .lock()
            .unwrap()
            .push(PeriodicOpen {
                module: module.to_string(),
                high,
                low,
                phase_at_zero,
            });
    }

    /// Add one displayed reference while the fake offset is in `[starts_at, starts_at + duration)`.
    pub fn sustain_open(&self, module: &str, starts_at: Duration, duration: Duration) {
        self.inner
            .sustained_opens
            .lock()
            .unwrap()
            .push(SustainedOpen {
                module: module.to_string(),
                starts_at,
                duration,
            });
    }

    /// Advance the fake clock while generating `/proc/modules` for the `nth` time (1-based),
    /// before that reading's reference counts are computed.
    pub fn advance_on_proc_read(&self, nth: u64, by: Duration) {
        self.inner
            .proc_modules_advances
            .lock()
            .unwrap()
            .push((nth, by));
    }

    /// Run `hook` at the start of each `Host::wait`, before the fake clock moves.
    pub fn on_wait(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.inner.on_wait.lock().unwrap() = Some(Arc::new(hook));
    }

    /// Remove `module` while generating `/proc/modules` for the `nth` time (1-based),
    /// before that reading is rendered. Earlier readings still show the module.
    pub fn drop_module_on_proc_read(&self, module: &str, nth: u64) {
        self.inner
            .proc_modules_drops
            .lock()
            .unwrap()
            .push((module.to_string(), nth));
    }

    /// The first `rmmod` of `module` advances the fake clock by `delay` before the in-use decision.
    pub fn unload_takes(&self, module: &str, delay: Duration) {
        self.inner
            .unload_delays
            .lock()
            .unwrap()
            .insert(module.to_string(), delay);
    }

    /// Stored reference count plus any time-based hold at fake offset `at`. Does not move the clock.
    pub fn displayed_refcount_at(&self, module: &str, at: Duration) -> Option<u64> {
        let periodic = self.inner.periodic_opens.lock().unwrap().clone();
        let sustained = self.inner.sustained_opens.lock().unwrap().clone();
        let modules = self.inner.modules.lock().unwrap();
        let stored = modules.get(module)?;
        Some(stored.refcount + extra_refcount(at, module, &periodic, &sustained))
    }

    pub fn refuse_unload(&self, name: &str) {
        self.inner
            .refuse_unload
            .lock()
            .unwrap()
            .insert(name.to_string());
    }

    pub fn fail_reload(&self, name: &str) {
        self.inner
            .fail_reload
            .lock()
            .unwrap()
            .insert(name.to_string());
    }

    pub fn set_module_load_fail(&self, name: &str) {
        *self.inner.module_load_fail.lock().unwrap() = Some(name.to_string());
    }

    // ---- mounts, dataset, sysext --------------------------------------

    pub fn add_mount(&self, source: &str, mount_point: &str, fstype: &str, options: &str) {
        self.inner.host_mounts.lock().unwrap().push(MountLine {
            source: source.to_string(),
            mount_point: mount_point.to_string(),
            fstype: fstype.to_string(),
            options: options.to_string(),
        });
    }

    /// Add a line to the App container's own mount table only.
    pub fn add_container_mount(
        &self,
        source: &str,
        mount_point: &str,
        fstype: &str,
        options: &str,
    ) {
        self.inner.container_mounts.lock().unwrap().push(MountLine {
            source: source.to_string(),
            mount_point: mount_point.to_string(),
            fstype: fstype.to_string(),
            options: options.to_string(),
        });
    }

    pub fn remove_mount(&self, mount_point: &str) {
        self.inner
            .host_mounts
            .lock()
            .unwrap()
            .retain(|m| m.mount_point != mount_point);
    }

    pub fn readonly(&self) -> String {
        self.inner.readonly.lock().unwrap().clone()
    }

    pub fn sysext_since(&self) -> u64 {
        *self.inner.sysext_since.lock().unwrap()
    }

    pub fn model_sysext_unmerge(&self) {
        *self.inner.merged.lock().unwrap() = false;
        *self.inner.merged_version.lock().unwrap() = None;
    }

    pub fn model_sysext_merge(&self) {
        // A command may change host state even if its reported exit fails.
        if *self.inner.refresh_missing.lock().unwrap() {
            self.model_sysext_unmerge();
            return;
        }
        let target = self
            .link_target("/run/extensions/nvidia.raw")
            .filter(|target| self.virtual_exists(target));
        match target {
            Some(target) => {
                *self.inner.merged.lock().unwrap() = true;
                *self.inner.merged_version.lock().unwrap() = self.listing_version(&target);
                *self.inner.merged_identity.lock().unwrap() =
                    self.file(&target).map(|b| super::sha256_hex(&b));
                *self.inner.sysext_since.lock().unwrap() += 1;
            }
            None => self.model_sysext_unmerge(),
        }
    }

    pub fn set_refresh_missing(&self, missing: bool) {
        *self.inner.refresh_missing.lock().unwrap() = missing;
    }

    pub fn set_driver_version(&self, version: &str) {
        *self.inner.driver_version.lock().unwrap() = Some(version.to_string());
        // An explicit test override changes the kernel reading, independently of refresh.
        if self.inner.modules.lock().unwrap().contains_key("nvidia") {
            *self.inner.loaded_version.lock().unwrap() = Some(version.to_string());
            *self.inner.loaded_text.lock().unwrap() = Some(format!(
                "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  {version}  Release Build\n"
            ));
        }
    }

    // ---- gpus ----------------------------------------------------------

    pub fn add_gpu(&self, address: &str, device: u16, driver: Option<&str>) {
        let base = format!("/sys/bus/pci/devices/{address}");
        self.mkdir(&base);
        self.put_text(&format!("{base}/vendor"), "0x10de\n");
        self.put_text(&format!("{base}/device"), &format!("0x{device:04x}\n"));
        self.put_text(&format!("{base}/class"), "0x030000\n");
        if let Some(driver) = driver {
            self.put_link(
                &format!("{base}/driver"),
                &format!("/sys/bus/pci/drivers/{driver}"),
            );
        }
    }

    pub fn set_gpu_driver(&self, address: &str, driver: Option<&str>) {
        let path = format!("/sys/bus/pci/devices/{address}/driver");
        match driver {
            Some(driver) => self.put_link(&path, &format!("/sys/bus/pci/drivers/{driver}")),
            None => self.remove(&path),
        }
    }

    pub fn add_drm_node(&self, node: &str, driver: &str) {
        let base = format!("/sys/class/drm/{node}");
        self.put_link(
            &format!("{base}/device/driver"),
            &format!("/sys/bus/pci/drivers/{driver}"),
        );
    }

    // ---- units ---------------------------------------------------------

    pub fn unit_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.units.lock().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Drop every preset (including gates) and the command log (scenario reset).
    pub fn clear_presets_and_commands(&self) {
        self.inner.presets.lock().unwrap().clear();
        self.inner.commands.lock().unwrap().clear();
    }

    /// Remove every tracked unit (scenario reset).
    pub fn clear_units(&self) {
        self.inner.units.lock().unwrap().clear();
    }

    /// Install a unit state directly (for killed-runner scenarios).
    pub fn set_unit(&self, id: &str, active: &str, sub: &str, result: &str, exec_main_status: i32) {
        let mut units = self.inner.units.lock().unwrap();
        units.insert(
            id.to_string(),
            UnitState {
                active: active.into(),
                sub: sub.into(),
                result: result.into(),
                exec_main_status,
            },
        );
    }

    /// Model the runner being killed: the unit becomes failed and the runner thread stops at its
    /// next host boundary, without running another command or writing anything.
    pub fn kill_unit(&self, id: &str) {
        {
            let mut units = self.inner.units.lock().unwrap();
            if let Some(u) = units.get_mut(id) {
                u.active = "failed".into();
                u.sub = "failed".into();
                u.result = "signal".into();
                u.exec_main_status = 0;
            }
        }
        if let Some(kill) = self.inner.kills.lock().unwrap().get(id) {
            kill.store(true, Ordering::SeqCst);
        }
    }

    /// True when the current thread runs a killed `systemd-run` runner.
    fn killed(&self) -> bool {
        RUNNER_KILL.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|kill| kill.load(Ordering::SeqCst))
                .unwrap_or(false)
        })
    }

    /// The execution boundary every runner command crosses: a killed runner stops here instead of
    /// continuing.
    fn check_killed(&self) {
        if self.killed() {
            stop_killed_runner();
        }
    }

    /// Stop every runner thread this fixture started (`systemd-run`) and wait until each one has
    /// really left its host boundaries. `clear_units` only clears the map: the thread keeps
    /// running, keeps writing state and keeps issuing commands, so a scenario reset that reuses the
    /// namespace must stop and join them first. Returns `false` when a thread did not stop inside
    /// `timeout`; the handles stay queued for a later wait.
    pub fn stop_runners_and_wait(&self, timeout: Duration) -> bool {
        let switches: Vec<Arc<AtomicBool>> =
            self.inner.kills.lock().unwrap().values().cloned().collect();
        for kill in &switches {
            kill.store(true, Ordering::SeqCst);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let jobs: Vec<std::thread::JoinHandle<()>> =
                self.inner.jobs.lock().unwrap().drain(..).collect();
            if jobs.is_empty() {
                // The map is empty only after every thread removed its own kill switch, so no
                // runner is still inside a host boundary.
                return true;
            }
            for job in jobs {
                if job.is_finished() {
                    let _ = job.join();
                    continue;
                }
                // Not finished yet: wait a tick and look again, so the join itself never blocks
                // past the deadline.
                let mut pending = Some(job);
                let mut stopped = false;
                while let Some(handle) = pending.take() {
                    if handle.is_finished() {
                        let _ = handle.join();
                        stopped = true;
                        break;
                    }
                    if Instant::now() >= deadline {
                        self.inner.jobs.lock().unwrap().push(handle);
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                    pending = Some(handle);
                }
                if stopped {
                    continue;
                }
            }
            if Instant::now() >= deadline {
                return self.inner.jobs.lock().unwrap().is_empty();
            }
        }
    }

    pub fn wait_for_units(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let jobs: Vec<std::thread::JoinHandle<()>> =
                self.inner.jobs.lock().unwrap().drain(..).collect();
            if jobs.is_empty() {
                return;
            }
            for job in jobs {
                let _ = job.join();
            }
            if Instant::now() > deadline {
                return;
            }
        }
    }

    // ---- presets for readers ------------------------------------------

    pub fn set_listing(&self, path: &str, listing: &str) {
        self.inner
            .listings
            .lock()
            .unwrap()
            .insert(path.to_string(), listing.to_string());
    }

    /// Make every `create` under this path fail.
    pub fn fail_write(&self, path: &str) {
        let path = FakeHost::translate(path);
        self.inner.fail_writes.lock().unwrap().push(path);
    }

    /// Make `fsync_dir` fail for this path (the directory-sync failure tests).
    pub fn fail_fsync_dir(&self, path: &str) {
        let path = FakeHost::translate(path);
        self.inner.fail_fsync_dirs.lock().unwrap().push(path);
    }

    /// Make every `rename` whose source path ends with `suffix` fail.
    pub fn fail_rename_suffix(&self, suffix: &str) {
        *self.inner.fail_rename_suffix.lock().unwrap() = Some(suffix.to_string());
    }

    /// Make every `rename` whose target path ends with `suffix` fail.
    pub fn fail_rename_to_suffix(&self, suffix: &str) {
        self.inner
            .fail_rename_to
            .lock()
            .unwrap()
            .push((suffix.to_string(), 0, 0));
    }

    /// Let the first `allowed` renames whose target ends with `suffix` pass, then fail the rest
    /// (the publish rename succeeds, every rollback rename to the same target is refused).
    pub fn fail_rename_to_suffix_after(&self, suffix: &str, allowed: usize) {
        self.inner
            .fail_rename_to
            .lock()
            .unwrap()
            .push((suffix.to_string(), allowed, 0));
    }

    /// Make renames that put a temporary `.<target>.tmp-…` file back onto `target` fail; the first
    /// publish or replacement rename is unaffected.
    pub fn fail_rename_rollback(&self, target: &str) {
        self.inner
            .fail_rename_rollback
            .lock()
            .unwrap()
            .push(target.to_string());
    }

    /// Clear every injected write, read, fsync, rename and lock failure.
    pub fn clear_failures(&self) {
        self.inner.fail_writes.lock().unwrap().clear();
        self.inner.fail_fsync_dirs.lock().unwrap().clear();
        *self.inner.fail_rename_suffix.lock().unwrap() = None;
        self.inner.fail_rename_to.lock().unwrap().clear();
        self.inner.fail_rename_rollback.lock().unwrap().clear();
        self.inner.fail_reads.lock().unwrap().clear();
        self.inner.fail_removes.lock().unwrap().clear();
        self.inner.fail_rename_contains.lock().unwrap().clear();
        self.inner.fail_syncs.lock().unwrap().clear();
        self.inner.read_changes.lock().unwrap().clear();
        self.inner.fail_rename_exdev.lock().unwrap().clear();
        *self.inner.lock_open_fails.lock().unwrap() = false;
        *self.inner.lock_acquire_fails.lock().unwrap() = false;
        *self.inner.lock_mode.lock().unwrap() = None;
    }

    pub fn set_unsquashfs_broken(&self, path: &str) {
        self.inner
            .unsquashfs_fails
            .lock()
            .unwrap()
            .insert(path.to_string());
    }

    pub fn set_modinfo(&self, path: &str, version: &str) {
        self.inner
            .modinfo
            .lock()
            .unwrap()
            .insert(path.to_string(), version.to_string());
    }

    pub fn set_dpkg(&self, name: &str, fields: &str) {
        self.inner
            .dpkg
            .lock()
            .unwrap()
            .insert(name.to_string(), fields.to_string());
    }

    pub fn set_midclt(&self, args: &str, output: FakeOutput) {
        self.inner
            .midclt
            .lock()
            .unwrap()
            .insert(args.to_string(), output);
    }

    pub fn set_curl(&self, url: &str, status: u16, body: &[u8]) {
        self.inner.curl.lock().unwrap().insert(
            url.to_string(),
            CurlResponse {
                status,
                body: body.to_vec(),
                exit: 0,
                stderr: String::new(),
                partial: Vec::new(),
                location: None,
            },
        );
    }

    /// A 3xx response with a `Location`. `curl` follows it only when the command carries
    /// `--location` / `-L`, and only as far as `--max-redirs` and the `--proto` / `--proto-redir`
    /// policy allow. Without `--location` the status and this body are the whole response.
    pub fn set_curl_redirect(&self, url: &str, status: u16, location: &str, body: &[u8]) {
        self.inner.curl.lock().unwrap().insert(
            url.to_string(),
            CurlResponse {
                status,
                body: body.to_vec(),
                exit: 0,
                stderr: String::new(),
                partial: Vec::new(),
                location: Some(location.to_string()),
            },
        );
    }

    pub fn set_curl_failure(&self, url: &str, exit: i32, stderr: &str) {
        self.inner.curl.lock().unwrap().insert(
            url.to_string(),
            CurlResponse {
                status: 0,
                body: Vec::new(),
                exit,
                stderr: stderr.to_string(),
                partial: Vec::new(),
                location: None,
            },
        );
    }

    /// A failing `curl` that already wrote `partial` bytes to its `--output` path, like a real
    /// transfer interrupted mid-body.
    pub fn set_curl_partial_failure(&self, url: &str, partial: &[u8], exit: i32, stderr: &str) {
        self.inner.curl.lock().unwrap().insert(
            url.to_string(),
            CurlResponse {
                status: 0,
                body: Vec::new(),
                exit,
                stderr: stderr.to_string(),
                partial: partial.to_vec(),
                location: None,
            },
        );
    }

    pub fn set_docker_ps(&self, ids: &[&str]) {
        *self.inner.docker_ps.lock().unwrap() = ids.iter().map(|s| s.to_string()).collect();
    }

    pub fn set_docker_inspect(&self, json: &str) {
        *self.inner.docker_inspect.lock().unwrap() = json.as_bytes().to_vec();
    }

    pub fn set_docker_inspect_fails(&self, fails: bool) {
        *self.inner.docker_inspect_fails.lock().unwrap() = fails;
    }

    /// Make one image local under this reference with this local image ID (the "already on the
    /// host" case of the pinned build environment).
    pub fn set_image(&self, reference: &str, id: &str) {
        self.inner
            .images
            .lock()
            .unwrap()
            .insert(reference.to_string(), id.to_string());
    }

    /// Every modelled local image as `(reference, local image ID)`.
    pub fn images(&self) -> Vec<(String, String)> {
        self.inner
            .images
            .lock()
            .unwrap()
            .iter()
            .map(|(reference, id)| (reference.clone(), id.clone()))
            .collect()
    }

    /// Give every runner this host starts an already prepared build environment instead of the
    /// pinned one: the fake host has no official image and no real packages, so a task that
    /// compiles would otherwise stop at the environment preparation. The pinned environment's own
    /// cases live in `tests/build_env.rs`.
    pub fn prepare_build_env(&self) {
        *self.inner.build_env.lock().unwrap() = Some(BuildEnv::new(
            truenas_nvidia_driver_manager::build_kit::BUILD_ENV_IMAGE_ID.to_string(),
            false,
            vec!["/build-kit/binutils_2.40-2_amd64.deb".to_string()],
        ));
    }

    /// The prepared build environment this host hands to a started runner, if any: a test that
    /// calls `runner::run_with` itself uses the same one the fake `systemd-run` would use.
    pub fn build_env(&self) -> Option<BuildEnv> {
        self.inner.build_env.lock().unwrap().clone()
    }

    /// Make `docker pull` fail (no network, a refused registry).
    pub fn set_docker_pull_fails(&self, fails: bool) {
        *self.inner.docker_pull_fails.lock().unwrap() = fails;
    }

    /// The local image ID a successful `docker pull` reports.
    pub fn set_docker_pull_id(&self, id: &str) {
        *self.inner.docker_pull_id.lock().unwrap() = id.to_string();
    }

    /// Make a successful `docker pull` leave nothing local: it still prints the modelled ID, but
    /// the image is not there afterwards, so the check after the pull finds nothing.
    pub fn set_docker_pull_leaves_nothing(&self, leaves_nothing: bool) {
        *self.inner.docker_pull_leaves_nothing.lock().unwrap() = leaves_nothing;
    }

    /// Make `docker image rm` fail; the image stays in the model.
    pub fn set_docker_image_rm_fails(&self, fails: bool) {
        *self.inner.docker_image_rm_fails.lock().unwrap() = fails;
    }

    /// True when the model holds a container with this name, running or not.
    pub fn has_build_container(&self, name: &str) -> bool {
        self.inner.containers.lock().unwrap().contains_key(name)
    }

    /// Install one modelled container directly, running or already exited (the stopped and orphan
    /// containers of the drain model).
    pub fn add_container(&self, name: &str, running: bool) {
        self.inner
            .containers
            .lock()
            .unwrap()
            .insert(name.to_string(), running);
    }

    /// Every modelled container name, sorted, running or not.
    pub fn container_names(&self) -> Vec<String> {
        self.inner
            .containers
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    /// Make `docker ps -a --format {{.Names}}` fail: a non-zero exit, or `Exit::TimedOut` when
    /// [`FakeHost::set_docker_ps_all_times_out`] is set.
    pub fn set_docker_ps_all_fails(&self, fails: bool) {
        *self.inner.docker_ps_all_fails.lock().unwrap() = fails;
    }

    /// Turn the `docker ps -a --format {{.Names}}` failure into a timeout.
    pub fn set_docker_ps_all_times_out(&self, times_out: bool) {
        *self.inner.docker_ps_all_times_out.lock().unwrap() = times_out;
    }

    /// Let the failing `docker ps -a --format {{.Names}}` write one truncated name line before it
    /// fails, like a listing cut off mid-line.
    pub fn set_docker_ps_all_partial(&self, partial: bool) {
        *self.inner.docker_ps_all_partial.lock().unwrap() = partial;
    }

    /// Make `docker rm` report success without removing the container from the model (the silent
    /// no-op the cleanup must not trust).
    pub fn set_docker_rm_noop(&self, noop: bool) {
        *self.inner.docker_rm_noop.lock().unwrap() = noop;
    }

    /// Make `docker rm` return a non-zero exit; the container stays in the model.
    pub fn set_docker_rm_fails(&self, fails: bool) {
        *self.inner.docker_rm_fails.lock().unwrap() = fails;
    }

    /// Make `docker rm` return a non-zero exit **and** remove the container from the model: the
    /// exact listing is then the only proof of absence.
    pub fn set_docker_rm_fails_after_removal(&self, fails: bool) {
        *self.inner.docker_rm_fails.lock().unwrap() = fails;
        *self.inner.docker_rm_fails_after_removal.lock().unwrap() = fails;
    }

    /// Install the lines `systemctl list-jobs` answers with, one job per line.
    pub fn set_jobs(&self, jobs: &[&str]) {
        *self.inner.job_lines.lock().unwrap() = jobs.iter().map(|j| j.to_string()).collect();
    }

    /// Make `systemctl list-units` fail with a non-zero exit and a reason.
    pub fn set_list_units_fails(&self, fails: bool) {
        *self.inner.list_units_fails.lock().unwrap() = fails;
    }

    /// Make the host mount table unreadable (the unclassifiable mount query).
    pub fn set_mount_table_fails(&self, fails: bool) {
        *self.inner.mount_table_fails.lock().unwrap() = fails;
    }

    /// Make every listing of this path fail with EACCES (never "empty").
    pub fn fail_read_dir(&self, path: &str) {
        let path = FakeHost::translate(path);
        self.inner.fail_read_dirs.lock().unwrap().push(path);
    }

    /// Model a container process: `view` maps host paths into this prefix, and `open_lock` reaches
    /// the host through it, exactly like the container mode of `RealHost`. The ordinary file
    /// interface keeps using the path it is given.
    pub fn set_view_prefix(&self, prefix: Option<&str>) {
        *self.inner.view_prefix.lock().unwrap() = prefix.map(str::to_string);
    }

    /// Make `systemctl list-jobs` fail with a non-zero exit, or with `Exit::TimedOut` when
    /// [`FakeHost::set_list_jobs_times_out`] is set.
    pub fn set_list_jobs_fails(&self, fails: bool) {
        *self.inner.list_jobs_fails.lock().unwrap() = fails;
    }

    /// Turn the `systemctl list-jobs` failure into a timeout.
    pub fn set_list_jobs_times_out(&self, times_out: bool) {
        *self.inner.list_jobs_times_out.lock().unwrap() = times_out;
    }

    /// Let the failing `systemctl list-jobs` write one truncated job line before it fails.
    pub fn set_list_jobs_partial(&self, partial: bool) {
        *self.inner.list_jobs_partial.lock().unwrap() = partial;
    }

    pub fn set_exe(&self, bytes: &[u8]) {
        *self.inner.exe.lock().unwrap() = bytes.to_vec();
    }

    pub fn set_uname(&self, release: &str) {
        *self.inner.uname.lock().unwrap() = release.to_string();
    }

    pub fn set_hostname_container_id(&self, id: &str) {
        *self.inner.hostname_id.lock().unwrap() = id.to_string();
    }

    // ---- the cross-process lock model ----------------------------------

    /// Make `open_lock` fail for every path (the unopenable lock file).
    pub fn set_lock_open_fails(&self, fails: bool) {
        *self.inner.lock_open_fails.lock().unwrap() = fails;
    }

    /// Make `try_acquire` of every handle fail with an I/O error, which is never "free" (the
    /// unclassifiable lock query).
    pub fn set_lock_acquire_fails(&self, fails: bool) {
        *self.inner.lock_acquire_fails.lock().unwrap() = fails;
    }

    /// Give an existing fixture directory this exact mode, so a test can model a directory the
    /// production mode validation must refuse (a permissive `manager/control`, for instance). The
    /// file sandbox keeps the real node: the product reads the mode through `lstat`.
    #[cfg(unix)]
    pub fn set_dir_mode(&self, path: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let real = self
            .real_path(path)
            .unwrap_or_else(|e| panic!("fake host set_dir_mode refuses {path}: {e}"));
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(mode))
            .expect("fake host set_dir_mode");
    }

    /// Block the next operation below `prefix` at an observable product boundary. This can hold
    /// the state-write phase before the task directory exists, inside the product's own lock.
    pub fn set_path_gate(&self, prefix: &str) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        self.inner
            .path_gate
            .lock()
            .unwrap()
            .push((prefix.to_string(), gate.clone()));
        gate
    }

    /// Wait on the path gate registered for `path`, if any.
    fn wait_path_gate(&self, path: &str) {
        let gates: Vec<Arc<Gate>> = {
            let gates = self.inner.path_gate.lock().unwrap();
            gates
                .iter()
                .filter(|(prefix, _)| path.starts_with(prefix))
                .map(|(_, gate)| gate.clone())
                .collect()
        };
        for gate in gates {
            gate.wait_observed();
        }
    }

    /// Create `marker` the first time this host reads a path below `prefix`. Used by the
    /// cross-process cases to observe a read that happens while the reader holds a lock (test
    /// handshake only, the read uses the string reason).
    pub fn on_read_marker(&self, prefix: &str, marker: std::path::PathBuf) {
        self.inner
            .read_markers
            .lock()
            .unwrap()
            .push((prefix.to_string(), marker));
    }

    pub fn use_real_flock(&self, on: bool) {
        self.inner.real_flock.store(on, Ordering::SeqCst);
    }

    pub fn set_lock_mode(&self, mode: u32) {
        *self.inner.lock_mode.lock().unwrap() = Some(mode);
    }

    /// True while some open handle of this fake host holds the modelled lock of `host_path` (a
    /// container or host view of the same object).
    pub fn lock_held(&self, host_path: &str) -> bool {
        self.inner
            .locks_held
            .lock()
            .unwrap()
            .contains(&lock_key(host_path))
    }

    // ---- command dispatch ---------------------------------------------

    fn record(&self, cmd: &Cmd) -> CommandRecord {
        let mut record = CommandRecord {
            program: cmd.program.to_string(),
            args: cmd
                .args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            at: cmd.at,
            timeout: cmd.timeout,
            env: BTreeMap::new(),
        };
        if cmd.at == At::Host {
            record.env.insert("LC_ALL".into(), "C".into());
            record
                .env
                .insert("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into());
        }
        if record.program == "docker" && record.args.first().is_some_and(|a| a == "run") {
            if let Some(name) = record.args.iter().find_map(|a| a.strip_prefix("--name=")) {
                // The container exists from the moment `docker run` starts, no matter how the
                // command later ends.
                self.inner
                    .containers
                    .lock()
                    .unwrap()
                    .insert(name.into(), true);
            }
        }
        self.inner.commands.lock().unwrap().push(record.clone());
        record
    }

    /// The container model follows a finished `docker` command: a successful `rm` removes the
    /// container, a failed one or an injected `rm` no-op leaves it in place.
    fn finish_docker_command(&self, cmd: &Cmd, out: &Output) {
        if cmd.program != "docker" {
            return;
        }
        let args = arg_strings(cmd);
        if args.first().is_some_and(|a| a == "rm") {
            // A removal may report a failure while the container really is gone; the mocked model
            // must be able to express that so the listing is exercised as the only proof.
            let gone = out.exit.ok() || *self.inner.docker_rm_fails_after_removal.lock().unwrap();
            let noop = *self.inner.docker_rm_noop.lock().unwrap();
            if gone && !noop {
                if let Some(name) = args.last() {
                    self.inner.containers.lock().unwrap().remove(name);
                }
            }
        }
    }

    fn matched_preset(&self, cmd: &Cmd) -> Option<Preset> {
        let mut presets = self.inner.presets.lock().unwrap();
        let args = arg_strings(cmd);
        let mut index = None;
        for (i, p) in presets.iter().enumerate() {
            if p.program != cmd.program {
                continue;
            }
            if p.args.len() <= args.len() && p.args.iter().zip(args.iter()).all(|(a, b)| a == b) {
                index = Some(i);
            }
        }
        let i = index?;
        let p = presets[i].clone();
        if let Some(1) = p.repeat {
            presets.remove(i);
        }
        Some(p)
    }

    fn builtin(&self, cmd: &Cmd) -> Option<FakeOutput> {
        match cmd.program {
            "systemd-run" => Some(self.systemd_run(cmd)),
            "systemctl" => Some(self.systemctl(cmd)),
            "docker" => Some(self.docker(cmd)),
            "zfs" => Some(self.zfs(cmd)),
            "mount" => {
                let args = arg_strings(cmd);
                if let Some(pos) = args.iter().position(|a| a == "-o") {
                    let opts = args.get(pos + 1).cloned().unwrap_or_default();
                    let source = args.iter().rev().nth(1).cloned().unwrap_or_default();
                    let mount_point = args.last().cloned().unwrap_or_default();
                    self.add_mount(&source, &mount_point, "zfs", &opts);
                    if mount_point == RW_MOUNT {
                        self.mkdir(&format!("{RW_MOUNT}/share/truenas/sysext-extensions"));
                        if let Some(bytes) = self.file(CANONICAL_PATH) {
                            self.put_file(RW_CANONICAL_PATH, bytes);
                        }
                    }
                    return Some(FakeOutput::code(0));
                }
                Some(FakeOutput::code(32).with_stderr("mount: unsupported in the fake host"))
            }
            "mv" => {
                let args = arg_strings(cmd);
                if args != ["-T", RW_TMP_PATH, RW_CANONICAL_PATH]
                    || self.readonly() != "off"
                    || !self
                        .inner
                        .host_mounts
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|line| line.mount_point == RW_MOUNT && line.is_rw())
                {
                    return Some(FakeOutput::code(1).with_stderr("mv: writable mount unavailable"));
                }
                let Some(bytes) = self.file(RW_TMP_PATH) else {
                    return Some(FakeOutput::code(1).with_stderr("mv: source missing"));
                };
                let listings = self.inner.listings.lock().unwrap().clone();
                let listing = listings.iter().find_map(|(path, listing)| {
                    (self.file(path).as_deref() == Some(bytes.as_slice())).then(|| listing.clone())
                });
                let (Ok(src), Ok(dst)) = (
                    self.real_path_nofollow(RW_TMP_PATH),
                    self.real_path_nofollow(RW_CANONICAL_PATH),
                ) else {
                    return Some(FakeOutput::code(1).with_stderr("mv: invalid path"));
                };
                if let Err(error) = std::fs::rename(&src, &dst) {
                    return Some(FakeOutput::code(1).with_stderr(&format!("mv: {error}")));
                }
                // The writable mount and /usr name the same ZFS file. Replace its inode so a
                // descriptor opened before mv keeps old bytes.
                let Ok(canonical) = self.real_path_nofollow(CANONICAL_PATH) else {
                    return Some(FakeOutput::code(1).with_stderr("mv: invalid canonical path"));
                };
                if let Err(error) = std::fs::rename(&dst, &canonical) {
                    return Some(FakeOutput::code(1).with_stderr(&format!("mv: {error}")));
                }
                if let Err(error) = std::fs::hard_link(&canonical, &dst) {
                    return Some(FakeOutput::code(1).with_stderr(&format!("mv: {error}")));
                }
                let mut stored = self.inner.listings.lock().unwrap();
                if let Some(listing) = listing {
                    stored.insert(CANONICAL_PATH.into(), listing);
                } else {
                    stored.remove(CANONICAL_PATH);
                }
                Some(FakeOutput::code(0))
            }
            "umount" => {
                let mount_point = cmd
                    .args
                    .last()
                    .map(|a| a.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.remove_mount(&mount_point);
                Some(FakeOutput::code(0))
            }
            "modprobe" => {
                let name = cmd
                    .args
                    .last()
                    .map(|a| a.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if self.inner.modules.lock().unwrap().contains_key(&name) {
                    return Some(FakeOutput::code(0));
                }
                if name != "nvidia"
                    && self.driver_reading().is_some_and(|v| {
                        self.inner
                            .merged_version
                            .lock()
                            .unwrap()
                            .as_ref()
                            .is_some_and(|m| m != &v)
                    })
                {
                    return Some(
                        FakeOutput::code(1)
                            .with_stderr("modprobe: Invalid argument (version mismatch)"),
                    );
                }
                if self.inner.fail_reload.lock().unwrap().contains(&name) {
                    return Some(
                        FakeOutput::code(1)
                            .with_stderr(&format!("modprobe: ERROR: could not insert {name}")),
                    );
                }
                if self.inner.module_load_fail.lock().unwrap().as_deref() == Some(name.as_str()) {
                    return Some(FakeOutput::code(1).with_stderr(&format!(
                        "modprobe: ERROR: could not insert {name}: No such device"
                    )));
                }
                if !self.inner.modules.lock().unwrap().contains_key(&name) {
                    self.load_module(&name, 0, &[]);
                }
                Some(FakeOutput::code(0))
            }
            "rmmod" => {
                let name = cmd
                    .args
                    .last()
                    .map(|a| a.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let delay = self.inner.unload_delays.lock().unwrap().remove(&name);
                if let Some(delay) = delay {
                    self.advance(delay);
                }
                let correction = self
                    .persisted_phase()
                    .is_some_and(|p| p.ends_with("align_modules"));
                if self.inner.refuse_unload.lock().unwrap().contains(&name)
                    || (correction && self.inner.refuse_correction.lock().unwrap().contains(&name))
                {
                    return Some(FakeOutput::code(1).with_stderr(&format!(
                        "rmmod: ERROR: Module {name} is in use by: holder"
                    )));
                }
                let at = self.offset();
                let periodic = self.inner.periodic_opens.lock().unwrap().clone();
                let sustained = self.inner.sustained_opens.lock().unwrap().clone();
                {
                    let modules = self.inner.modules.lock().unwrap();
                    let Some(module) = modules.get(&name) else {
                        return Some(FakeOutput::code(1).with_stderr(&format!(
                            "rmmod: ERROR: Module {name} not currently loaded"
                        )));
                    };
                    // An open device raises the reference count; a loaded module in the holder list
                    // (`/proc/modules` dependencies) refuses the unload. A time-based opener adds
                    // the same extra count `/proc/modules` displays.
                    let refcount =
                        module.refcount + extra_refcount(at, &name, &periodic, &sustained);
                    if refcount > 0 {
                        return Some(
                            FakeOutput::code(1)
                                .with_stderr(&format!("rmmod: ERROR: Module {name} is in use")),
                        );
                    }
                    let holders: Vec<String> = module
                        .deps
                        .iter()
                        .filter(|d| modules.contains_key(d.as_str()))
                        .cloned()
                        .collect();
                    if !holders.is_empty() {
                        return Some(FakeOutput::code(1).with_stderr(&format!(
                            "rmmod: ERROR: Module {name} is in use by: {}",
                            holders.join(",")
                        )));
                    }
                }
                self.unload_module(&name);
                Some(FakeOutput::code(0))
            }
            "unsquashfs" => {
                let args = arg_strings(cmd);
                if let Some(pos) = args.iter().position(|a| a == "-d") {
                    let dir = args.get(pos + 1).cloned().unwrap_or_default();
                    let member = args.last().cloned().unwrap_or_default();
                    let target = format!("{dir}/{member}");
                    self.put_text(&target, "fake nvidia.ko");
                    return Some(FakeOutput::code(0));
                }
                let path = args.last().cloned().unwrap_or_default();
                if self.inner.unsquashfs_fails.lock().unwrap().contains(&path) {
                    return Some(FakeOutput::code(1).with_stderr("unsquashfs: failed"));
                }
                let listing = self.inner.listings.lock().unwrap().get(&path).cloned();
                match listing {
                    Some(l) => Some(FakeOutput::stdout(&l)),
                    None if path.ends_with("driver.run") => Some(FakeOutput {
                        exit: Exit::Code(1),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        delay: Duration::ZERO,
                    }),
                    None => Some(FakeOutput::code(1).with_stderr("unsquashfs: no such file")),
                }
            }
            "modinfo" => {
                let args = arg_strings(cmd);
                let path = args.last().cloned().unwrap_or_default();
                match self.inner.modinfo.lock().unwrap().get(&path).cloned() {
                    Some(v) => Some(FakeOutput::stdout(&format!("{v}\n"))),
                    None => Some(
                        FakeOutput::code(1).with_stderr("modinfo: ERROR: could not find module"),
                    ),
                }
            }
            "dpkg-deb" => {
                if *self.inner.dpkg_passthrough.lock().unwrap() {
                    return Some(self.real_dpkg(cmd));
                }
                let args = arg_strings(cmd);
                let path = args.get(1).cloned().unwrap_or_default();
                if *self.inner.dpkg_fixture_content.lock().unwrap() {
                    let fields = self
                        .read(Path::new(&path))
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .and_then(|text| text.strip_prefix("NVM_TEST_DEB\n").map(str::to_owned))
                        .filter(|fields| {
                            let lines: Vec<_> = fields.lines().collect();
                            lines.len() == 3
                                && lines[0].starts_with("Package: ")
                                && lines[1].starts_with("Version: ")
                                && lines[2].starts_with("Architecture: ")
                        });
                    return Some(match fields {
                        Some(fields) => FakeOutput::stdout(&fields),
                        None => {
                            FakeOutput::code(1).with_stderr("invalid synthetic package fixture")
                        }
                    });
                }
                match self.inner.dpkg.lock().unwrap().get(&path).cloned() {
                    Some(f) => Some(FakeOutput::stdout(&f)),
                    None => match self.inner.dpkg_default.lock().unwrap().clone() {
                        Some(f) => Some(FakeOutput::stdout(&f)),
                        None => Some(FakeOutput::code(1).with_stderr("dpkg-deb: error")),
                    },
                }
            }
            "midclt" => {
                let args = arg_strings(cmd);
                let key = args.join(" ");
                let map = self.inner.midclt.lock().unwrap();
                for (pattern, out) in map.iter().rev() {
                    if key == *pattern || key.starts_with(&format!("{pattern} ")) {
                        return Some(out.clone());
                    }
                }
                // The default fixture exposes virt; callers can override container.* for 27.
                if key.starts_with("call container.") {
                    Some(FakeOutput::code(1).with_stderr("[ENOMETHOD] Method does not exist"))
                } else {
                    Some(FakeOutput::stdout("[]"))
                }
            }
            "curl" => Some(self.curl(cmd)),
            "nvidia-smi"
                if arg_strings(cmd).first().map(String::as_str)
                    == Some("--query-gpu=name,pci.bus_id") =>
            {
                if self.driver_reading().is_some_and(|v| {
                    self.inner
                        .merged_version
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|m| m != &v)
                }) {
                    Some(
                        FakeOutput::code(18).with_stderr(
                            "Failed to initialize NVML: Driver/library version mismatch",
                        ),
                    )
                } else {
                    Some(FakeOutput::stdout("NVIDIA Example GPU, 00000000:01:00.0\n"))
                }
            }
            "nvidia-smi" => {
                match self.driver_reading() {
                    Some(version)
                        if self
                            .inner
                            .merged_version
                            .lock()
                            .unwrap()
                            .as_ref()
                            .is_some_and(|m| m != &version) =>
                    {
                        Some(FakeOutput::code(18).with_stderr(
                            "Failed to initialize NVML: Driver/library version mismatch",
                        ))
                    }
                    Some(version) => Some(FakeOutput::stdout(&format!("{version}\n"))),
                    None => Some(FakeOutput::code(9).with_stderr("nvidia-smi: no devices")),
                }
            }
            "systemd-sysext" => {
                let args = arg_strings(cmd);
                match args.first().map(|s| s.as_str()) {
                    Some("status") => {
                        let merged = *self.inner.merged.lock().unwrap()
                            && !*self.inner.refresh_missing.lock().unwrap();
                        if merged {
                            let since = *self.inner.sysext_since.lock().unwrap();
                            Some(FakeOutput::stdout(&format!(
                                "[{{\"hierarchy\":\"/usr\",\"extensions\":[\"nvidia\"],\"since\":{since}}}]\n"
                            )))
                        } else {
                            Some(FakeOutput::stdout(
                                "[{\"hierarchy\":\"/usr\",\"extensions\":\"none\",\"since\":null}]\n",
                            ))
                        }
                    }
                    Some("unmerge") => {
                        self.model_sysext_unmerge();
                        Some(FakeOutput::code(0))
                    }
                    Some("merge" | "refresh") => {
                        // Merge reads what the link target holds: a missing link or target drops
                        // the `/usr` extension; a target with a readable listing decides the loaded
                        // version and gets a new SINCE.
                        self.model_sysext_merge();
                        Some(FakeOutput::code(0))
                    }
                    _ => Some(FakeOutput::code(0)),
                }
            }
            "sleep" => {
                self.advance(Duration::from_secs(2));
                Some(FakeOutput::code(0))
            }
            "ldconfig" => Some(FakeOutput::code(0)),
            "cat" => {
                let path = cmd
                    .args
                    .last()
                    .map(|a| a.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match self.file(&path) {
                    Some(bytes) => Some(FakeOutput {
                        exit: Exit::Code(0),
                        stdout: bytes,
                        stderr: Vec::new(),
                        delay: Duration::ZERO,
                    }),
                    None => Some(FakeOutput::code(1).with_stderr("cat: No such file or directory")),
                }
            }
            _ => None,
        }
    }

    fn systemd_run(&self, cmd: &Cmd) -> FakeOutput {
        let args = arg_strings(cmd);
        let unit_arg = args
            .iter()
            .find(|a| a.starts_with("--unit="))
            .cloned()
            .unwrap_or_default();
        let unit = unit_arg.trim_start_matches("--unit=").to_string();
        let Some(id) = unit
            .strip_prefix("nvm-task-")
            .and_then(|r| r.strip_suffix(".service"))
        else {
            return FakeOutput::code(1).with_stderr("systemd-run: bad unit name");
        };
        // The argv after `--` is the runner path, `task run <id>`.
        let runner_path = args
            .iter()
            .position(|a| a == "--")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default();
        let Some(data_dir) = runner_path.strip_suffix("/manager/bin/truenas-nvidia-driver-manager")
        else {
            return FakeOutput::code(1).with_stderr("systemd-run: runner path not found");
        };
        {
            let mut units = self.inner.units.lock().unwrap();
            units.insert(
                id.to_string(),
                UnitState {
                    active: "active".into(),
                    sub: "running".into(),
                    result: "success".into(),
                    exec_main_status: 0,
                },
            );
        }
        let kill = Arc::new(AtomicBool::new(false));
        self.inner
            .kills
            .lock()
            .unwrap()
            .insert(id.to_string(), kill.clone());
        let host = self.clone();
        let id_owned = id.to_string();
        let data_dir_owned = data_dir.to_string();
        let job = std::thread::spawn(move || {
            // The unit runs on the host: inside this thread `/proc/self/mounts` is the host table.
            let _runner_scope = host.enter_runner_scope();
            // Every host boundary of this thread can now see the unit's kill switch.
            RUNNER_KILL.with(|cell| *cell.borrow_mut() = Some(kill));
            let paths = truenas_nvidia_driver_manager::store::Paths::new(data_dir_owned);
            let arc: Arc<dyn Host> = Arc::new(host.clone());
            let prepared = host.inner.build_env.lock().unwrap().clone();
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match prepared {
                    Some(env) => truenas_nvidia_driver_manager::runner::run_with_env(
                        arc, paths, &id_owned, env,
                    ),
                    None => truenas_nvidia_driver_manager::runner::run_with(arc, paths, &id_owned),
                }));
            host.inner.kills.lock().unwrap().remove(&id_owned);
            let code = match outcome {
                Ok(code) => code,
                // `kill_unit` already marked the unit failed; the killed thread stops here without
                // running or writing anything.
                Err(payload) if payload.is::<RunnerKilled>() => return,
                Err(payload) => std::panic::resume_unwind(payload),
            };
            let mut units = host.inner.units.lock().unwrap();
            use std::process::ExitCode;
            if code == ExitCode::SUCCESS {
                units.remove(&id_owned);
            } else if let Some(u) = units.get_mut(&id_owned) {
                u.active = "failed".into();
                u.sub = "failed".into();
                u.result = "exit-code".into();
                u.exec_main_status = 1;
            }
        });
        self.inner.jobs.lock().unwrap().push(job);
        FakeOutput::code(0)
    }

    fn systemctl(&self, cmd: &Cmd) -> FakeOutput {
        let args = arg_strings(cmd);
        match args.first().map(|s| s.as_str()) {
            Some("show") => {
                let unit = args.get(1).cloned().unwrap_or_default();
                let id = unit
                    .strip_prefix("nvm-task-")
                    .and_then(|r| r.strip_suffix(".service"))
                    .unwrap_or("");
                let units = self.inner.units.lock().unwrap();
                match units.get(id) {
                    Some(u) => FakeOutput::stdout(&format!(
                        "LoadState=loaded\nActiveState={}\nSubState={}\nResult={}\nExecMainStatus={}\n",
                        u.active, u.sub, u.result, u.exec_main_status
                    )),
                    None => FakeOutput::stdout(
                        "LoadState=not-found\nActiveState=inactive\nSubState=dead\nResult=success\nExecMainStatus=0\n",
                    ),
                }
            }
            Some("list-units") => {
                if *self.inner.list_units_fails.lock().unwrap() {
                    return FakeOutput::code(1).with_stderr("systemctl: list-units failed");
                }
                let units = self.inner.units.lock().unwrap();
                let mut out = String::new();
                for (id, u) in units.iter() {
                    out.push_str(&format!(
                        "nvm-task-{id}.service loaded {} {} nvm-task mock\n",
                        u.active, u.sub
                    ));
                }
                FakeOutput::stdout(&out)
            }
            Some("list-jobs") => {
                if *self.inner.list_jobs_fails.lock().unwrap() {
                    let exit = if *self.inner.list_jobs_times_out.lock().unwrap() {
                        Exit::TimedOut
                    } else {
                        Exit::Code(1)
                    };
                    let mut stdout = Vec::new();
                    if *self.inner.list_jobs_partial.lock().unwrap() {
                        let jobs = self.inner.job_lines.lock().unwrap();
                        let first = jobs
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "0 nvm-task.service start running".to_string());
                        stdout = format!("{}\n", first.chars().take(12).collect::<String>())
                            .into_bytes();
                    }
                    return FakeOutput {
                        exit,
                        stdout,
                        stderr: b"systemctl: list-jobs failed".to_vec(),
                        delay: Duration::ZERO,
                    };
                }
                let jobs = self.inner.job_lines.lock().unwrap().clone();
                let mut out = String::new();
                for job in jobs {
                    out.push_str(&job);
                    out.push('\n');
                }
                FakeOutput::stdout(&out)
            }
            Some("reset-failed") => FakeOutput::code(0),
            _ => FakeOutput::code(1).with_stderr("systemctl: unmodelled subcommand"),
        }
    }

    /// `docker ps` in the two forms the model answers; every other `ps` shape is refused with a
    /// non-zero exit instead of being answered with a guess.
    fn docker_ps(&self, args: &[String]) -> FakeOutput {
        let all = args.iter().any(|a| a == "-a" || a == "--all");
        let format = args
            .iter()
            .position(|a| a == "--format")
            .and_then(|i| args.get(i + 1).cloned())
            .or_else(|| {
                args.iter()
                    .find_map(|a| a.strip_prefix("--format=").map(str::to_string))
            })
            // The shell may have kept the quotes around the template.
            .map(|value| value.trim_matches(|c| c == '"' || c == '\'').to_string());
        if all {
            if format.as_deref() != Some("{{.Names}}") {
                return FakeOutput::code(1).with_stderr(&format!(
                    "docker: ps -a is modelled only with --format {{{{.Names}}}}, not {}",
                    format.as_deref().unwrap_or("(a missing --format)")
                ));
            }
            if *self.inner.docker_ps_all_fails.lock().unwrap() {
                let exit = if *self.inner.docker_ps_all_times_out.lock().unwrap() {
                    Exit::TimedOut
                } else {
                    Exit::Code(1)
                };
                let mut stdout = Vec::new();
                if *self.inner.docker_ps_all_partial.lock().unwrap() {
                    let names = self.container_names();
                    let first = names
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "nvm-build-".to_string());
                    stdout =
                        format!("{}\n", first.chars().take(12).collect::<String>()).into_bytes();
                }
                return FakeOutput {
                    exit,
                    stdout,
                    stderr: b"docker: ps -a --format {{.Names}} failed".to_vec(),
                    delay: Duration::ZERO,
                };
            }
            let mut text = String::new();
            for name in self.container_names() {
                text.push_str(&name);
                text.push('\n');
            }
            return FakeOutput::stdout(&text);
        }
        if format.is_some() {
            return FakeOutput::code(1)
                .with_stderr("docker: ps --format is modelled only together with -a");
        }
        // The unchanged form: the injected container ids of `ps -q --no-trunc`.
        let ids = self.inner.docker_ps.lock().unwrap().clone();
        FakeOutput::stdout(&ids.join("\n"))
    }

    fn docker(&self, cmd: &Cmd) -> FakeOutput {
        let args = arg_strings(cmd);
        match args.first().map(|s| s.as_str()) {
            Some("ps") => self.docker_ps(&args),
            Some("inspect") => {
                if *self.inner.docker_inspect_fails.lock().unwrap() {
                    FakeOutput::code(1).with_stderr("Error: No such object: x")
                } else {
                    FakeOutput {
                        exit: Exit::Code(0),
                        stdout: self.inner.docker_inspect.lock().unwrap().clone(),
                        stderr: Vec::new(),
                        delay: Duration::ZERO,
                    }
                }
            }
            Some("rm") => {
                if *self.inner.docker_rm_fails.lock().unwrap() {
                    return FakeOutput::code(1)
                        .with_stderr("Error response from daemon: cannot remove the container");
                }
                FakeOutput::code(0).with_stderr("Error response from daemon: No such container")
            }
            Some("run") => {
                // The build container: place the preset output into `out/`.
                let name = args
                    .iter()
                    .find(|a| a.starts_with("--name="))
                    .cloned()
                    .unwrap_or_default();
                let id = name.trim_start_matches("--name=nvm-build-").to_string();
                let out_dir = format!("{HOST_DATA_DIR}/manager/tasks/{id}/out");
                // Tests provide the artifact and result through put_file.
                let _ = self.exists(&out_dir);
                FakeOutput::code(0)
            }
            Some("rmi") => FakeOutput::code(0),
            Some("image") => self.docker_image(&args[1..]),
            Some("pull") => {
                if *self.inner.docker_pull_fails.lock().unwrap() {
                    return FakeOutput::code(1)
                        .with_stderr("Error response from daemon: pull access denied for debian");
                }
                // `docker pull [--platform <p>] <reference>`: the last argument is the reference,
                // and a successful pull makes it local with the modelled ID -- unless the test asks
                // for a pull that leaves nothing behind.
                let reference = args.last().cloned().unwrap_or_default();
                let id = self.inner.docker_pull_id.lock().unwrap().clone();
                if !*self.inner.docker_pull_leaves_nothing.lock().unwrap() {
                    self.inner
                        .images
                        .lock()
                        .unwrap()
                        .insert(reference, id.clone());
                }
                FakeOutput::stdout(&format!("{id}\n"))
            }
            _ => FakeOutput::code(1).with_stderr("docker: unmodelled subcommand"),
        }
    }

    /// `docker image inspect --format {{.Id}} <reference>` and `docker image rm <reference|id>`:
    /// the modelled local images of the pinned build environment.
    fn docker_image(&self, args: &[String]) -> FakeOutput {
        match args.first().map(|s| s.as_str()) {
            Some("inspect") => {
                let format = args
                    .iter()
                    .position(|a| a == "--format")
                    .and_then(|i| args.get(i + 1))
                    .map(|value| value.trim_matches(|c| c == '"' || c == '\'').to_string());
                if format.as_deref() != Some("{{.Id}}") {
                    return FakeOutput::code(1).with_stderr(
                        "docker: image inspect is modelled only with --format {{.Id}}",
                    );
                }
                let reference = args.last().cloned().unwrap_or_default();
                match self.inner.images.lock().unwrap().get(&reference) {
                    Some(id) => FakeOutput::stdout(&format!("{id}\n")),
                    None => FakeOutput::code(1).with_stderr(&format!(
                        "Error response from daemon: No such image: {reference}"
                    )),
                }
            }
            Some("rm") => {
                if *self.inner.docker_image_rm_fails.lock().unwrap() {
                    return FakeOutput::code(1)
                        .with_stderr("Error response from daemon: conflict: image is being used");
                }
                let target = args.last().cloned().unwrap_or_default();
                let mut images = self.inner.images.lock().unwrap();
                let before = images.len();
                images.retain(|reference, id| reference != &target && id != &target);
                if images.len() == before {
                    return FakeOutput::code(1).with_stderr(&format!(
                        "Error response from daemon: No such image: {target}"
                    ));
                }
                FakeOutput::code(0)
            }
            _ => FakeOutput::code(1).with_stderr("docker: unmodelled image subcommand"),
        }
    }

    fn zfs(&self, cmd: &Cmd) -> FakeOutput {
        let args = arg_strings(cmd);
        match args.first().map(|s| s.as_str()) {
            Some("get") => {
                let value = self.readonly();
                FakeOutput::stdout(&format!("{value}\n"))
            }
            Some("set") => {
                let prop = args.get(1).cloned().unwrap_or_default();
                if let Some(value) = prop.strip_prefix("readonly=") {
                    *self.inner.readonly.lock().unwrap() = value.to_string();
                    // With the overlay in place the exit code is non-zero even though the property
                    // was applied.
                    return FakeOutput::code(255)
                        .with_stderr("cannot set property for 'usr': property may be set but unable to remount filesystem");
                }
                FakeOutput::code(0)
            }
            _ => FakeOutput::code(0),
        }
    }

    /// The real path of a command argument that names a file this fixture models, so a passthrough
    /// program reads the fixture's own copy. A host command carries the host path
    /// (`<host_data_dir>/manager/upload/…`); the fixture keeps that file inside its sandbox, the
    /// same tree through the modelled data-directory bind mount. Only an absolute argument is
    /// looked at, so a flag or a field name is never mistaken for a file name.
    fn real_file_argument(&self, arg: &OsStr) -> OsString {
        let text = arg.to_string_lossy();
        if !text.starts_with('/') {
            return arg.to_os_string();
        }
        match self.real_path(&text) {
            Ok(real) if real.exists() => real.into_os_string(),
            _ => arg.to_os_string(),
        }
    }

    /// One `dpkg-deb` invocation against the real program (passthrough).
    fn real_dpkg(&self, cmd: &Cmd) -> FakeOutput {
        let started = Instant::now();
        let mut command = std::process::Command::new("dpkg-deb");
        command.args(cmd.args.iter().map(|a| self.real_file_argument(a)));
        command.env_clear();
        command.env("LC_ALL", "C");
        command.env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
        command.stdin(std::process::Stdio::null());
        match command.output() {
            Ok(out) => FakeOutput {
                exit: Exit::Code(out.status.code().unwrap_or(-1)),
                stdout: out.stdout,
                stderr: out.stderr,
                delay: started.elapsed(),
            },
            Err(e) => FakeOutput {
                exit: Exit::SpawnFailed,
                stdout: Vec::new(),
                stderr: e.to_string().into_bytes(),
                delay: started.elapsed(),
            },
        }
    }

    fn lookup_curl(&self, url: &str) -> Option<CurlResponse> {
        self.inner.curl.lock().unwrap().get(url).cloned()
    }

    /// Follow a 3xx chain the way curl 8 does when `--location` is set. Returns `None` when the
    /// first response is not a redirect, so the single-response path keeps its existing bytes. A
    /// redirect is fetched only when both `--proto` and `--proto-redir` allow its scheme; a missing
    /// flag adds no extra restriction, and `--proto-redir` cannot re-enable a scheme `--proto`
    /// rejected. Passing `--max-redirs` stops the chain with exit 47; the default cap is curl's own
    /// 50.
    fn curl_following(
        &self,
        args: &[String],
        start_url: &str,
        output: Option<&str>,
        headers_path: Option<&str>,
    ) -> Option<FakeOutput> {
        let first = self.lookup_curl(start_url)?;
        if first.exit != 0 || !(300..400).contains(&first.status) || first.location.is_none() {
            return None;
        }
        let max_redirs = curl_flag(args, "--max-redirs")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(50);
        let mut current = start_url.to_string();
        let mut followed: u32 = 0;
        let mut header_text = String::new();
        loop {
            let Some(response) = self.lookup_curl(&current) else {
                self.write_curl_headers(headers_path, &header_text);
                return Some(curl_miss());
            };
            if response.exit != 0 {
                self.write_curl_headers(headers_path, &header_text);
                if !response.partial.is_empty() {
                    if let Some(path) = output {
                        self.put_file(path, response.partial.clone());
                    }
                }
                return Some(FakeOutput {
                    exit: Exit::Code(response.exit),
                    stdout: Vec::new(),
                    stderr: response.stderr.into_bytes(),
                    delay: Duration::ZERO,
                });
            }
            push_curl_segment(&mut header_text, &response);
            let Some(location) = response
                .location
                .clone()
                .filter(|_| (300..400).contains(&response.status))
            else {
                self.write_curl_headers(headers_path, &header_text);
                return Some(self.curl_landed(output, &response));
            };
            if followed >= max_redirs {
                self.write_curl_headers(headers_path, &header_text);
                return Some(FakeOutput {
                    exit: Exit::Code(47),
                    stdout: status_stdout(response.status),
                    stderr: format!("curl: (47) Maximum ({max_redirs}) redirects followed\n")
                        .into_bytes(),
                    delay: Duration::ZERO,
                });
            }
            let next = resolve_location(&current, &location);
            if !redirect_allowed(args, &next) {
                self.write_curl_headers(headers_path, &header_text);
                let scheme = next.split_once("://").map(|(s, _)| s).unwrap_or("");
                return Some(FakeOutput {
                    exit: Exit::Code(1),
                    stdout: status_stdout(response.status),
                    stderr: format!("curl: (1) Protocol \"{scheme}\" disabled (in redirect)\n")
                        .into_bytes(),
                    delay: Duration::ZERO,
                });
            }
            followed += 1;
            current = next;
        }
    }

    fn write_curl_headers(&self, path: Option<&str>, text: &str) {
        if let Some(path) = path {
            if !text.is_empty() {
                self.put_text(path, text);
            }
        }
    }

    fn curl_landed(&self, output: Option<&str>, response: &CurlResponse) -> FakeOutput {
        let stdout = match output {
            Some("-") => {
                let mut out = response.body.clone();
                out.push(b'\n');
                out.extend_from_slice(status_stdout(response.status).as_slice());
                out
            }
            Some(path) => {
                self.put_file(path, response.body.clone());
                status_stdout(response.status)
            }
            None => status_stdout(response.status),
        };
        FakeOutput {
            exit: Exit::Code(0),
            stdout,
            stderr: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    fn curl(&self, cmd: &Cmd) -> FakeOutput {
        let args = arg_strings(cmd);
        let url = args.last().cloned().unwrap_or_default();
        let output = args
            .iter()
            .position(|a| a == "--output")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let headers = args
            .iter()
            .position(|a| a == "--dump-header")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let follow = args.iter().any(|a| a == "--location" || a == "-L");
        if follow {
            if let Some(followed) =
                self.curl_following(&args, &url, output.as_deref(), headers.as_deref())
            {
                return followed;
            }
        }
        let response = self.inner.curl.lock().unwrap().get(&url).cloned();
        match response {
            Some(r) if r.exit != 0 => {
                // A failing transfer still leaves what it already wrote to `--output`, like real
                // `curl`.
                if !r.partial.is_empty() {
                    if let Some(path) = output.as_deref() {
                        self.put_file(path, r.partial.clone());
                    }
                }
                FakeOutput {
                    exit: Exit::Code(r.exit),
                    stdout: Vec::new(),
                    stderr: r.stderr.into_bytes(),
                    delay: Duration::ZERO,
                }
            }
            Some(r) => {
                if let Some(headers) = &headers {
                    self.put_text(
                        headers,
                        &format!(
                            "HTTP/1.1 {} OK\r\nContent-Length: {}\r\n\r\n",
                            r.status,
                            r.body.len()
                        ),
                    );
                }
                let stdout = match output.as_deref() {
                    Some("-") => {
                        // Curl writes the body to stdout, then `--write-out`.
                        let mut out = r.body.clone();
                        out.push(b'\n');
                        out.extend_from_slice(format!("{}", r.status).as_bytes());
                        out
                    }
                    Some(path) => {
                        self.put_file(path, r.body.clone());
                        format!("{}", r.status).into_bytes()
                    }
                    None => format!("{}", r.status).into_bytes(),
                };
                FakeOutput {
                    exit: Exit::Code(0),
                    stdout,
                    stderr: Vec::new(),
                    delay: Duration::ZERO,
                }
            }
            None => curl_miss(),
        }
    }
}

fn arg_strings(cmd: &Cmd) -> Vec<String> {
    cmd.args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn refuses(list: &Mutex<Vec<String>>, path: &str) -> bool {
    list.lock()
        .unwrap()
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

fn curl_miss() -> FakeOutput {
    FakeOutput {
        exit: Exit::Code(6),
        stdout: Vec::new(),
        stderr: b"curl: (6) Could not resolve host".to_vec(),
        delay: Duration::ZERO,
    }
}

fn status_stdout(status: u16) -> Vec<u8> {
    format!("{status}").into_bytes()
}

fn curl_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn push_curl_segment(buf: &mut String, response: &CurlResponse) {
    let reason = match response.status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        _ => "",
    };
    buf.push_str(&format!(
        "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\n",
        response.status,
        response.body.len()
    ));
    if (300..400).contains(&response.status) {
        if let Some(location) = &response.location {
            buf.push_str(&format!("Location: {location}\r\n"));
        }
    }
    buf.push_str("\r\n");
}

/// Resolve absolute, scheme-relative and rooted redirect locations.
fn resolve_location(current: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    let (scheme, rest) = current.split_once("://").expect("an absolute transfer URL");
    if location.starts_with("//") {
        return format!("{scheme}:{location}");
    }
    assert!(
        location.starts_with('/'),
        "unsupported redirect location: {location}"
    );
    let authority = rest.split(['/', '?', '#']).next().unwrap();
    format!("{scheme}://{authority}{location}")
}

/// Both protocol lists apply. A scheme rejected by `--proto` stays rejected when `--proto-redir`
/// names it. A missing flag does not add a restriction.
fn redirect_allowed(args: &[String], next: &str) -> bool {
    let scheme = next.split_once("://").map(|(value, _)| value).unwrap_or("");
    if scheme.is_empty() {
        return false;
    }
    let proto_ok = curl_flag(args, "--proto")
        .map(|policy| proto_allows(policy, scheme))
        .unwrap_or(true);
    let redir_ok = curl_flag(args, "--proto-redir")
        .map(|policy| proto_allows(policy, scheme))
        .unwrap_or(true);
    proto_ok && redir_ok
}

fn proto_allows(policy: &str, scheme: &str) -> bool {
    let only = policy
        .strip_prefix('=')
        .expect("an explicit protocol allowlist");
    only.split(',').any(|name| name == scheme)
}

/// A writable file backed by the real sandbox file: bytes are written through immediately and
/// `sync_all` flushes the real file.
struct FakeWriter {
    file: std::fs::File,
    inner: Arc<Inner>,
    path: String,
}

impl Write for FakeWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.file.write(data)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl SyncWrite for FakeWriter {
    fn sync_all(&mut self) -> std::io::Result<()> {
        if refuses(&self.inner.fail_syncs, &self.path) {
            return Err(std::io::Error::other("sync refused by the fake host"));
        }
        self.file.sync_all()
    }
}

/// The key of the process-wide lock model for one host path: the bind-mount translation, then the
/// container view (`/proc/1/root/<x>` names `/<x>`), then `.`/`..` folded away. Purely textual --
/// it never touches the real file system -- so the container view, the `/proc/1/root` view and the
/// host view of one data-directory file all key the same object.
fn lock_key(host_path: &str) -> String {
    let translated = FakeHost::translate(host_path);
    let folded = match translated.strip_prefix("/proc/1/root/") {
        Some(rest) => format!("/{rest}"),
        None => translated,
    };
    // The folded path may itself be a container view of the data directory.
    let translated = FakeHost::translate(&folded);
    let mut parts: Vec<&str> = Vec::new();
    for part in translated.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// `(st_dev, st_ino)` of a real sandbox node; `0/0` when there is none, like the production reading
/// of an object that is already gone.
fn node_identity(meta: &std::fs::Metadata) -> LockId {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        LockId {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        LockId { dev: 0, ino: 0 }
    }
}

/// The lstat-style checks the production open runs on the descriptor it holds (src/host.rs): a lock
/// file must not be accessible to group or others and must belong to the effective user.
fn refuse_insecure_lock(meta: &std::fs::Metadata) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the lock file is accessible to group or others",
            ));
        }
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the lock file belongs to another user",
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
    }
    Ok(())
}

/// One open file description of the fake lock model. It is **not** a real OS lock and does not
/// claim to be one: the model cannot express a foreign process, a kernel-held `flock`, a lock on a
/// node outside the sandbox, an fd passed to another process, or any advisory-lock semantics of a
/// real file system. What it does model is the one property the drain logic depends on: two handles
/// opened through this fake host for the same normalized path contend, even inside one process,
/// exactly like two open file descriptions of one inode.
struct FakeLock {
    /// The normalized host path, the key of the model.
    key: String,
    /// The real sandbox node the identity is read from.
    real: PathBuf,
    /// Whether this handle is the current holder.
    held: bool,
    /// The open descriptor of the real kernel lock, when that mode is on.
    file: Option<std::fs::File>,
    inner: Arc<Inner>,
}

impl FakeLock {
    fn do_release(&mut self) {
        if self.held {
            if let Some(file) = &self.file {
                #[cfg(unix)]
                {
                    use std::os::unix::io::AsRawFd;
                    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
                }
            } else {
                self.inner.locks_held.lock().unwrap().remove(&self.key);
            }
            self.held = false;
        }
    }
}

impl HostLock for FakeLock {
    fn identity(&self) -> std::io::Result<LockId> {
        match std::fs::metadata(&self.real) {
            Ok(meta) => Ok(node_identity(&meta)),
            Err(_) => Ok(LockId { dev: 0, ino: 0 }),
        }
    }

    fn try_acquire(&mut self) -> std::io::Result<bool> {
        if *self.inner.lock_acquire_fails.lock().unwrap() {
            return Err(std::io::Error::other(
                "lock acquisition refused by the fake host",
            ));
        }
        if self.held {
            // Re-locking the descriptor already held is a no-op success in `flock`, and it is here
            // too.
            return Ok(true);
        }
        if let Some(file) = &self.file {
            // The real kernel lock: another process holding the same node fails this non-blocking
            // attempt, exactly like the product sees.
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd;
                let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if rc == 0 {
                    self.held = true;
                    return Ok(true);
                }
                let err = std::io::Error::last_os_error();
                return match err.raw_os_error() {
                    Some(libc::EWOULDBLOCK) => Ok(false),
                    _ => Err(err),
                };
            }
            #[cfg(not(unix))]
            return Ok(false);
        }
        let mut held = self.inner.locks_held.lock().unwrap();
        if held.contains(&self.key) {
            return Ok(false);
        }
        held.insert(self.key.clone());
        self.held = true;
        Ok(true)
    }

    fn release(&mut self) -> std::io::Result<()> {
        // Releasing twice is fine, like closing the same descriptor twice is not an error the
        // caller has to care about.
        self.do_release();
        Ok(())
    }
}

impl Drop for FakeLock {
    fn drop(&mut self) {
        self.do_release();
    }
}

impl Host for FakeHost {
    fn run(&self, cmd: &Cmd) -> Output {
        // A killed runner must not run another command; this also covers a command that has not
        // started yet.
        self.check_killed();
        self.record(cmd);
        if let Some(preset) = self.matched_preset(cmd) {
            let deadline = self.now() + cmd.timeout;
            if let Some(gate) = &preset.gate {
                // The deadline follows the fake clock: a test may advance it while the command
                // hangs.
                loop {
                    // The kill wakes the blocked command: the thread stops before the gate can ever
                    // release it.
                    self.check_killed();
                    if self.now() >= deadline {
                        return Output {
                            exit: Exit::TimedOut,
                            stdout: preset.output.stdout,
                            stderr: preset.output.stderr,
                            elapsed: cmd.timeout,
                        };
                    }
                    if gate.is_open() {
                        break;
                    }
                    *gate.blocked.lock().unwrap() = true;
                    std::thread::sleep(Duration::from_millis(2));
                }
                // The gate may have opened between the check above and the kill; the command still
                // must not run.
                self.check_killed();
            }
            let started = Instant::now();
            while started.elapsed() < preset.output.delay {
                self.check_killed();
                if self.now() >= deadline {
                    return Output {
                        exit: Exit::TimedOut,
                        stdout: preset.output.stdout,
                        stderr: preset.output.stderr,
                        elapsed: cmd.timeout,
                    };
                }
                let remaining = preset.output.delay.saturating_sub(started.elapsed());
                std::thread::sleep(remaining.min(Duration::from_millis(2)));
            }
            self.check_killed();
            let out = Output {
                exit: preset.output.exit,
                stdout: preset.output.stdout,
                stderr: preset.output.stderr,
                elapsed: started.elapsed() + preset.output.delay,
            };
            self.finish_docker_command(cmd, &out);
            return out;
        }
        let started = Instant::now();
        let out = match self.builtin(cmd) {
            Some(out) => Output {
                exit: out.exit,
                stdout: out.stdout,
                stderr: out.stderr,
                elapsed: started.elapsed() + out.delay,
            },
            None => Output {
                exit: Exit::Code(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
                elapsed: started.elapsed(),
            },
        };
        self.finish_docker_command(cmd, &out);
        out
    }

    fn run_watch(&self, cmd: &Cmd, every: Duration, w: &mut dyn Watch) -> Output {
        // Presets and builtins are returned in one go; ticks are simulated so the progress writers
        // still run.
        let started = Instant::now();
        let mut ticks = 0;
        let out = self.run(cmd);
        if cmd.program == "docker" && out.exit.ok() {
            let args = arg_strings(cmd);
            if args.first().is_some_and(|a| a == "run") && args.iter().any(|a| a == "--rm") {
                if let Some(name) = args.iter().find_map(|a| a.strip_prefix("--name=")) {
                    self.inner.containers.lock().unwrap().remove(name);
                }
            }
        }
        let elapsed = started.elapsed();
        if elapsed > every {
            ticks = (elapsed.as_nanos() / every.as_nanos().max(1)) as usize;
        }
        if ticks == 0 {
            ticks = 1;
        }
        for _ in 0..ticks {
            w.tick();
        }
        for line in out.stdout.split(|b| *b == b'\n') {
            if !line.is_empty() {
                w.line(Stream::Stdout, line);
            }
        }
        for line in out.stderr.split(|b| *b == b'\n') {
            if !line.is_empty() {
                w.line(Stream::Stderr, line);
            }
        }
        out
    }

    fn view(&self, host_path: &Path) -> PathBuf {
        match self.inner.view_prefix.lock().unwrap().clone() {
            Some(prefix) => {
                Path::new(&prefix).join(host_path.strip_prefix("/").unwrap_or(host_path))
            }
            None => host_path.to_path_buf(),
        }
    }

    fn mount_table(&self) -> std::io::Result<Vec<MountLine>> {
        if *self.inner.mount_table_fails.lock().unwrap() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "mount table refused by the fake host",
            ));
        }
        Ok(self.inner.host_mounts.lock().unwrap().clone())
    }

    fn read(&self, p: &Path) -> std::io::Result<Vec<u8>> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note("read", &path);
        if refuses(&self.inner.fail_reads, &path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "read refused by the fake host",
            ));
        }
        // A registered marker fires on the first read below its prefix. The read itself is not
        // altered in any way.
        let fired: Vec<std::path::PathBuf> = {
            let markers = self.inner.read_markers.lock().unwrap();
            markers
                .iter()
                .filter(|(prefix, _)| path.starts_with(prefix))
                .map(|(_, marker)| marker.clone())
                .collect()
        };
        for marker in fired {
            let _ = std::fs::write(&marker, b"read");
        }
        if let Some(bytes) = self.generated_proc(&path)? {
            return self.apply_read_change(&path, bytes);
        }
        let bytes = std::fs::read(self.resolve(&path, true)?)?;
        self.apply_read_change(&path, bytes)
    }

    fn open(&self, p: &Path) -> std::io::Result<Box<dyn Read + Send>> {
        let translated = FakeHost::translate(&p.to_string_lossy());
        if translated == "/proc/self/exe" {
            self.note("open", "/proc/self/exe");
            return Ok(Box::new(Cursor::new(
                self.inner.exe.lock().unwrap().clone(),
            )));
        }
        let bytes = self.read(Path::new(&translated))?;
        Ok(Box::new(Cursor::new(bytes)))
    }

    fn create(
        &self,
        p: &Path,
        how: Create,
        mode: u32,
    ) -> std::io::Result<Box<dyn SyncWrite + Send>> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note(
            match how {
                Create::New => "create_new",
                Create::Truncate => "create_truncate",
            },
            &path,
        );
        if refuses(&self.inner.fail_writes, &path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "write refused by the fake host",
            ));
        }
        let real = self.resolve(&path, true)?;
        self.ensure_parent(&real);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(mode);
        }
        match how {
            Create::New => {
                opts.create_new(true);
            }
            Create::Truncate => {
                opts.create(true).truncate(true);
            }
        }
        Ok(Box::new(FakeWriter {
            file: opts.open(real)?,
            inner: self.inner.clone(),
            path,
        }))
    }

    fn append(&self, p: &Path, mode: u32) -> std::io::Result<Box<dyn Write + Send>> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note("append", &path);
        if refuses(&self.inner.fail_writes, &path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "write refused by the fake host",
            ));
        }
        let real = self.resolve(&path, true)?;
        self.ensure_parent(&real);
        let mut opts = std::fs::OpenOptions::new();
        opts.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(mode);
        }
        Ok(Box::new(opts.open(real)?))
    }

    fn lstat(&self, p: &Path) -> std::io::Result<std::fs::Metadata> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note("lstat", &path);
        self.wait_path_gate(&path);
        std::fs::symlink_metadata(self.resolve(&path, false)?)
    }

    fn read_link(&self, p: &Path) -> std::io::Result<PathBuf> {
        let path = p.to_string_lossy().into_owned();
        self.note("readlink", &path);
        self.wait_path_gate(&FakeHost::translate(&path));
        if path == "/proc/1/ns/mnt" {
            return Ok(PathBuf::from("mnt:[host]"));
        }
        if path == "/proc/self/ns/mnt" {
            if *self.inner.same_namespace.lock().unwrap() {
                return Ok(PathBuf::from("mnt:[host]"));
            }
            return Ok(PathBuf::from("mnt:[container]"));
        }
        let translated = FakeHost::translate(&path);
        std::fs::read_link(self.resolve(&translated, false)?)
    }

    fn read_dir(&self, p: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note("read_dir", &path);
        if refuses(&self.inner.fail_read_dirs, &path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "listing refused by the fake host",
            ));
        }
        Ok(real_names(&self.resolve(&path, true)?)?
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect())
    }

    fn create_dir(&self, p: &Path, mode: u32) -> std::io::Result<()> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note("mkdir", &path);
        self.wait_path_gate(&path);
        let real = self.resolve(&path, false)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(mode)
                .create(real)
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
            std::fs::create_dir_all(real)
        }
    }

    fn hard_link(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        let from = FakeHost::translate(&from.to_string_lossy());
        let to = FakeHost::translate(&to.to_string_lossy());
        self.note("hard_link", &format!("{from} -> {to}"));
        if refuses(&self.inner.fail_writes, &to) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "link refused by the fake host",
            ));
        }
        std::fs::hard_link(self.resolve(&from, true)?, self.resolve(&to, false)?)
    }

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        let from = FakeHost::translate(&from.to_string_lossy());
        let to = FakeHost::translate(&to.to_string_lossy());
        self.note("rename", &format!("{from} -> {to}"));
        if self
            .inner
            .fail_rename_contains
            .lock()
            .unwrap()
            .iter()
            .any(|needle| from.contains(needle.as_str()))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "rename refused by the fake host",
            ));
        }
        if let Some(suffix) = self.inner.fail_rename_suffix.lock().unwrap().clone() {
            if from.ends_with(&suffix) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "rename refused by the fake host",
                ));
            }
        }
        if self
            .inner
            .fail_rename_exdev
            .lock()
            .unwrap()
            .iter()
            .any(|suffix| from.ends_with(suffix.as_str()))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::CrossesDevices,
                "rename across filesystems refused by the fake host",
            ));
        }
        {
            let mut rules = self.inner.fail_rename_to.lock().unwrap();
            for (suffix, allowed, seen) in rules.iter_mut() {
                if to.ends_with(suffix.as_str()) {
                    *seen += 1;
                    if *seen > *allowed {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "rename to this target refused by the fake host",
                        ));
                    }
                }
            }
        }
        {
            let from_name = from.rsplit('/').next().unwrap_or("");
            let to_name = to.rsplit('/').next().unwrap_or("");
            let rules = self.inner.fail_rename_rollback.lock().unwrap();
            for target in rules.iter() {
                if to_name == target && from_name.starts_with(&format!(".{target}.tmp-")) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "rollback rename refused by the fake host",
                    ));
                }
            }
        }
        let state_phase = if to.contains("/manager/state/") && to.ends_with(".json") {
            self.file(&from)
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|value| value["phase"].as_str().map(str::to_string))
        } else {
            None
        };
        if state_phase
            .as_ref()
            .is_some_and(|phase| self.inner.fail_state_phases.lock().unwrap().contains(phase))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "state phase refused",
            ));
        }
        let real_from = self.resolve(&from, false)?;
        let real_to = self.resolve(&to, false)?;
        if let Some(parent) = real_to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::rename(real_from, real_to)?;
        if state_phase
            .is_some_and(|phase| self.inner.autoload_phases.lock().unwrap().remove(&phase))
        {
            // Seed the current merged view before loading, just as a runtime hook would.
            if self.inner.merged_version.lock().unwrap().is_none() {
                *self.inner.merged_version.lock().unwrap() = self.listing_version(CANONICAL_PATH);
                *self.inner.merged_identity.lock().unwrap() =
                    self.file(CANONICAL_PATH).map(|b| super::sha256_hex(&b));
            }
            self.load_module("nvidia", 0, &[]);
            self.load_module("nvidia_uvm", 0, &[]);
        }
        let mut listings = self.inner.listings.lock().unwrap();
        let moved = listings
            .keys()
            .filter_map(|path| {
                path.strip_prefix(&from)
                    .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
                    .map(|suffix| (path.clone(), format!("{to}{suffix}")))
            })
            .collect::<Vec<_>>();
        for (old, new) in moved {
            if let Some(listing) = listings.remove(&old) {
                listings.insert(new, listing);
            }
        }
        Ok(())
    }

    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note("remove_file", &path);
        if refuses(&self.inner.fail_removes, &path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "remove refused by the fake host",
            ));
        }
        std::fs::remove_file(self.resolve(&path, false)?)
    }

    fn remove_dir_all(&self, p: &Path) -> std::io::Result<()> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note("remove_dir_all", &path);
        std::fs::remove_dir_all(self.resolve(&path, false)?)
    }

    fn fsync_dir(&self, p: &Path) -> std::io::Result<()> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note("fsync_dir", &path);
        if refuses(&self.inner.fail_fsync_dirs, &path) {
            return Err(std::io::Error::other("fsync refused by the fake host"));
        }
        std::fs::File::open(self.resolve(&path, true)?)?.sync_all()
    }

    fn fsync_file(&self, p: &Path) -> std::io::Result<()> {
        let path = FakeHost::translate(&p.to_string_lossy());
        self.note("fsync_file", &path);
        std::fs::File::open(self.resolve(&path, true)?)?.sync_all()
    }

    /// Open the modelled lock object of `host_path`. The real file is created inside the sandbox
    /// with `mode` (parent directories included) so its identity and mode are real, the production
    /// checks on the held descriptor are applied, and the handle then competes in the in-memory
    /// model keyed by the normalized host path. The lock itself is **not** a real OS lock: see
    /// [`FakeLock`] for what the model cannot express.
    fn open_lock(&self, host_path: &Path, mode: u32) -> std::io::Result<Box<dyn HostLock + Send>> {
        // The kernel lock is taken on the host path; a container process reaches it through the
        // host view, exactly like the container mode of `RealHost`.
        let viewed = self.view(host_path);
        // The model key is the normalized host path (two views of one file contend on the same
        // key); the file itself is opened where this process really reaches it.
        let key = lock_key(&viewed.to_string_lossy());
        self.note("open_lock", &key);
        if *self.inner.lock_open_fails.lock().unwrap() {
            return Err(std::io::Error::other("lock open refused by the fake host"));
        }
        let real = self.real_path(&viewed.to_string_lossy())?;
        self.ensure_parent(&real);
        let override_mode = *self.inner.lock_mode.lock().unwrap();
        let create_mode = override_mode.unwrap_or(mode);
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(create_mode);
        }
        #[cfg(not(unix))]
        {
            let _ = create_mode;
        }
        let file = opts.open(&real)?;
        #[cfg(unix)]
        if let Some(mode) = override_mode {
            // An injected mode is applied exactly, without the process umask: the test asks for one
            // specific node mode, whatever the environment.
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = override_mode;
        // The mode argument only applies at creation: an existing file keeps its own mode, which
        // the checks below then judge, exactly like the production `open_lock_real`.
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the lock path is not a regular file",
            ));
        }
        refuse_insecure_lock(&meta)?;
        let real_flock = self.inner.real_flock.load(Ordering::SeqCst);
        let file = if real_flock { Some(file) } else { None };
        Ok(Box::new(FakeLock {
            key,
            real,
            held: false,
            file,
            inner: self.inner.clone(),
        }))
    }

    fn uname_release(&self) -> std::io::Result<String> {
        Ok(self.inner.uname.lock().unwrap().clone())
    }

    fn now(&self) -> Instant {
        Instant::now() + self.inner.clock.lock().unwrap().offset
    }

    fn wait(&self, duration: Duration) {
        let hook = self.inner.on_wait.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
        if !duration.is_zero() {
            self.advance(duration);
        }
    }

    fn wall_ms(&self) -> u64 {
        let clock = self.inner.clock.lock().unwrap();
        clock.wall_base_ms + clock.offset.as_millis() as u64
    }
}

/// A helper used by tests to build the default PCI directory.
pub fn default_pci_dir(host: &FakeHost) {
    host.mkdir("/sys/bus/pci/devices");
}

/// The kinds of facts a test host starts with.
pub fn default_host() -> FakeHost {
    let host = FakeHost::new();
    prime_default_scene(&host);
    host
}

/// Prime an **existing** fixture into the healthy default scene, without touching the deployment
/// claim: two processes can share one sandbox, and the one that owns the reset also owns the claim.
pub fn prime_default_scene(host: &FakeHost) {
    default_pci_dir(host);
    host.mkdir("/sys/class/drm");
    host.mkdir("/boot");
    host.put_text(
        &format!("/boot/config-{KERNEL}"),
        "CONFIG_CC_VERSION_TEXT=\"gcc (Debian 12.2.0-14+deb12u1) 12.2.0\"\n",
    );
    host.mkdir("/usr/src/linux-headers-truenas-production-amd64/include/linux");
    host.mkdir("/usr/src/linux-headers-truenas-production-amd64/include/drm");
    host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH_FOR_TESTS);
    host.load_module("nvidia", 2, &["nvidia_uvm", "nvidia_modeset"]);
    host.load_module("nvidia_uvm", 0, &[]);
    host.load_module("nvidia_modeset", 1, &["nvidia"]);
    host.load_module("nvidia_drm", 0, &["nvidia_modeset"]);
}

pub const CANONICAL_PATH_FOR_TESTS: &str = "/usr/share/truenas/sysext-extensions/nvidia.raw";

/// Supported host/compiler scenes share the same implementation path.
pub fn prime_kernel_scene(host: &FakeHost, kernel: &str, truenas: &str, compiler: &str) {
    // Scene resets must override this response too: the midclt map survives resets.
    let containers = if truenas.starts_with("27.") {
        FakeOutput::stdout("[]")
    } else {
        FakeOutput::code(1).with_stderr("[ENOMETHOD] Method does not exist")
    };
    host.set_midclt("call container.query", containers);
    host.set_uname(kernel);
    host.put_text(
        "/data/manifest.json",
        &serde_json::json!({"version":truenas}).to_string(),
    );
    host.put_text(
        &format!("/boot/config-{kernel}"),
        &format!("CONFIG_CC_VERSION_TEXT=\"{compiler}\"\n"),
    );
    host.mkdir("/usr/src/linux-headers-truenas-production-amd64/include/config");
    host.put_text(
        "/usr/src/linux-headers-truenas-production-amd64/include/config/auto.conf",
        &format!("CONFIG_CC_VERSION_TEXT={compiler}\n"),
    );
}

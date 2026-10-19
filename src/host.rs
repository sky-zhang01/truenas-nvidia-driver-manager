//! The single entry point for host access.
//!
//! Every host command and every file operation of the web process and of the runner goes
//! through the [`Host`] trait. Production code uses [`RealHost`] for both processes, switched
//! by `in_container`; tests implement the same trait with a fake host.
//!
//! Execution rules: argv is passed item by item to `execve`, never through a shell; every
//! command gets a new process group, an empty stdin, the working directory `/` and only
//! `LC_ALL=C` and a fixed `PATH`; stdout and stderr are collected concurrently; on timeout
//! the process group is killed with `SIGKILL` and reaped.

#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::codes::CONTAINER_DATA_DIR;
use crate::facts::MountLine;

/// Where a command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// On the host. In the web process this is wrapped in the binary's own
    /// namespace mode ([`enter_host_namespaces`]); in the runner it runs
    /// directly.
    Host,
    /// In the caller's own environment (the container for the web process).
    Here,
}

/// One host command.
#[derive(Debug, Clone)]
pub struct Cmd {
    pub at: At,
    pub program: &'static str,
    pub args: Vec<OsString>,
    pub timeout: Duration,
}

impl Cmd {
    pub fn new(at: At, program: &'static str, timeout: Duration) -> Cmd {
        Cmd {
            at,
            program,
            args: Vec::new(),
            timeout,
        }
    }

    pub fn arg(mut self, a: impl Into<OsString>) -> Cmd {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Cmd
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// The argv as it is handed to `execve`, without the namespace-mode
    /// prefix.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![self.program.to_string()];
        argv.extend(self.args.iter().map(|a| a.to_string_lossy().into_owned()));
        argv
    }
}

/// Which stream a watched line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// How a command ended; one of the four values stored in state files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    TimedOut,
    SpawnFailed,
}

impl Exit {
    pub fn ok(&self) -> bool {
        matches!(self, Exit::Code(0))
    }

    /// The `{"code": …}` / `{"signal": …}` / `{"timed_out": true}` / `{"spawn_failed": true}`
    /// shape of state files.
    pub fn to_json(self) -> serde_json::Value {
        match self {
            Exit::Code(c) => serde_json::json!({ "code": c }),
            Exit::Signal(s) => serde_json::json!({ "signal": s }),
            Exit::TimedOut => serde_json::json!({ "timed_out": true }),
            Exit::SpawnFailed => serde_json::json!({ "spawn_failed": true }),
        }
    }

    pub fn code(self) -> Option<i32> {
        match self {
            Exit::Code(c) => Some(c),
            _ => None,
        }
    }
}

/// The result of one command.
#[derive(Debug, Clone)]
pub struct Output {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed: Duration,
}

impl Output {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// Last `n` lines of stdout followed by stderr, used for `error.raw`.
    pub fn raw_tail(&self, lines: usize) -> String {
        let mut all: Vec<&[u8]> = Vec::new();
        all.extend(self.stdout.split(|b| *b == b'\n'));
        all.extend(self.stderr.split(|b| *b == b'\n'));
        if all.last().map(|l| l.is_empty()).unwrap_or(false) {
            all.pop();
        }
        let start = all.len().saturating_sub(lines);
        let mut out = Vec::new();
        for (i, line) in all[start..].iter().enumerate() {
            if i > 0 {
                out.push(b'\n');
            }
            out.extend_from_slice(line);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// The command-failure reason, first match wins.
    pub fn failure_reason(&self, timeout: Duration) -> Reason {
        match self.exit {
            Exit::TimedOut => Reason::keyed(
                crate::codes::REASON_TIMED_OUT,
                serde_json::json!({ "seconds": timeout.as_secs() }),
            ),
            Exit::SpawnFailed => {
                Reason::keyed(crate::codes::REASON_SPAWN_FAILED, serde_json::json!({}))
            }
            _ if !self.stderr.is_empty() => Reason::Stderr(self.stderr_text()),
            Exit::Code(c) => Reason::keyed(
                crate::codes::REASON_EXIT_STATUS,
                serde_json::json!({ "code": c }),
            ),
            Exit::Signal(s) => Reason::keyed(
                crate::codes::REASON_SIGNAL,
                serde_json::json!({ "signal": s }),
            ),
        }
    }
}

/// A `{key, params}` reason or a raw stderr text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    Keyed {
        key: &'static str,
        params: serde_json::Value,
    },
    Stderr(String),
}

impl Reason {
    pub fn keyed(key: &'static str, params: serde_json::Value) -> Reason {
        Reason::Keyed { key, params }
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Reason::Keyed { key, params } => serde_json::json!({ "key": key, "params": params }),
            Reason::Stderr(text) => serde_json::json!({ "stderr": text }),
        }
    }
}

/// Receives output lines and periodic ticks of a long-running command.
pub trait Watch {
    fn line(&mut self, from: Stream, line: &[u8]);
    fn tick(&mut self);
}

/// How a file is opened for writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Create {
    /// `O_CREAT | O_EXCL`
    New,
    /// `O_CREAT | O_TRUNC`
    Truncate,
}

/// A writable file that can be flushed to stable storage.
pub trait SyncWrite: Write + Send {
    fn sync_all(&mut self) -> io::Result<()>;
}

impl SyncWrite for std::fs::File {
    fn sync_all(&mut self) -> io::Result<()> {
        std::fs::File::sync_all(self)
    }
}

/// Identity of one open file object. It is reported for audit output only and never used to
/// decide who holds a lock (the kernel lock on the open file description is the
/// authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockId {
    pub dev: u64,
    pub ino: u64,
}

/// An exclusive advisory lock on one fixed file.
///
/// The file behind it is opened once, validated on the descriptor that is
/// actually held, and then locked with `flock`; it is never removed, renamed or
/// written, so every process that follows the protocol competes for the same
/// inode. Dropping the handle closes the descriptor, which releases the lock.
pub trait HostLock: Send {
    /// `(st_dev, st_ino)` of the locked object.
    fn identity(&self) -> io::Result<LockId>;
    /// One non-blocking attempt. `Ok(false)` means another holder has it; every
    /// other failure is an I/O error and is never treated as "free".
    fn try_acquire(&mut self) -> io::Result<bool>;
    /// Release the OS lock. The file itself stays in place.
    fn release(&mut self) -> io::Result<()>;
}

/// Host access. All methods are synchronous; the web process runs them in `spawn_blocking`.
pub trait Host: Send + Sync {
    fn run(&self, cmd: &Cmd) -> Output;
    fn run_watch(&self, cmd: &Cmd, every: Duration, w: &mut dyn Watch) -> Output;

    /// How a host path looks from this process.
    fn view(&self, host_path: &Path) -> PathBuf;

    /// The mount table of the namespace this process must look at.
    fn mount_table(&self) -> io::Result<Vec<MountLine>>;

    fn read(&self, p: &Path) -> io::Result<Vec<u8>>;
    fn open(&self, p: &Path) -> io::Result<Box<dyn Read + Send>>;
    fn create(&self, p: &Path, how: Create, mode: u32) -> io::Result<Box<dyn SyncWrite + Send>>;
    fn append(&self, p: &Path, mode: u32) -> io::Result<Box<dyn Write + Send>>;
    fn lstat(&self, p: &Path) -> io::Result<std::fs::Metadata>;
    fn read_link(&self, p: &Path) -> io::Result<PathBuf>;
    fn read_dir(&self, p: &Path) -> io::Result<Vec<OsString>>;
    fn create_dir(&self, p: &Path, mode: u32) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Atomically add a name for a complete file, refusing an existing destination.
    fn hard_link(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this host cannot publish an exclusive hard link",
        ))
    }

    fn remove_file(&self, p: &Path) -> io::Result<()>;
    fn remove_dir_all(&self, p: &Path) -> io::Result<()>;
    fn fsync_dir(&self, p: &Path) -> io::Result<()>;

    /// Flush an existing file before a directory rename publishes it.
    fn fsync_file(&self, p: &Path) -> io::Result<()>;

    fn uname_release(&self) -> io::Result<String>;
    fn now(&self) -> Instant;
    fn wall_ms(&self) -> u64;

    /// Open (creating with `mode` when absent) the fixed lock file behind
    /// `host_path` and return a handle for it. The default implementation
    /// **fails closed**: a host that cannot hold a cross-process lock must not
    /// silently pretend it did.
    fn open_lock(&self, _host_path: &Path, _mode: u32) -> io::Result<Box<dyn HostLock + Send>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this host cannot hold a cross-process lock",
        ))
    }
}

/// Fixed environment of host commands.
pub const HOST_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

/// The internal argument of the namespace mode.
///
/// In the web container every host command is this same binary re-executed as
/// `/proc/self/exe <INTERNAL_NSENTER_ARG> <program> [args…]`: the image carries
/// no `nsenter` of its own, so [`enter_host_namespaces`] enters the host
/// namespaces and `execve`s the target. The argument is an implementation
/// detail — it is not part of the usage text — and the only accepted call form
/// is the one [`build_command`] produces.
pub const INTERNAL_NSENTER_ARG: &str = "--internal-nsenter";

fn build_command(cmd: &Cmd, in_container: bool) -> Command {
    let mut command;
    if in_container && cmd.at == At::Host {
        command = Command::new("/proc/self/exe");
        command.arg(INTERNAL_NSENTER_ARG);
        command.arg(cmd.program);
    } else {
        command = Command::new(cmd.program);
    }
    command.args(&cmd.args);
    command.env_clear();
    command.env("LC_ALL", "C");
    command.env("PATH", HOST_PATH);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.current_dir("/");
    command.process_group(0);
    command
}

/// The exit code of the namespace mode when it cannot reach the target; the
/// code `nsenter` uses, so every caller keeps its classification.
const NSENTER_EXIT_FAILURE: u8 = 1;

/// The namespaces `nsenter -t 1 -m -u -i -n -p` enters, in the order util-linux
/// enters them (its `namespace_files` array order, the mount namespace last).
/// The user namespace is not requested, so the two passes util-linux makes for
/// it collapse into one.
#[cfg(target_os = "linux")]
const HOST_NAMESPACES: [(&str, libc::c_int); 5] = [
    ("ns/ipc", libc::CLONE_NEWIPC),
    ("ns/uts", libc::CLONE_NEWUTS),
    ("ns/net", libc::CLONE_NEWNET),
    ("ns/pid", libc::CLONE_NEWPID),
    ("ns/mnt", libc::CLONE_NEWNS),
];

/// The internal namespace mode: enter the host namespaces of PID 1, then replace this process
/// with `program` (looked up in [`HOST_PATH`]) and `args`.
///
/// It never returns on success: `execve` hands the pipes, the process group, the working
/// directory and the fixed `LC_ALL`/`PATH` environment of [`build_command`] straight to the
/// target, so a host command sees exactly what it saw when the image's `nsenter` ran it.
/// Every failure prints one line starting with `nsenter: ` and ends the process with exit
/// code 1, the shape `nsenter` produced.
pub fn enter_host_namespaces(program: &OsStr, args: &[OsString]) -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    {
        enter_host_namespaces_linux(program, args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // The host namespaces only exist on Linux. No other platform may
        // silently run a host command in this process's own environment.
        let _ = (program, args);
        nsenter_failure(format_args!("entering the host namespaces needs Linux"))
    }
}

#[cfg(target_os = "linux")]
fn enter_host_namespaces_linux(program: &OsStr, args: &[OsString]) -> std::process::ExitCode {
    use std::os::unix::io::AsRawFd;

    // Every descriptor is opened before any namespace is entered, like
    // `nsenter` does: the files are read in this process's own mount
    // namespace, which the mount namespace switch below takes away.
    let mut namespaces = Vec::with_capacity(HOST_NAMESPACES.len());
    for (name, _) in HOST_NAMESPACES {
        let path = format!("/proc/1/{name}");
        match std::fs::File::open(&path) {
            Ok(file) => namespaces.push(file),
            Err(e) => return nsenter_failure(format_args!("cannot open {path}: {e}")),
        }
    }
    for ((name, nstype), file) in HOST_NAMESPACES.iter().zip(namespaces.iter()) {
        let rc = unsafe { libc::setns(file.as_raw_fd(), *nstype) };
        if rc != 0 {
            return nsenter_failure(format_args!(
                "reassociate to namespace '{name}' failed: {}",
                io::Error::last_os_error()
            ));
        }
    }
    // `setns(CLONE_NEWPID)` only affects children, and the compose file runs
    // with `pid: host`, so the pid namespace is this one already: like
    // `nsenter`'s `-p` without its fork, the process goes on to `execve`.
    restore_default_sigpipe();
    let error = exec_on_host_path(program, args);
    nsenter_failure(format_args!(
        "failed to execute {}: {error}",
        program.to_string_lossy()
    ))
}

/// Give the target program the signal dispositions `nsenter`'s child had.
///
/// Rust's runtime ignores `SIGPIPE` while it starts, an ignored signal survives
/// `execve` (a handler does not), and `nsenter` was a C program whose child saw
/// the default. Without this a host command would inherit the ignored
/// `SIGPIPE` of this mode.
#[cfg(target_os = "linux")]
fn restore_default_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// `execve` `program` with `args`, searching [`HOST_PATH`] when the name holds no `/`. It
/// returns only on failure.
///
/// `execv` passes the caller's `environ`, which is exactly the fixed `LC_ALL`/`PATH` of
/// [`build_command`]. There is no `/bin/sh` fallback for a non-executable file: a host
/// command never goes through a shell.
#[cfg(target_os = "linux")]
fn exec_on_host_path(program: &OsStr, args: &[OsString]) -> io::Error {
    use std::os::unix::ffi::OsStrExt;

    let program_c = match CString::new(program.as_bytes()) {
        Ok(c) => c,
        Err(_) => {
            return io::Error::new(
                io::ErrorKind::InvalidInput,
                "the program name holds a NUL byte",
            )
        }
    };
    let mut arg_c = Vec::with_capacity(args.len());
    for arg in args {
        match CString::new(arg.as_bytes()) {
            Ok(c) => arg_c.push(c),
            Err(_) => {
                return io::Error::new(io::ErrorKind::InvalidInput, "an argument holds a NUL byte")
            }
        }
    }
    let mut argv: Vec<*const libc::c_char> = Vec::with_capacity(arg_c.len() + 2);
    argv.push(program_c.as_ptr());
    argv.extend(arg_c.iter().map(|c| c.as_ptr()));
    argv.push(std::ptr::null());

    let candidates: Vec<PathBuf> = if program.as_bytes().contains(&b'/') {
        vec![PathBuf::from(program)]
    } else {
        HOST_PATH
            .split(':')
            .map(|dir| Path::new(dir).join(program))
            .collect()
    };
    // `execvp`'s rule: the search goes on, a failure other than ENOENT is
    // remembered and decides the message when no candidate could run.
    let mut remembered: Option<i32> = None;
    let mut last = libc::ENOENT;
    for candidate in candidates {
        let Ok(path) = CString::new(candidate.as_os_str().as_bytes()) else {
            continue;
        };
        unsafe { libc::execv(path.as_ptr(), argv.as_ptr()) };
        last = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::ENOENT);
        if last != libc::ENOENT && remembered.is_none() {
            remembered = Some(last);
        }
    }
    io::Error::from_raw_os_error(remembered.unwrap_or(last))
}

/// The one failure shape of the namespace mode: an `nsenter: ` line on stderr
/// and exit code 1.
fn nsenter_failure(message: std::fmt::Arguments<'_>) -> std::process::ExitCode {
    eprintln!("nsenter: {message}");
    std::process::ExitCode::from(NSENTER_EXIT_FAILURE)
}

fn exit_of(status: std::process::ExitStatus) -> Exit {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(code) = status.code() {
            Exit::Code(code)
        } else if let Some(signal) = status.signal() {
            Exit::Signal(signal)
        } else {
            Exit::Code(-1)
        }
    }
    #[cfg(not(unix))]
    {
        Exit::Code(status.code().unwrap_or(-1))
    }
}

fn spawn_failed(e: io::Error, started: Instant) -> Output {
    Output {
        exit: Exit::SpawnFailed,
        stdout: Vec::new(),
        stderr: format!("{e}").into_bytes(),
        elapsed: started.elapsed(),
    }
}

fn kill_process_group(child: &mut Child) {
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
}

/// Wait for one reader thread until `deadline`; a thread that still holds a
/// pipe after descendants were killed is detached rather than joined forever.
fn wait_reader_until<T>(handle: std::thread::JoinHandle<T>, deadline: Instant) -> Option<T> {
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().ok()
}

/// Lines are delivered to the watcher in bounded batches, so every drain
/// returns to the deadline check and a command that produces output
/// continuously cannot starve the timeout. Returns true when more lines were
/// still pending.
const DELIVER_BATCH: usize = 256;

fn deliver_pending(rx: &mpsc::Receiver<(Stream, Vec<u8>)>, w: &mut Option<&mut dyn Watch>) -> bool {
    for _ in 0..DELIVER_BATCH {
        match rx.try_recv() {
            Ok((stream, line)) => {
                if let Some(w) = w.as_deref_mut() {
                    w.line(stream, &line);
                }
            }
            Err(_) => return false,
        }
    }
    true
}

/// Run one command: collect all output and, when `w` is given, deliver
/// complete lines and periodic ticks to it while the command runs.
///
/// One core serves both entries. Without a watcher the reader threads never
/// split lines, so a large single output pays for no per-line channel
/// messages. Every loop, including the drains that follow the child, returns
/// to the deadline check, so a command or a descendant that keeps the pipe
/// open and writes continuously cannot extend the command past its deadline;
/// after a forced kill the remaining drain gets one fixed grace period and
/// never runs unbounded, while a command that finished on its own delivers
/// its whole output to the watcher.
fn run_monitored(
    cmd: &Cmd,
    every: Duration,
    mut w: Option<&mut dyn Watch>,
    in_container: bool,
) -> Output {
    let started = Instant::now();
    let deadline = started + cmd.timeout;
    let mut command = build_command(cmd, in_container);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            if let Some(w) = w.as_deref_mut() {
                w.tick();
            }
            return spawn_failed(e, started);
        }
    };
    let (tx, rx) = if w.is_some() {
        let (tx, rx) = mpsc::channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let stdout_all = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_all = Arc::new(Mutex::new(Vec::<u8>::new()));
    let spawn_reader = |stream: Stream,
                        mut src: Box<dyn Read + Send>,
                        tx: Option<mpsc::Sender<(Stream, Vec<u8>)>>,
                        all: Arc<Mutex<Vec<u8>>>|
     -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut line = Vec::new();
            loop {
                match src.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut a) = all.lock() {
                            a.extend_from_slice(&buf[..n]);
                        }
                        if let Some(tx) = tx.as_ref() {
                            for &b in &buf[..n] {
                                if b == b'\n' {
                                    let _ = tx.send((stream, std::mem::take(&mut line)));
                                } else {
                                    line.push(b);
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if let (Some(tx), false) = (tx.as_ref(), line.is_empty()) {
                let _ = tx.send((stream, line));
            }
        })
    };
    let out_thread = child
        .stdout
        .take()
        .map(|s| spawn_reader(Stream::Stdout, Box::new(s), tx.clone(), stdout_all.clone()));
    let err_thread = child
        .stderr
        .take()
        .map(|s| spawn_reader(Stream::Stderr, Box::new(s), tx.clone(), stderr_all.clone()));
    drop(tx);

    let mut next_tick = started + every;
    let exit = loop {
        if let Some(rx) = rx.as_ref() {
            while deliver_pending(rx, &mut w) && Instant::now() < deadline {}
        }
        match child.try_wait() {
            Ok(Some(status)) => break exit_of(status),
            Ok(None) => {}
            Err(_) => {
                kill_process_group(&mut child);
                let _ = child.wait();
                break Exit::SpawnFailed;
            }
        }
        let now = Instant::now();
        if now >= deadline {
            kill_process_group(&mut child);
            let _ = child.wait();
            break Exit::TimedOut;
        }
        if let Some(w) = w.as_deref_mut() {
            if now >= next_tick {
                w.tick();
                next_tick += every;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // The child is gone; the readers may still be delivering buffered data,
    // and a descendant holding the pipe can keep them alive. Waiting for the
    // readers ends at the deadline. After a forced kill the drains end at one
    // fixed grace period; a command that finished on its own delivers every
    // line to the watcher.
    let mut forced = false;
    loop {
        let out_done = out_thread.as_ref().map(|h| h.is_finished()).unwrap_or(true);
        let err_done = err_thread.as_ref().map(|h| h.is_finished()).unwrap_or(true);
        if out_done && err_done {
            break;
        }
        if Instant::now() >= deadline {
            kill_process_group(&mut child);
            let _ = child.wait();
            forced = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let killed = forced || matches!(exit, Exit::TimedOut | Exit::SpawnFailed);
    if killed {
        let grace = Instant::now() + Duration::from_millis(500);
        if let Some(rx) = rx.as_ref() {
            while deliver_pending(rx, &mut w) && Instant::now() < grace {}
        }
        if let Some(t) = out_thread {
            let _ = wait_reader_until(t, grace);
        }
        if let Some(t) = err_thread {
            let _ = wait_reader_until(t, grace);
        }
        if let Some(rx) = rx.as_ref() {
            while deliver_pending(rx, &mut w) && Instant::now() < grace {}
        }
    } else {
        // Both readers have ended, so every sender is gone and the channel
        // holds a finite tail: deliver all of it, however slow the watcher.
        if let Some(t) = out_thread {
            let _ = t.join();
        }
        if let Some(t) = err_thread {
            let _ = t.join();
        }
        if let Some(rx) = rx.as_ref() {
            while deliver_pending(rx, &mut w) {}
        }
    }
    let stdout = stdout_all.lock().map(|v| v.clone()).unwrap_or_default();
    let stderr = stderr_all.lock().map(|v| v.clone()).unwrap_or_default();
    let exit = if forced { Exit::TimedOut } else { exit };
    Output {
        exit,
        stdout,
        stderr,
        elapsed: started.elapsed(),
    }
}

fn read_dir_real(p: &Path) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(p)? {
        names.push(entry?.file_name());
    }
    names.sort();
    Ok(names)
}

fn uname_release_real() -> io::Result<String> {
    let mut buf: libc::utsname = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::uname(&mut buf) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let release = unsafe { std::ffi::CStr::from_ptr(buf.release.as_ptr()) };
    Ok(release.to_string_lossy().into_owned())
}

fn wall_ms_real() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One held-or-attempted `flock` on an open descriptor.
struct FileLock {
    file: std::fs::File,
    held: bool,
}

impl FileLock {
    fn fd(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.file.as_raw_fd()
    }
}

impl HostLock for FileLock {
    fn identity(&self) -> io::Result<LockId> {
        use std::os::unix::fs::MetadataExt;
        let meta = self.file.metadata()?;
        Ok(LockId {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    fn try_acquire(&mut self) -> io::Result<bool> {
        loop {
            let rc = unsafe { libc::flock(self.fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                self.held = true;
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                // A signal must not lose the failure classification.
                Some(libc::EINTR) => continue,
                // Another open file description holds it.
                Some(libc::EWOULDBLOCK) => return Ok(false),
                _ => return Err(error),
            }
        }
    }

    fn release(&mut self) -> io::Result<()> {
        if !self.held {
            return Ok(());
        }
        let rc = unsafe { libc::flock(self.fd(), libc::LOCK_UN) };
        self.held = false;
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if self.held {
            unsafe {
                libc::flock(self.fd(), libc::LOCK_UN);
            }
        }
    }
}

/// Open the fixed lock file and validate the descriptor that is actually held.
///
/// `O_NOFOLLOW` makes the kernel refuse a symlink at the path, and the checks
/// run on `fstat` of that same descriptor, so a path swapped between a
/// pre-check and the open cannot pass. The `mode` only applies when the file is
/// created; an existing file keeps its own mode, which is then validated.
fn open_lock_real(process_path: &Path, mode: u32) -> io::Result<Box<dyn HostLock + Send>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW);
    let file = options.open(process_path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the lock path is not a regular file",
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the lock file is accessible to group or others",
        ));
    }
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the lock file belongs to another user",
        ));
    }
    Ok(Box::new(FileLock { file, held: false }))
}

/// The production host: one type with the one switch that really differs
/// between the two processes. The web process reads host files through
/// `/proc/1/root` and runs host commands through its own namespace mode,
/// never through a program from the image; the runner already lives in the
/// host namespaces.
#[derive(Clone, Copy, Default)]
pub struct RealHost {
    in_container: bool,
}

impl RealHost {
    /// The host of the web process.
    pub fn in_container() -> RealHost {
        RealHost { in_container: true }
    }

    /// The host of the runner.
    pub fn on_host() -> RealHost {
        RealHost {
            in_container: false,
        }
    }
}

impl Host for RealHost {
    fn run(&self, cmd: &Cmd) -> Output {
        run_monitored(cmd, Duration::ZERO, None, self.in_container)
    }

    fn run_watch(&self, cmd: &Cmd, every: Duration, w: &mut dyn Watch) -> Output {
        run_monitored(cmd, every, Some(w), self.in_container)
    }

    fn view(&self, host_path: &Path) -> PathBuf {
        if !self.in_container {
            return host_path.to_path_buf();
        }
        let mut p = PathBuf::from("/proc/1/root");
        p.push(host_path.strip_prefix("/").unwrap_or(host_path));
        p
    }

    fn mount_table(&self) -> io::Result<Vec<MountLine>> {
        if self.in_container {
            let text = std::fs::read_to_string("/proc/1/mountinfo")?;
            Ok(crate::facts::parse_mountinfo(&text)
                .into_iter()
                .map(|m| m.line)
                .collect())
        } else {
            let text = std::fs::read_to_string("/proc/self/mounts")?;
            Ok(crate::facts::parse_mounts(&text))
        }
    }

    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(p)
    }

    fn open(&self, p: &Path) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(std::fs::File::open(p)?))
    }

    fn create(&self, p: &Path, how: Create, mode: u32) -> io::Result<Box<dyn SyncWrite + Send>> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).mode(mode);
        match how {
            Create::New => {
                opts.create_new(true);
            }
            Create::Truncate => {
                opts.create(true).truncate(true);
            }
        }
        Ok(Box::new(opts.open(p)?))
    }

    fn append(&self, p: &Path, mode: u32) -> io::Result<Box<dyn Write + Send>> {
        use std::os::unix::fs::OpenOptionsExt;
        Ok(Box::new(
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(mode)
                .open(p)?,
        ))
    }

    fn lstat(&self, p: &Path) -> io::Result<std::fs::Metadata> {
        std::fs::symlink_metadata(p)
    }

    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        std::fs::read_link(p)
    }

    fn read_dir(&self, p: &Path) -> io::Result<Vec<OsString>> {
        read_dir_real(p)
    }

    fn create_dir(&self, p: &Path, mode: u32) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true).mode(mode);
        b.create(p)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::hard_link(from, to)
    }

    fn remove_file(&self, p: &Path) -> io::Result<()> {
        std::fs::remove_file(p)
    }

    fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
        std::fs::remove_dir_all(p)
    }

    fn fsync_dir(&self, p: &Path) -> io::Result<()> {
        let dir = std::fs::File::open(p)?;
        dir.sync_all()
    }

    fn fsync_file(&self, p: &Path) -> io::Result<()> {
        let file = std::fs::File::open(p)?;
        file.sync_all()
    }

    fn uname_release(&self) -> io::Result<String> {
        uname_release_real()
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wall_ms(&self) -> u64 {
        wall_ms_real()
    }

    fn open_lock(&self, host_path: &Path, mode: u32) -> io::Result<Box<dyn HostLock + Send>> {
        if self.in_container {
            // `host_path` is a host path; this process reaches it through the
            // host root it already uses for every other host file.
            open_lock_real(&self.view(host_path), mode)
        } else {
            open_lock_real(host_path, mode)
        }
    }
}

/// Read `/proc/self/exe` to find the data directory of the runner.
pub fn data_dir_from_exe() -> io::Result<PathBuf> {
    let exe = std::fs::read_link("/proc/self/exe")?;
    data_dir_from_exe_path(&exe)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no data directory"))
}

/// The runner lives at `<data>/manager/bin/<exe>`; the data directory is the grandparent of
/// `bin`. The two intermediate names are checked so a binary staged elsewhere cannot silently
/// point at the wrong root.
pub fn data_dir_from_exe_path(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    let manager = bin.parent()?;
    let root = manager.parent()?;
    if bin.file_name()? != "bin" || manager.file_name()? != "manager" {
        return None;
    }
    Some(root.to_path_buf())
}

/// The host path of a file the container holds under its data directory.
///
/// `host_data_dir` is the mapping the start-up check confirmed
/// (`Startup::host_data_dir`), so `/var/lib/nvm/<rest>` becomes
/// `<host_data_dir>/<rest>`. `None` means the mapping is unknown (the check did
/// not confirm it) or the path is not under the container data directory; the
/// caller refuses then instead of guessing a host path.
pub fn host_path_of_data_dir(
    host_data_dir: Option<&Path>,
    container_path: &Path,
) -> Option<PathBuf> {
    let rest = container_path.strip_prefix(CONTAINER_DATA_DIR).ok()?;
    Some(host_data_dir?.join(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_container_host_command_is_re_executed_in_the_internal_mode() {
        let cmd = Cmd::new(At::Host, "docker", Duration::from_secs(5)).arg("inspect");
        let command = build_command(&cmd, true);
        assert_eq!(command.get_program(), OsStr::new("/proc/self/exe"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new(INTERNAL_NSENTER_ARG),
                OsStr::new("docker"),
                OsStr::new("inspect"),
            ]
        );
        // The environment and the working directory of the re-executed process
        // are the ones every host command had.
        let mut env: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        env.sort();
        assert_eq!(
            env,
            vec![
                ("LC_ALL".to_string(), Some("C".to_string())),
                ("PATH".to_string(), Some(HOST_PATH.to_string())),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/")));
    }

    #[test]
    fn only_a_container_host_command_takes_the_internal_mode() {
        // `At::Here` is the caller's own environment, and the runner is already
        // on the host: neither is wrapped.
        let here = build_command(
            &Cmd::new(At::Here, "/bin/echo", Duration::from_secs(5)).arg("x"),
            true,
        );
        assert_eq!(here.get_program(), OsStr::new("/bin/echo"));
        assert_eq!(here.get_args().collect::<Vec<_>>(), vec![OsStr::new("x")]);
        let runner = build_command(&Cmd::new(At::Host, "docker", Duration::from_secs(5)), false);
        assert_eq!(runner.get_program(), OsStr::new("docker"));
        assert_eq!(runner.get_args().count(), 0);
    }

    #[test]
    fn the_data_directory_mapping_needs_a_host_root_and_a_container_path() {
        let host_root = Path::new("/mnt/<pool>/data");
        assert_eq!(
            host_path_of_data_dir(
                Some(host_root),
                Path::new("/var/lib/nvm/manager/upload/g/.incoming-1")
            ),
            Some(PathBuf::from(
                "/mnt/<pool>/data/manager/upload/g/.incoming-1"
            ))
        );
        // A mapping the start-up check did not confirm, a path outside the data
        // directory and a name that only starts like it are all "no guess".
        assert_eq!(
            host_path_of_data_dir(None, Path::new("/var/lib/nvm/x")),
            None
        );
        assert_eq!(
            host_path_of_data_dir(Some(host_root), Path::new("/etc/passwd")),
            None
        );
        assert_eq!(
            host_path_of_data_dir(Some(host_root), Path::new("/var/lib/nvmx/y")),
            None
        );
    }

    #[test]
    fn run_captures_output_and_exit_code() {
        let h = RealHost::on_host();
        let out = h.run(
            &Cmd::new(At::Here, "/bin/sh", Duration::from_secs(5))
                .arg("-c")
                .arg("printf out; printf err >&2; exit 3"),
        );
        assert_eq!(out.exit, Exit::Code(3));
        assert_eq!(out.stdout_text(), "out");
        assert_eq!(out.stderr_text(), "err");
    }

    #[test]
    fn run_times_out_and_kills_the_process_group() {
        let h = RealHost::on_host();
        let out = h.run(
            &Cmd::new(At::Here, "/bin/sh", Duration::from_millis(200))
                .arg("-c")
                .arg("sleep 30"),
        );
        assert_eq!(out.exit, Exit::TimedOut);
        assert!(out.elapsed < Duration::from_secs(5));
    }

    #[test]
    fn environment_is_minimal() {
        let h = RealHost::on_host();
        let out = h.run(
            &Cmd::new(At::Here, "/bin/sh", Duration::from_secs(5))
                .arg("-c")
                .arg("env"),
        );
        let text = out.stdout_text();
        assert!(text.contains("LC_ALL=C"), "{text}");
        assert!(
            text.contains("PATH=/usr/sbin:/usr/bin:/sbin:/bin"),
            "{text}"
        );
        assert!(!text.contains("HOME="), "{text}");
    }

    #[test]
    fn argv_never_goes_through_a_shell() {
        let h = RealHost::on_host();
        let out = h.run(
            &Cmd::new(At::Here, "/bin/echo", Duration::from_secs(5))
                .arg("a;b")
                .arg("$(id)"),
        );
        assert_eq!(out.stdout_text().trim_end(), "a;b $(id)");
    }

    #[test]
    fn spawn_failure_is_reported() {
        let h = RealHost::on_host();
        let out = h.run(&Cmd::new(
            At::Here,
            "/nonexistent-nvm",
            Duration::from_secs(1),
        ));
        assert_eq!(out.exit, Exit::SpawnFailed);
    }

    #[test]
    fn raw_tail_keeps_the_last_lines() {
        let out = Output {
            exit: Exit::Code(1),
            stdout: b"a\nb\nc\n".to_vec(),
            stderr: b"d\ne\n".to_vec(),
            elapsed: Duration::ZERO,
        };
        assert_eq!(out.raw_tail(2), "d\ne");
    }

    struct Collect {
        lines: Vec<String>,
        ticks: usize,
    }

    impl Watch for Collect {
        fn line(&mut self, _from: Stream, line: &[u8]) {
            self.lines.push(String::from_utf8_lossy(line).into_owned());
        }
        fn tick(&mut self) {
            self.ticks += 1;
        }
    }

    #[test]
    fn watch_delivers_lines_and_ticks() {
        let h = RealHost::on_host();
        let mut w = Collect {
            lines: Vec::new(),
            ticks: 0,
        };
        let out = h.run_watch(
            &Cmd::new(At::Here, "/bin/sh", Duration::from_secs(5))
                .arg("-c")
                .arg("printf 'one\\ntwo\\n'; sleep 0.15; printf three"),
            Duration::from_millis(50),
            &mut w,
        );
        assert!(out.exit.ok());
        assert_eq!(w.lines, vec!["one", "two", "three"]);
        assert!(w.ticks >= 1, "ticks={}", w.ticks);
    }

    #[test]
    fn data_dir_from_exe_path_takes_the_grandparent_of_bin() {
        let root = data_dir_from_exe_path(Path::new(
            "/mnt/<pool>/data/manager/bin/truenas-nvidia-driver-manager",
        ))
        .unwrap();
        assert_eq!(root, PathBuf::from("/mnt/<pool>/data"));
        // A binary staged outside the documented layout must not be guessed.
        assert!(
            data_dir_from_exe_path(Path::new("/tmp/stage/truenas-nvidia-driver-manager")).is_none()
        );
        assert!(data_dir_from_exe_path(Path::new(
            "/mnt/<pool>/data/other/bin/truenas-nvidia-driver-manager"
        ))
        .is_none());
    }

    #[test]
    fn a_descendant_holding_the_pipe_cannot_extend_the_deadline() {
        let h = RealHost::on_host();
        let out = h.run(
            &Cmd::new(At::Here, "/bin/sh", Duration::from_millis(50))
                .arg("-c")
                .arg("sleep 0.4 & exit 0"),
        );
        assert!(
            out.elapsed < Duration::from_millis(200),
            "deadline 50ms; elapsed {:?}, exit {:?}",
            out.elapsed,
            out.exit
        );
    }

    /// A watcher that needs far longer per line than the producer makes the
    /// pending queue grow: a drain that does not return to the deadline check
    /// between batches would never reach the timeout, and the collected
    /// output would keep growing with it.
    struct SlowWatch {
        lines: usize,
    }

    impl Watch for SlowWatch {
        fn line(&mut self, _from: Stream, _line: &[u8]) {
            self.lines += 1;
            std::thread::sleep(Duration::from_micros(100));
        }
        fn tick(&mut self) {}
    }

    #[test]
    fn continuous_output_cannot_starve_the_deadline() {
        let h = RealHost::on_host();
        let mut w = SlowWatch { lines: 0 };
        let out = h.run_watch(
            &Cmd::new(At::Here, "/bin/sh", Duration::from_millis(200))
                .arg("-c")
                .arg("while :; do printf 'line\n'; done"),
            Duration::from_millis(50),
            &mut w,
        );
        assert_eq!(out.exit, Exit::TimedOut);
        assert!(
            out.elapsed < Duration::from_secs(2),
            "elapsed {:?}, lines delivered {}",
            out.elapsed,
            w.lines
        );
        assert!(w.lines > 0, "lines must still be delivered");
        // Without a watcher the same flood is collected in one piece and is
        // bounded by what the command produced before the kill.
        let plain = h.run(
            &Cmd::new(At::Here, "/bin/sh", Duration::from_millis(200))
                .arg("-c")
                .arg("while :; do printf 'line\n'; done"),
        );
        assert_eq!(plain.exit, Exit::TimedOut);
        assert!(
            plain.elapsed < Duration::from_secs(2),
            "elapsed {:?}",
            plain.elapsed
        );
        assert!(
            plain.stdout.len() < 64 * 1024 * 1024,
            "collected {} bytes",
            plain.stdout.len()
        );
    }

    #[test]
    fn a_finished_command_delivers_every_line_to_a_slow_watcher() {
        // The direct child exits at once; a descendant writes the lines a
        // moment later, so they all queue up behind a slow watcher after the
        // exit. None may be dropped.
        let h = RealHost::on_host();
        let mut w = SlowWatch { lines: 0 };
        let out = h.run_watch(
            &Cmd::new(At::Here, "/bin/sh", Duration::from_secs(30))
                .arg("-c")
                .arg("(sleep 0.05; seq 10000) &"),
            Duration::from_millis(50),
            &mut w,
        );
        assert_eq!(out.exit, Exit::Code(0));
        assert_eq!(out.stdout.iter().filter(|&&b| b == b'\n').count(), 10000);
        assert_eq!(w.lines, 10000, "elapsed {:?}", out.elapsed);
    }

    #[test]
    fn uname_release_is_non_empty() {
        let h = RealHost::on_host();
        assert!(!h.uname_release().unwrap().is_empty());
    }
}

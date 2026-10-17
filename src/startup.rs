//! Startup validation of host access and the data-directory mapping.
//!
//! At startup, check the PID namespace, discover the container identity, inspect its
//! configuration and verify that the container and host read the same data directory.

use std::path::{Path, PathBuf};

use crate::codes::*;
use crate::host::{At, Cmd, Host};
use crate::store::Paths;

/// Why the data directory mapping check did not pass (closed set of startup failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingReason {
    NoHostnameLine,
    NsenterFailed,
    InspectFailed {
        reason: crate::task_state::ReasonValue,
        raw: Option<String>,
    },
    MountCount {
        count: u64,
    },
    MountUnder {
        mount_point: String,
    },
    PathCharacters,
    CheckWriteFailed,
    CheckReadFailed {
        reason: crate::task_state::ReasonValue,
        raw: Option<String>,
    },
    CheckMismatch,
}

impl MappingReason {
    /// The nested `{key, params}` object rendered inside startup failure.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            MappingReason::NoHostnameLine => serde_json::json!({
                "key": "startup.mapping.no_hostname_line", "params": {}
            }),
            MappingReason::NsenterFailed => serde_json::json!({
                "key": "startup.mapping.nsenter_failed", "params": {}
            }),
            MappingReason::InspectFailed { reason, .. } => serde_json::json!({
                "key": "startup.mapping.inspect_failed",
                "params": {
                    "read_reason": {"key": reason.key, "params": reason.params},
                }
            }),
            MappingReason::MountCount { count } => serde_json::json!({
                "key": "startup.mapping.mount_count", "params": { "mount_count": count }
            }),
            MappingReason::MountUnder { mount_point } => serde_json::json!({
                "key": "startup.mapping.mount_under", "params": { "mount_point": mount_point }
            }),
            MappingReason::PathCharacters => serde_json::json!({
                "key": "startup.mapping.path_characters", "params": {}
            }),
            MappingReason::CheckWriteFailed => serde_json::json!({
                "key": "startup.mapping.check_write_failed", "params": {}
            }),
            MappingReason::CheckReadFailed { reason, .. } => serde_json::json!({
                "key": "startup.mapping.check_read_failed",
                "params": {
                    "read_reason": {"key": reason.key, "params": reason.params},
                }
            }),
            MappingReason::CheckMismatch => serde_json::json!({
                "key": "startup.mapping.check_mismatch", "params": {}
            }),
        }
    }

    pub fn raw(&self) -> Option<&str> {
        match self {
            MappingReason::InspectFailed { raw, .. }
            | MappingReason::CheckReadFailed { raw, .. } => raw.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupProblem {
    PidHost,
    Runtime { value: Option<String> },
    DataMapping { reason: MappingReason },
}

impl StartupProblem {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            StartupProblem::PidHost => {
                serde_json::json!({ "key": "startup.pid_host", "params": {} })
            }
            StartupProblem::Runtime { value } => serde_json::json!({
                "key": "startup.runtime",
                "params": { "runtime": value.clone().unwrap_or_else(|| "UNKNOWN".into()) }
            }),
            StartupProblem::DataMapping { reason } => serde_json::json!({
                "key": "startup.data_mapping",
                "params": { "mapping_reason": reason.to_json() }
            }),
        }
    }

    /// The login status response and structured startup log use the same reason shape.
    pub fn log_reason_json(&self) -> serde_json::Value {
        self.to_json()
    }

    pub fn raw(&self) -> Option<&str> {
        match self {
            StartupProblem::DataMapping { reason } => reason.raw(),
            _ => None,
        }
    }

    /// The `check` field of `docker logs startup_check_failed`.
    pub fn log_check(&self) -> &'static str {
        match self {
            StartupProblem::PidHost => "pid_host",
            StartupProblem::Runtime { .. } => "runtime",
            StartupProblem::DataMapping { .. } => "data_mapping",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Startup {
    pub problems: Vec<StartupProblem>,
    pub pid_host_ok: bool,
    pub host_data_dir: Option<PathBuf>,
    pub image_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct InspectEntry {
    #[serde(rename = "Image")]
    image: Option<String>,
    #[serde(rename = "HostConfig")]
    host_config: Option<InspectHostConfig>,
    #[serde(rename = "Mounts")]
    mounts: Option<Vec<InspectMount>>,
}

#[derive(serde::Deserialize)]
struct InspectHostConfig {
    #[serde(rename = "Runtime")]
    runtime: Option<String>,
}

#[derive(serde::Deserialize)]
struct InspectMount {
    #[serde(rename = "Source")]
    source: Option<String>,
    #[serde(rename = "Destination")]
    destination: Option<String>,
}

/// namespace check: the two mount namespace links are readable and different.
pub fn check_pid_host(h: &dyn Host) -> bool {
    let a = h.read_link(Path::new("/proc/1/ns/mnt"));
    let b = h.read_link(Path::new("/proc/self/ns/mnt"));
    match (a, b) {
        (Ok(a), Ok(b)) => a != b,
        _ => false,
    }
}

/// The container id of one mountinfo root, when the root has the `…/containers/<64 lowercase
/// hex>/hostname` shape.
fn container_id_in_root(root: &str) -> Option<String> {
    let pos = root.find("/containers/")?;
    let rest = &root[pos + "/containers/".len()..];
    let end = rest.find('/')?;
    // The root shape ends at `/hostname`; any other suffix is not the host's hostname bind
    // mount.
    if &rest[end + 1..] != "hostname" {
        return None;
    }
    let id = &rest[..end];
    if id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Some(id.to_string());
    }
    None
}

/// container identity check: find the container id in the container's own mountinfo.
///
/// The quantifier is *exactly one* `/etc/hostname` row: one matching row yields the id, no
/// row yields `None`, and a second row makes the answer unknown instead of picking one of
/// them. A single row that does not match the root also yields `None`.
pub fn container_id_from_mountinfo(mountinfo: &str) -> Option<String> {
    let mut id: Option<String> = None;
    let mut rows = 0u32;
    for line in crate::facts::parse_mountinfo(mountinfo) {
        if line.line.mount_point != "/etc/hostname" {
            continue;
        }
        rows += 1;
        if rows > 1 {
            return None;
        }
        id = container_id_in_root(&line.root);
    }
    if rows == 1 {
        id
    } else {
        None
    }
}

/// The data directory host path is safe to pass to host commands.
pub fn data_dir_path_ok(s: &str) -> bool {
    if !s.starts_with('/') {
        return false;
    }
    s.chars().all(|c| {
        let code = c as u32;
        !(code <= 0x1f || code == 0x7f || c == ',' || c == '"')
    })
}

fn image_id_ok(s: &str) -> bool {
    match s.strip_prefix("sha256:") {
        // The approved closed set is lowercase (`^sha256:[0-9a-f]{64}$`).
        Some(hex) => {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        None => false,
    }
}

/// The command-failure reason of container inspection/data-directory readback with its
/// placeholders.
fn command_reason(out: &crate::host::Output) -> crate::task_state::ReasonValue {
    match out.exit {
        crate::host::Exit::TimedOut => crate::task_state::ReasonValue::new(
            REASON_TIMED_OUT,
            serde_json::json!({"seconds": HOST_QUERY_TIMEOUT.as_secs()}),
        ),
        crate::host::Exit::SpawnFailed => {
            crate::task_state::ReasonValue::new(REASON_SPAWN_FAILED, serde_json::json!({}))
        }
        crate::host::Exit::Code(code) => crate::task_state::ReasonValue::new(
            REASON_EXIT_STATUS,
            serde_json::json!({"code": code}),
        ),
        crate::host::Exit::Signal(signal) => crate::task_state::ReasonValue::new(
            REASON_SIGNAL,
            serde_json::json!({"signal": signal}),
        ),
    }
}

/// Run namespace check–data-directory readback once.
pub fn check(h: &dyn Host, p: &Paths) -> Startup {
    let pid_host_ok = check_pid_host(h);
    if !pid_host_ok {
        return Startup {
            problems: vec![StartupProblem::PidHost],
            pid_host_ok: false,
            host_data_dir: None,
            image_id: None,
        };
    }

    // container identity check
    let mountinfo = match h.read(Path::new("/proc/self/mountinfo")) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    };
    let Some(container_id) = container_id_from_mountinfo(&mountinfo) else {
        return Startup {
            problems: vec![StartupProblem::DataMapping {
                reason: MappingReason::NoHostnameLine,
            }],
            pid_host_ok: true,
            host_data_dir: None,
            image_id: None,
        };
    };

    // container inspection
    let out = h.run(
        &Cmd::new(At::Host, "docker", HOST_QUERY_TIMEOUT)
            .arg("inspect")
            .arg(container_id),
    );
    if !out.exit.ok() {
        // Only the shape is the nsenter failure: exit code 1 and the stderr text starting
        // exactly with `nsenter:`. No trimming: leading whitespace is a different answer.
        // Everything else keeps the inspect_failed reason and the raw text.
        let stderr = out.stderr_text();
        if matches!(out.exit, crate::host::Exit::Code(1)) && out.stderr.starts_with(b"nsenter:") {
            return Startup {
                problems: vec![StartupProblem::DataMapping {
                    reason: MappingReason::NsenterFailed,
                }],
                pid_host_ok: true,
                host_data_dir: None,
                image_id: None,
            };
        }
        let reason = MappingReason::InspectFailed {
            reason: command_reason(&out),
            raw: Some(stderr),
        };
        return Startup {
            problems: vec![StartupProblem::DataMapping { reason }],
            pid_host_ok: true,
            host_data_dir: None,
            image_id: None,
        };
    }
    let entries: Vec<InspectEntry> = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => {
            return Startup {
                problems: vec![StartupProblem::DataMapping {
                    reason: MappingReason::InspectFailed {
                        reason: crate::task_state::ReasonValue::new(
                            REASON_FORMAT_UNRECOGNIZED,
                            serde_json::json!({}),
                        ),
                        // Unrecognized inspect output can include the container environment.
                        // Keep the failure and drop that text.
                        raw: None,
                    },
                }],
                pid_host_ok: true,
                host_data_dir: None,
                image_id: None,
            };
        }
    };
    let Some(entry) = entries.into_iter().next() else {
        return Startup {
            problems: vec![StartupProblem::DataMapping {
                reason: MappingReason::InspectFailed {
                    reason: crate::task_state::ReasonValue::new(
                        REASON_FORMAT_UNRECOGNIZED,
                        serde_json::json!({}),
                    ),
                    raw: None,
                },
            }],
            pid_host_ok: true,
            host_data_dir: None,
            image_id: None,
        };
    };

    let mut problems = Vec::new();
    let runtime = entry.host_config.as_ref().and_then(|c| c.runtime.clone());
    if runtime.as_deref() != Some("runc") {
        problems.push(StartupProblem::Runtime { value: runtime });
    }

    let mounts = entry.mounts.unwrap_or_default();
    let matching: Vec<&InspectMount> = mounts
        .iter()
        .filter(|m| m.destination.as_deref() == Some(CONTAINER_DATA_DIR))
        .collect();
    let mut host_data_dir = None;
    if matching.len() != 1 {
        problems.push(StartupProblem::DataMapping {
            reason: MappingReason::MountCount {
                count: matching.len() as u64,
            },
        });
    } else {
        let source = matching[0].source.clone().unwrap_or_default();
        if !data_dir_path_ok(&source) {
            problems.push(StartupProblem::DataMapping {
                reason: MappingReason::PathCharacters,
            });
        } else {
            host_data_dir = Some(PathBuf::from(source));
        }
    }
    for m in &mounts {
        if let Some(dest) = &m.destination {
            if dest.starts_with(&format!("{CONTAINER_DATA_DIR}/")) {
                problems.push(StartupProblem::DataMapping {
                    reason: MappingReason::MountUnder {
                        mount_point: dest.clone(),
                    },
                });
            }
        }
    }

    let image_id = match entry.image {
        Some(id) if image_id_ok(&id) => Some(id),
        Some(_) => {
            problems.push(StartupProblem::DataMapping {
                reason: MappingReason::InspectFailed {
                    reason: crate::task_state::ReasonValue::new(
                        REASON_FORMAT_UNRECOGNIZED,
                        serde_json::json!({}),
                    ),
                    raw: None,
                },
            });
            None
        }
        None => {
            problems.push(StartupProblem::DataMapping {
                reason: MappingReason::InspectFailed {
                    reason: crate::task_state::ReasonValue::new(
                        REASON_FORMAT_UNRECOGNIZED,
                        serde_json::json!({}),
                    ),
                    raw: None,
                },
            });
            None
        }
    };

    // data-directory readback, only when the path is usable.
    if let Some(dir) = host_data_dir.clone() {
        if problems
            .iter()
            .all(|p| !matches!(p, StartupProblem::DataMapping { .. }))
        {
            match check_readback(h, p, &dir) {
                Ok(()) => {}
                Err(reason) => problems.push(StartupProblem::DataMapping { reason }),
            }
        }
        if !data_dir_path_ok(&dir.to_string_lossy()) {
            host_data_dir = None;
        }
    }

    Startup {
        problems,
        pid_host_ok: true,
        host_data_dir,
        image_id,
    }
}

/// data-directory readback: write a random value in the container, read it back through the
/// host path, and remove the check file.
fn check_readback(h: &dyn Host, p: &Paths, host_dir: &Path) -> Result<(), MappingReason> {
    let container_path = p.startup_check();
    let _ = h.remove_file(&container_path);
    let value = crate::store::random_hex(16);
    match h.create(&container_path, crate::host::Create::New, 0o600) {
        Ok(mut f) => {
            use std::io::Write;
            if f.write_all(value.as_bytes()).is_err() || f.sync_all().is_err() {
                drop(f);
                let _ = h.remove_file(&container_path);
                return Err(MappingReason::CheckWriteFailed);
            }
            drop(f);
        }
        Err(_) => {
            return Err(MappingReason::CheckWriteFailed);
        }
    }
    let host_path = host_dir.join("manager").join(".startup-check");
    let out = h.run(&Cmd::new(At::Host, "cat", HOST_QUERY_TIMEOUT).arg(host_path.as_os_str()));
    let _ = h.remove_file(&container_path);
    if !out.exit.ok() {
        return Err(MappingReason::CheckReadFailed {
            reason: command_reason(&out),
            raw: Some(out.stderr_text()),
        });
    }
    // data-directory readback requires the read-back content to be byte-identical to what the
    // container wrote.
    if out.stdout != value.as_bytes() {
        return Err(MappingReason::CheckMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_id_from_mountinfo_works() {
        let id = "a".repeat(64);
        let mountinfo = format!(
            "1 0 0:1 / / rw,relatime - tmpfs tmpfs rw\n\
             25 1 0:30 /var/lib/docker/containers/{id}/hostname /etc/hostname rw,relatime - ext4 /dev/sda1 rw\n",
        );
        assert_eq!(container_id_from_mountinfo(&mountinfo), Some(id.clone()));
        assert_eq!(
            container_id_from_mountinfo("1 0 0:1 / / rw - tmpfs tmpfs rw\n"),
            None
        );
        // The root ends at `/hostname`; another suffix is not the container's hostname bind
        // mount.
        let wrong_suffix = format!(
            "25 1 0:30 /var/lib/docker/containers/{id}/not-hostname /etc/hostname rw - ext4 /dev/sda1 rw\n",
        );
        assert_eq!(container_id_from_mountinfo(&wrong_suffix), None);
        let uppercase_id = "A".repeat(64);
        let uppercase = format!(
            "25 1 0:30 /var/lib/docker/containers/{uppercase_id}/hostname /etc/hostname rw - ext4 /dev/sda1 rw\n",
        );
        assert_eq!(container_id_from_mountinfo(&uppercase), None);
    }

    #[test]
    fn the_image_id_closed_set_is_lowercase() {
        assert!(image_id_ok(&format!("sha256:{}", "a".repeat(64))));
        assert!(image_id_ok(&format!("sha256:{}", "0".repeat(64))));
        assert!(!image_id_ok(&format!("sha256:{}", "A".repeat(64))));
        assert!(!image_id_ok(&format!("sha256:{}", "a".repeat(63))));
        assert!(!image_id_ok(&format!("SHA256:{}", "a".repeat(64))));
        assert!(!image_id_ok("sha256:"));
    }

    #[test]
    fn data_dir_path_rules() {
        assert!(data_dir_path_ok("/mnt/<pool>/data"));
        assert!(data_dir_path_ok("/mnt/<pool>/my pool/$weird"));
        assert!(!data_dir_path_ok("relative/path"));
        assert!(!data_dir_path_ok(&format!("/mnt/<pool>/a{}b", ',')));
        assert!(!data_dir_path_ok(&format!("/mnt/<pool>/a{}b", '"')));
        assert!(!data_dir_path_ok(&format!("/mnt/<pool>/a{}b", '\t')));
    }

    #[test]
    fn timeout_reason_carries_the_approved_parameters() {
        let reason = command_reason(&crate::host::Output {
            exit: crate::host::Exit::TimedOut,
            stdout: Vec::new(),
            stderr: Vec::new(),
            elapsed: std::time::Duration::ZERO,
        });
        assert_eq!(reason.key, REASON_TIMED_OUT);
        assert_eq!(
            reason.params,
            serde_json::json!({"seconds": HOST_QUERY_TIMEOUT.as_secs()})
        );
        let reason = command_reason(&crate::host::Output {
            exit: crate::host::Exit::Code(7),
            stdout: Vec::new(),
            stderr: Vec::new(),
            elapsed: std::time::Duration::ZERO,
        });
        assert_eq!(reason.key, REASON_EXIT_STATUS);
        assert_eq!(reason.params, serde_json::json!({"code": 7}));
    }

    #[test]
    fn mapping_reason_json_shape() {
        let v = MappingReason::MountCount { count: 2 }.to_json();
        assert_eq!(v["key"], "startup.mapping.mount_count");
        assert_eq!(v["params"]["mount_count"], 2);
        let v = MappingReason::CheckReadFailed {
            reason: crate::task_state::ReasonValue::new(REASON_FILE_MISSING, serde_json::json!({})),
            raw: Some("cat: no such file".into()),
        }
        .to_json();
        assert_eq!(v["params"]["read_reason"]["key"], "reason.file_missing");
    }
}

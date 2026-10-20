//! Objects that requested the GPU.
//!
//! Docker containers and TrueNAS container instances are queried independently; one failing category
//! does not stop the other.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::codes::*;
use crate::host::{At, Cmd, Host};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserKind {
    Docker,
    #[serde(rename = "incus")]
    TrueNasContainer,
}

impl UserKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UserKind::Docker => "docker",
            UserKind::TrueNasContainer => "incus",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuUser {
    pub name: String,
    pub kind: UserKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryError {
    pub kind: UserKind,
    pub reason: crate::task_state::ReasonValue,
}

/// The JSON shape used in GPU occupancy, unload/restore failures and version-change lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuUserJson {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuUserErrorJson {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub reason: crate::task_state::ReasonValue,
}

impl From<&GpuUser> for GpuUserJson {
    fn from(u: &GpuUser) -> GpuUserJson {
        GpuUserJson {
            name: u.name.clone(),
            kind: u.kind.as_str(),
        }
    }
}

impl From<&CategoryError> for GpuUserErrorJson {
    fn from(e: &CategoryError) -> GpuUserErrorJson {
        GpuUserErrorJson {
            kind: e.kind.as_str(),
            reason: e.reason.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuUsers {
    pub users: Vec<GpuUser>,
    pub errors: Vec<CategoryError>,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceRequest {
    pub driver: Option<String>,
    pub capabilities: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceMapping {
    pub path_on_host: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct InspectedContainer {
    pub name: String,
    pub env: Vec<String>,
    pub device_requests: Vec<DeviceRequest>,
    pub devices: Vec<DeviceMapping>,
    /// `by-path` link target file names resolved by the caller.
    pub by_path_targets: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct RawInspect {
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Config")]
    config: Option<RawConfig>,
    #[serde(rename = "HostConfig")]
    host_config: Option<RawHostConfig>,
}

#[derive(Deserialize)]
struct RawConfig {
    #[serde(rename = "Env")]
    env: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RawHostConfig {
    #[serde(rename = "DeviceRequests")]
    device_requests: Option<Vec<RawDeviceRequest>>,
    #[serde(rename = "Devices")]
    devices: Option<Vec<RawDevice>>,
}

#[derive(Deserialize)]
struct RawDeviceRequest {
    #[serde(rename = "Driver")]
    driver: Option<String>,
    #[serde(rename = "Capabilities")]
    capabilities: Option<Vec<Vec<String>>>,
}

#[derive(Deserialize)]
struct RawDevice {
    #[serde(rename = "PathOnHost")]
    path_on_host: Option<String>,
}

/// Parse `docker inspect` output; every field may be null.
pub fn parse_inspect(json: &[u8]) -> Result<Vec<InspectedContainer>, String> {
    let raw: Vec<RawInspect> = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .map(|c| InspectedContainer {
            name: c.name.unwrap_or_default(),
            env: c.config.and_then(|c| c.env).unwrap_or_default(),
            device_requests: c
                .host_config
                .as_ref()
                .and_then(|h| h.device_requests.as_ref())
                .map(|list| {
                    list.iter()
                        .map(|r| DeviceRequest {
                            driver: r.driver.clone(),
                            capabilities: r.capabilities.clone().unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            devices: c
                .host_config
                .and_then(|h| h.devices)
                .map(|list| {
                    list.into_iter()
                        .map(|d| DeviceMapping {
                            path_on_host: d.path_on_host,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            by_path_targets: BTreeMap::new(),
        })
        .collect())
}

/// The last value of an environment variable in the container's list.
fn last_env<'a>(env: &'a [String], name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=");
    env.iter().rev().find_map(|e| e.strip_prefix(&prefix))
}

/// The four rules of for one Docker container (pure function).
pub fn docker_requests_gpu(c: &InspectedContainer, nvidia_dri: &BTreeSet<String>) -> bool {
    for req in &c.device_requests {
        if req.driver.as_deref() == Some("nvidia") {
            return true;
        }
        if req
            .capabilities
            .iter()
            .any(|inner| inner.iter().any(|cap| cap == "gpu"))
        {
            return true;
        }
    }
    if let Some(value) = last_env(&c.env, "NVIDIA_VISIBLE_DEVICES") {
        let value = value.trim();
        if !value.is_empty() && value != "void" {
            return true;
        }
    }
    for d in &c.devices {
        let Some(path) = &d.path_on_host else {
            continue;
        };
        if path.starts_with("/dev/nvidia") {
            return true;
        }
        if path == "/dev/dri" {
            if !nvidia_dri.is_empty() {
                return true;
            }
        } else if let Some(node) = path.strip_prefix("/dev/dri/by-path/") {
            if let Some(target) = c.by_path_targets.get(node) {
                let last = target.rsplit('/').next().unwrap_or(target);
                if nvidia_dri.contains(last) {
                    return true;
                }
            }
        } else if let Some(node) = path.strip_prefix("/dev/dri/") {
            if nvidia_dri.contains(node) {
                return true;
            }
        }
    }
    false
}

/// DRM node reading: the set of DRM nodes whose device driver is `nvidia`.
pub fn nvidia_dri_nodes(h: &dyn Host) -> Result<BTreeSet<String>, &'static str> {
    let dir = Path::new("/sys/class/drm");
    let names = h.read_dir(dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => REASON_FILE_MISSING,
        std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
        _ => REASON_IO_ERROR,
    })?;
    let mut out = BTreeSet::new();
    let mut any_readable = false;
    for name in names {
        let name = name.to_string_lossy().into_owned();
        let is_node = (name.starts_with("card")
            && name["card".len()..].bytes().all(|b| b.is_ascii_digit())
            && name.len() > 4)
            || (name.starts_with("renderD")
                && name["renderD".len()..].bytes().all(|b| b.is_ascii_digit())
                && name.len() > 7);
        if !is_node {
            continue;
        }
        let link = dir.join(&name).join("device").join("driver");
        match h.read_link(&link) {
            Ok(target) => {
                any_readable = true;
                if target.file_name().map(|n| n == "nvidia").unwrap_or(false) {
                    out.insert(name);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(REASON_IO_ERROR),
        }
    }
    let _ = any_readable;
    Ok(out)
}

/// The Docker category error for a failed command: the same command-failure classification as
/// elsewhere, with the query timeout as the seconds placeholder.
pub fn category_error(
    kind: UserKind,
    out: &crate::host::Output,
    timeout: Duration,
) -> CategoryError {
    use crate::host::Exit;
    let reason = match out.exit {
        Exit::TimedOut => crate::task_state::ReasonValue::new(
            REASON_TIMED_OUT,
            serde_json::json!({"seconds": timeout.as_secs()}),
        ),
        Exit::SpawnFailed => {
            crate::task_state::ReasonValue::new(REASON_SPAWN_FAILED, serde_json::json!({}))
        }
        Exit::Code(c) => {
            crate::task_state::ReasonValue::new(REASON_EXIT_STATUS, serde_json::json!({"code": c}))
        }
        Exit::Signal(s) => {
            crate::task_state::ReasonValue::new(REASON_SIGNAL, serde_json::json!({"signal": s}))
        }
    };
    CategoryError { kind, reason }
}

fn run_midclt(
    h: &dyn Host,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, crate::host::Output> {
    let mut cmd = Cmd::new(At::Host, "midclt", timeout);
    cmd.args
        .extend(args.iter().map(|s| std::ffi::OsString::from(*s)));
    let out = h.run(&cmd);
    if out.exit.ok() {
        Ok(out.stdout)
    } else {
        Err(out)
    }
}

/// One TrueNAS container instance of the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub dev_type: String,
}

pub fn instance_id_ok(s: &str) -> bool {
    let b = s.as_bytes();
    if !(1..=63).contains(&b.len()) {
        return false;
    }
    if !b[0].is_ascii_alphanumeric() {
        return false;
    }
    b[1..]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Parse `midclt call virt.instance.query` output.
pub fn parse_instances(json: &[u8]) -> Result<Vec<Instance>, String> {
    let raw: Vec<serde_json::Value> = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for item in raw {
        let id = item.get("id").and_then(|v| v.as_str()).map(str::to_string);
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();
        let kind = item
            .get("type")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();
        let status = item
            .get("status")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();
        if let Some(id) = id {
            out.push(Instance {
                id,
                name,
                kind,
                status,
            });
        }
    }
    Ok(out)
}

/// Parse `midclt call virt.instance.device_list <id>` output.
pub fn parse_devices(json: &[u8]) -> Result<Vec<Device>, String> {
    let raw: Vec<serde_json::Value> = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .map(|item| Device {
            dev_type: item
                .get("dev_type")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_default(),
        })
        .collect())
}

/// Pure rule: containers with a GPU device, sorted by name.
pub fn container_gpu_instances(
    instances: &[Instance],
    devices: &BTreeMap<String, Vec<Device>>,
) -> Vec<GpuUser> {
    let mut out: Vec<GpuUser> = instances
        .iter()
        .filter(|i| i.kind == "CONTAINER" && i.status == "RUNNING")
        .filter(|i| {
            devices
                .get(&i.id)
                .map(|list| list.iter().any(|d| d.dev_type == "GPU"))
                .unwrap_or(false)
        })
        .map(|i| GpuUser {
            name: i.name.clone(),
            kind: UserKind::TrueNasContainer,
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Docker GPU-user query–TrueNAS instance GPU-user query/query both categories. The caller passes the
/// timeout of its side: the web process uses `HOST_QUERY_TIMEOUT`, the runner uses
/// `HOST_CMD_TIMEOUT`, so the timeout reason carries the timeout that actually applied.
pub fn list(h: &dyn Host, timeout: Duration) -> GpuUsers {
    let mut result = GpuUsers::default();

    // Docker.
    let ps = h.run(
        &Cmd::new(At::Host, "docker", timeout)
            .arg("ps")
            .arg("-q")
            .arg("--no-trunc"),
    );
    if !ps.exit.ok() {
        result
            .errors
            .push(category_error(UserKind::Docker, &ps, timeout));
    } else {
        let ids: Vec<String> = ps
            .stdout_text()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        let all_hex = ids
            .iter()
            .all(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()));
        if !all_hex {
            result.errors.push(CategoryError {
                kind: UserKind::Docker,
                reason: crate::task_state::ReasonValue::new(
                    REASON_FORMAT_UNRECOGNIZED,
                    serde_json::json!({}),
                ),
            });
        } else if ids.is_empty() {
            // No running containers.
        } else {
            let mut cmd = Cmd::new(At::Host, "docker", timeout).arg("inspect");
            cmd.args.extend(ids.iter().map(std::ffi::OsString::from));
            let inspect = h.run(&cmd);
            let parse_result = parse_inspect(&inspect.stdout);
            let stderr = inspect.stderr_text();
            // Only a non-zero exit whose stderr has at least one `No such object` line and
            // nothing else may be treated as "the container disappeared between ps and
            // inspect". An empty stderr (for example a timeout) is never that case.
            let no_such_object = matches!(inspect.exit, crate::host::Exit::Code(code) if code != 0)
                && stderr.lines().any(|l| l.contains("No such object"))
                && stderr
                    .lines()
                    .all(|l| l.trim().is_empty() || l.contains("No such object"));
            match parse_result {
                Ok(mut containers) if inspect.exit.ok() || no_such_object => {
                    for c in &mut containers {
                        // Resolve /dev/dri/by-path links through the host view.
                        for d in &c.devices {
                            if let Some(node) = d
                                .path_on_host
                                .as_deref()
                                .and_then(|p| p.strip_prefix("/dev/dri/by-path/"))
                            {
                                let link = h.view(Path::new(&format!("/dev/dri/by-path/{node}")));
                                if let Ok(target) = h.read_link(&link) {
                                    c.by_path_targets.insert(
                                        node.to_string(),
                                        target.to_string_lossy().into_owned(),
                                    );
                                }
                            }
                        }
                    }
                    let needs_nodes = containers.iter().any(|c| {
                        c.devices.iter().any(|d| {
                            d.path_on_host
                                .as_deref()
                                .map(|p| p.starts_with("/dev/dri"))
                                .unwrap_or(false)
                        })
                    });
                    let nodes = if needs_nodes {
                        nvidia_dri_nodes(h)
                    } else {
                        Ok(BTreeSet::new())
                    };
                    match nodes {
                        Err(reason) => result.errors.push(CategoryError {
                            kind: UserKind::Docker,
                            reason: crate::task_state::ReasonValue::new(
                                reason,
                                serde_json::json!({}),
                            ),
                        }),
                        Ok(nodes) => {
                            let mut users: Vec<GpuUser> = containers
                                .iter()
                                .filter(|c| docker_requests_gpu(c, &nodes))
                                .map(|c| GpuUser {
                                    name: c.name.trim_start_matches('/').to_string(),
                                    kind: UserKind::Docker,
                                })
                                .collect();
                            users.sort_by(|a, b| a.name.cmp(&b.name));
                            result.users.extend(users);
                        }
                    }
                }
                Ok(_) => result
                    .errors
                    .push(category_error(UserKind::Docker, &inspect, timeout)),
                Err(_) => result.errors.push(CategoryError {
                    kind: UserKind::Docker,
                    reason: if inspect.exit.ok() || no_such_object {
                        // A successful exit whose JSON cannot be parsed is a format reading
                        // failure, never "exit code 0".
                        crate::task_state::ReasonValue::new(
                            REASON_FORMAT_UNRECOGNIZED,
                            serde_json::json!({}),
                        )
                    } else {
                        category_error(UserKind::Docker, &inspect, timeout).reason
                    },
                }),
            }
        }
    }

    match run_midclt(h, &["call", "container.query"], timeout) {
        Ok(bytes) => match parse_container_users(h, &bytes) {
            Ok(users) => result.users.extend(users),
            Err(reason) => result.errors.push(CategoryError {
                kind: UserKind::TrueNasContainer,
                reason: crate::task_state::ReasonValue::new(reason, serde_json::json!({})),
            }),
        },
        Err(out) if method_missing(&out, "container", "query") => {
            list_virt_instances(h, timeout, &mut result);
        }
        Err(out) => result
            .errors
            .push(category_error(UserKind::TrueNasContainer, &out, timeout)),
    }

    result
}

// Preserve the stored `incus` category key so existing task records still render.
fn method_missing(out: &crate::host::Output, service: &str, method: &str) -> bool {
    matches!(out.exit, crate::host::Exit::Code(code) if code != 0)
        && out.stderr_text().lines().any(|line| {
            let line = line
                .trim()
                .strip_prefix("[ENOMETHOD] ")
                .unwrap_or(line.trim());
            line == "Method does not exist"
                || line == format!("Method '{method}' not found in '{service}'")
                || line == format!("Service '{service}' not found")
        })
}

fn list_virt_instances(h: &dyn Host, timeout: Duration, result: &mut GpuUsers) {
    match run_midclt(h, &["call", "virt.instance.query"], timeout) {
        Err(out) if method_missing(&out, "virt.instance", "query") => {}
        Err(out) => result
            .errors
            .push(category_error(UserKind::TrueNasContainer, &out, timeout)),
        Ok(bytes) => match parse_instances(&bytes) {
            Err(_) => result.errors.push(CategoryError {
                kind: UserKind::TrueNasContainer,
                reason: crate::task_state::ReasonValue::new(
                    REASON_FORMAT_UNRECOGNIZED,
                    serde_json::json!({}),
                ),
            }),
            Ok(instances) => {
                let running: Vec<&Instance> = instances
                    .iter()
                    .filter(|i| i.kind == "CONTAINER" && i.status == "RUNNING")
                    .collect();
                let mut devices = BTreeMap::new();
                let mut failed: Option<CategoryError> = None;
                for i in &running {
                    if !instance_id_ok(&i.id) {
                        failed = Some(CategoryError {
                            kind: UserKind::TrueNasContainer,
                            reason: crate::task_state::ReasonValue::new(
                                REASON_FORMAT_UNRECOGNIZED,
                                serde_json::json!({}),
                            ),
                        });
                        break;
                    }
                    match run_midclt(h, &["call", "virt.instance.device_list", &i.id], timeout) {
                        Err(out) => {
                            failed =
                                Some(category_error(UserKind::TrueNasContainer, &out, timeout));
                            break;
                        }
                        Ok(bytes) => match parse_devices(&bytes) {
                            Ok(list) => {
                                devices.insert(i.id.clone(), list);
                            }
                            Err(_) => {
                                failed = Some(CategoryError {
                                    kind: UserKind::TrueNasContainer,
                                    reason: crate::task_state::ReasonValue::new(
                                        REASON_FORMAT_UNRECOGNIZED,
                                        serde_json::json!({}),
                                    ),
                                });
                                break;
                            }
                        },
                    }
                }
                if let Some(error) = failed {
                    result.errors.push(error);
                } else {
                    result
                        .users
                        .extend(container_gpu_instances(&instances, &devices));
                }
            }
        },
    }
}

// `container.query` includes `container.device.query` entries in `devices`.
#[derive(Deserialize)]
struct ContainerEntry {
    id: u64,
    name: String,
    status: ContainerStatus,
    devices: Vec<ContainerDevice>,
}

#[derive(Deserialize)]
struct ContainerStatus {
    state: String,
}

#[derive(Deserialize)]
struct ContainerDevice {
    attributes: serde_json::Value,
}

fn parse_container_users(h: &dyn Host, bytes: &[u8]) -> Result<Vec<GpuUser>, &'static str> {
    let entries: Vec<ContainerEntry> =
        serde_json::from_slice(bytes).map_err(|_| REASON_FORMAT_UNRECOGNIZED)?;
    let mut users = Vec::new();
    for entry in entries {
        if entry.id == 0
            || entry.name.is_empty()
            || !matches!(
                entry.status.state.as_str(),
                "RUNNING" | "STOPPED" | "SUSPENDED"
            )
        {
            return Err(REASON_FORMAT_UNRECOGNIZED);
        }
        if entry.status.state != "RUNNING" {
            continue;
        }
        let mut requested = false;
        for device in entry.devices {
            let attributes = &device.attributes;
            match attributes.get("dtype").and_then(|v| v.as_str()) {
                Some("GPU") => {
                    let vendor = attributes
                        .get("gpu_type")
                        .and_then(|v| v.as_str())
                        .filter(|v| matches!(*v, "NVIDIA" | "AMD" | "INTEL"))
                        .ok_or(REASON_FORMAT_UNRECOGNIZED)?;
                    attributes
                        .get("pci_address")
                        .and_then(|v| v.as_str())
                        .filter(|v| !v.is_empty())
                        .ok_or(REASON_FORMAT_UNRECOGNIZED)?;
                    requested |= vendor == "NVIDIA";
                }
                Some("FILESYSTEM") => {
                    let source = attributes
                        .get("source")
                        .and_then(|v| v.as_str())
                        .ok_or(REASON_FORMAT_UNRECOGNIZED)?;
                    let mut mapping = InspectedContainer {
                        devices: vec![DeviceMapping {
                            path_on_host: Some(source.to_string()),
                        }],
                        ..Default::default()
                    };
                    if let Some(node) = source.strip_prefix("/dev/dri/by-path/") {
                        if let Ok(target) = h.read_link(&h.view(Path::new(source))) {
                            mapping
                                .by_path_targets
                                .insert(node.to_string(), target.to_string_lossy().into_owned());
                        }
                    }
                    let nodes = if source.starts_with("/dev/dri") {
                        nvidia_dri_nodes(h)?
                    } else {
                        BTreeSet::new()
                    };
                    requested |= docker_requests_gpu(&mapping, &nodes);
                }
                Some("NIC" | "USB") => {}
                _ => return Err(REASON_FORMAT_UNRECOGNIZED),
            }
        }
        if requested {
            users.push(GpuUser {
                name: entry.name,
                kind: UserKind::TrueNasContainer,
            });
        }
    }
    users.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(users)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(env: &[&str], devices: &[&str]) -> InspectedContainer {
        InspectedContainer {
            name: "/ix-emby-1".into(),
            env: env.iter().map(|s| s.to_string()).collect(),
            device_requests: Vec::new(),
            devices: devices
                .iter()
                .map(|p| DeviceMapping {
                    path_on_host: Some((*p).to_string()),
                })
                .collect(),
            by_path_targets: BTreeMap::new(),
        }
    }

    #[test]
    fn docker_rules() {
        let nodes: BTreeSet<String> = ["renderD128".to_string(), "card1".to_string()].into();
        assert!(!docker_requests_gpu(&container(&[], &[]), &nodes));
        assert!(docker_requests_gpu(
            &container(&["NVIDIA_VISIBLE_DEVICES=all"], &[]),
            &nodes
        ));
        assert!(!docker_requests_gpu(
            &container(&["NVIDIA_VISIBLE_DEVICES=void"], &[]),
            &nodes
        ));
        assert!(!docker_requests_gpu(
            &container(&["NVIDIA_VISIBLE_DEVICES="], &[]),
            &nodes
        ));
        assert!(docker_requests_gpu(
            &container(&["NVIDIA_VISIBLE_DEVICES=0"], &["/dev/nvidia0"]),
            &nodes
        ));
        assert!(docker_requests_gpu(
            &container(&[], &["/dev/dri/renderD128"]),
            &nodes
        ));
        assert!(!docker_requests_gpu(
            &container(&[], &["/dev/dri/renderD129"]),
            &nodes
        ));
        assert!(docker_requests_gpu(&container(&[], &["/dev/dri"]), &nodes));
        assert!(!docker_requests_gpu(
            &container(&[], &["/dev/dri"]),
            &BTreeSet::new()
        ));
        let mut c = container(&[], &["/dev/dri/by-path/pci-0000:01:00.0-card"]);
        c.by_path_targets
            .insert("pci-0000:01:00.0-card".into(), "../card1".into());
        assert!(docker_requests_gpu(&c, &nodes));
        // The last NVIDIA_VISIBLE_DEVICES wins.
        assert!(!docker_requests_gpu(
            &container(
                &["NVIDIA_VISIBLE_DEVICES=all", "NVIDIA_VISIBLE_DEVICES=void"],
                &[]
            ),
            &nodes
        ));
    }

    #[test]
    fn device_requests_driver_and_capabilities() {
        let nodes = BTreeSet::new();
        let mut c = container(&[], &[]);
        c.device_requests.push(DeviceRequest {
            driver: Some("nvidia".into()),
            capabilities: Vec::new(),
        });
        assert!(docker_requests_gpu(&c, &nodes));
        let mut c = container(&[], &[]);
        c.device_requests.push(DeviceRequest {
            driver: None,
            capabilities: vec![vec!["gpu".into()]],
        });
        assert!(docker_requests_gpu(&c, &nodes));
    }

    #[test]
    fn container_rule() {
        let instances = vec![
            Instance {
                id: "a".into(),
                name: "media".into(),
                kind: "CONTAINER".into(),
                status: "RUNNING".into(),
            },
            Instance {
                id: "b".into(),
                name: "idle".into(),
                kind: "CONTAINER".into(),
                status: "STOPPED".into(),
            },
            Instance {
                id: "c".into(),
                name: "vm".into(),
                kind: "VM".into(),
                status: "RUNNING".into(),
            },
        ];
        let mut devices = BTreeMap::new();
        devices.insert(
            "a".to_string(),
            vec![Device {
                dev_type: "GPU".into(),
            }],
        );
        let users = container_gpu_instances(&instances, &devices);
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, "media");
        assert_eq!(users[0].kind, UserKind::TrueNasContainer);
    }

    #[test]
    fn inspect_parsing_is_null_safe() {
        let json = br#"[{"Name":"/x","Config":null,"HostConfig":{"DeviceRequests":null,"Devices":[{"PathOnHost":"/dev/nvidia0"}]}}]"#;
        let parsed = parse_inspect(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "/x");
        assert_eq!(
            parsed[0].devices[0].path_on_host.as_deref(),
            Some("/dev/nvidia0")
        );
    }

    use crate::host::{Cmd, Output, Watch};
    use std::time::Duration;

    /// A Docker query mock: `ps` reports one running container and `inspect` returns the
    /// configured output.
    struct DockerMock {
        inspect: Output,
    }
    impl Host for DockerMock {
        fn run(&self, c: &Cmd) -> Output {
            match (c.program, c.argv().get(1).map(String::as_str)) {
                ("docker", Some("ps")) => Output {
                    exit: crate::host::Exit::Code(0),
                    stdout: format!("{}\n", "a".repeat(64)).into_bytes(),
                    stderr: Vec::new(),
                    elapsed: Duration::ZERO,
                },
                ("docker", Some("inspect")) => self.inspect.clone(),
                ("midclt", _) => Output {
                    exit: crate::host::Exit::Code(0),
                    stdout: b"[]".to_vec(),
                    stderr: Vec::new(),
                    elapsed: Duration::ZERO,
                },
                _ => panic!("unexpected command {}", c.program),
            }
        }
        fn run_watch(&self, _: &Cmd, _: Duration, _: &mut dyn Watch) -> Output {
            unreachable!()
        }
        fn view(&self, p: &Path) -> std::path::PathBuf {
            p.to_path_buf()
        }
        fn mount_table(&self) -> std::io::Result<Vec<crate::facts::MountLine>> {
            unreachable!()
        }
        fn read(&self, _: &Path) -> std::io::Result<Vec<u8>> {
            unreachable!()
        }
        fn open(&self, _: &Path) -> std::io::Result<Box<dyn std::io::Read + Send>> {
            unreachable!()
        }
        fn create(
            &self,
            _: &Path,
            _: crate::host::Create,
            _: u32,
        ) -> std::io::Result<Box<dyn crate::host::SyncWrite + Send>> {
            unreachable!()
        }
        fn append(&self, _: &Path, _: u32) -> std::io::Result<Box<dyn std::io::Write + Send>> {
            unreachable!()
        }
        fn lstat(&self, _: &Path) -> std::io::Result<std::fs::Metadata> {
            unreachable!()
        }
        fn read_link(&self, _: &Path) -> std::io::Result<std::path::PathBuf> {
            unreachable!()
        }
        fn read_dir(&self, _: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
            unreachable!()
        }
        fn create_dir(&self, _: &Path, _: u32) -> std::io::Result<()> {
            unreachable!()
        }
        fn rename(&self, _: &Path, _: &Path) -> std::io::Result<()> {
            unreachable!()
        }
        fn remove_file(&self, _: &Path) -> std::io::Result<()> {
            unreachable!()
        }
        fn remove_dir_all(&self, _: &Path) -> std::io::Result<()> {
            unreachable!()
        }
        fn fsync_dir(&self, _: &Path) -> std::io::Result<()> {
            unreachable!()
        }
        fn fsync_file(&self, _: &Path) -> std::io::Result<()> {
            unreachable!()
        }
        fn uname_release(&self) -> std::io::Result<String> {
            unreachable!()
        }
        fn now(&self) -> std::time::Instant {
            std::time::Instant::now()
        }
        fn wall_ms(&self) -> u64 {
            0
        }
    }

    #[test]
    fn inspect_timeout_reports_an_unreadable_category() {
        let timeout = DockerMock {
            inspect: Output {
                exit: crate::host::Exit::TimedOut,
                stdout: br#"[{"Name":"/media","Config":null,"HostConfig":null}]"#.to_vec(),
                stderr: Vec::new(),
                elapsed: Duration::ZERO,
            },
        };
        let result = list(&timeout, HOST_QUERY_TIMEOUT);
        assert!(result.users.is_empty());
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].kind, UserKind::Docker);
        assert_eq!(result.errors[0].reason.key, REASON_TIMED_OUT);
        // The reason carries the timeout that actually applied.
        assert_eq!(
            result.errors[0].reason.params,
            serde_json::json!({"seconds": HOST_QUERY_TIMEOUT.as_secs()})
        );
        let runner = list(&timeout, HOST_CMD_TIMEOUT);
        assert_eq!(
            runner.errors[0].reason.params,
            serde_json::json!({"seconds": HOST_CMD_TIMEOUT.as_secs()})
        );
    }

    #[test]
    fn signaled_inspect_is_not_a_disappeared_container_even_with_matching_stderr() {
        let output = |exit| DockerMock {
            inspect: Output {
                exit,
                stdout: b"[]".to_vec(),
                stderr: b"Error: No such object: x".to_vec(),
                elapsed: Duration::ZERO,
            },
        };
        let signaled = list(&output(crate::host::Exit::Signal(15)), HOST_QUERY_TIMEOUT);
        assert!(signaled.users.is_empty());
        assert_eq!(signaled.errors.len(), 1);
        assert_eq!(signaled.errors[0].kind, UserKind::Docker);
        assert_eq!(signaled.errors[0].reason.key, REASON_SIGNAL);
        assert_eq!(
            signaled.errors[0].reason.params,
            serde_json::json!({"signal": 15})
        );

        let disappeared = list(&output(crate::host::Exit::Code(1)), HOST_QUERY_TIMEOUT);
        assert!(disappeared.users.is_empty());
        assert!(disappeared.errors.is_empty());
    }

    #[test]
    fn malformed_inspect_json_is_a_format_failure() {
        let malformed = DockerMock {
            inspect: Output {
                exit: crate::host::Exit::Code(0),
                stdout: b"{not json".to_vec(),
                stderr: Vec::new(),
                elapsed: Duration::ZERO,
            },
        };
        let result = list(&malformed, HOST_QUERY_TIMEOUT);
        assert!(result.users.is_empty());
        assert_eq!(result.errors.len(), 1);
        // A successful exit whose JSON cannot be parsed is a format reading failure, never
        // "exit code 0".
        assert_eq!(result.errors[0].reason.key, REASON_FORMAT_UNRECOGNIZED);
    }

    #[test]
    fn instance_ids() {
        assert!(instance_id_ok("media-1"));
        assert!(!instance_id_ok(""));
        assert!(!instance_id_ok("-x"));
        assert!(!instance_id_ok(&"a".repeat(64)));
    }

    #[test]
    fn instances_and_devices_parse() {
        let instances = parse_instances(
            br#"[{"id":"a","name":"media","type":"CONTAINER","status":"RUNNING"}]"#,
        )
        .unwrap();
        assert_eq!(instances[0].id, "a");
        let devices = parse_devices(br#"[{"dev_type":"GPU","name":"gpu0"}]"#).unwrap();
        assert_eq!(devices[0].dev_type, "GPU");
    }
}

//! The post-update check.

use serde_json::json;

use crate::codes::*;
use crate::facts::{self, Reading};
use crate::host::Exit;
use crate::runner::RunCtx;
use crate::task_state::{CheckRecord, Pass, ReadingJson, ReasonValue};

pub type RefreshReadings = (
    Reading<u64>,
    Reading<u64>,
    Option<ReasonValue>,
    Option<ReasonValue>,
    bool,
);

/// One strict `/usr` status reading. The optional reason preserves the
/// command's exit details when the reading failed before parsing.
fn sysext_status(ctx: &RunCtx) -> (Reading<facts::SysextUsr>, Option<ReasonValue>) {
    // The argv is shared with the ctx-free readers in `facts`, so both
    // paths ask the host exactly the same question.
    let cmd = facts::sysext_status_cmd(HOST_CMD_TIMEOUT);
    let out = ctx.cmd(&cmd);
    if !out.exit.ok() {
        let reason = command_reason(HOST_CMD_TIMEOUT, &out);
        return (Reading::Unknown(reason_key(&reason)), Some(reason));
    }
    let reading = match facts::parse_sysext_status(&out.stdout_text()) {
        Ok(status) => Reading::Value(status),
        Err(_) => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
    };
    (reading, None)
}

/// the merge time of the `/usr` nvidia extension. A row with other extensions or no merge
/// does not supply a nvidia SINCE value.
pub fn sysext_since(ctx: &RunCtx) -> (Reading<u64>, Option<ReasonValue>) {
    let (status, reason) = sysext_status(ctx);
    let since = match status {
        Reading::Value(facts::SysextUsr::Nvidia(since)) => Reading::Value(since),
        Reading::Value(_) => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
        Reading::Unknown(key) => Reading::Unknown(key),
    };
    (since, reason)
}

/// The intermediate confirmation requires exactly `"none"` and `since:null`
/// on the unique `/usr` row. An unreadable or malformed status remains UNKNOWN.
pub fn sysext_unmerged(ctx: &RunCtx) -> Reading<bool> {
    sysext_status(ctx)
        .0
        .map(|status| status == facts::SysextUsr::Unmerged)
}

/// the user-space driver version.
pub fn nvidia_smi_version(ctx: &RunCtx) -> (Reading<String>, Option<ReasonValue>) {
    let cmd = facts::nvidia_smi_version_cmd(HOST_CMD_TIMEOUT);
    let out = ctx.cmd(&cmd);
    if !out.exit.ok() {
        let reason = command_reason(HOST_CMD_TIMEOUT, &out);
        return (Reading::Unknown(reason_key(&reason)), Some(reason));
    }
    let reading = match facts::parse_smi_versions(&out.stdout_text()) {
        Some(v) => Reading::Value(v),
        None => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
    };
    (reading, None)
}

/// The command-failure reason of as a stored `{key, params}` value.
fn command_reason(timeout: std::time::Duration, out: &crate::host::Output) -> ReasonValue {
    match out.exit {
        Exit::TimedOut => ReasonValue::new(REASON_TIMED_OUT, json!({"seconds": timeout.as_secs()})),
        Exit::SpawnFailed => ReasonValue::new(REASON_SPAWN_FAILED, json!({})),
        Exit::Code(code) => ReasonValue::new(REASON_EXIT_STATUS, json!({"code": code})),
        Exit::Signal(signal) => ReasonValue::new(REASON_SIGNAL, json!({"signal": signal})),
    }
}

fn reason_key(reason: &ReasonValue) -> &'static str {
    // The key is one of the compiled-in constants, so the lookup is safe.
    crate::task_state::GENERAL_REASON_KEYS
        .iter()
        .find(|k| **k == reason.key.as_str())
        .copied()
        .unwrap_or(REASON_IO_ERROR)
}

/// the version in `/proc/driver/nvidia/version`.
pub fn proc_version(ctx: &RunCtx) -> Reading<String> {
    match ctx.host.read(std::path::Path::new(NVRM_VERSION_PATH)) {
        Ok(bytes) => match facts::parse_nvrm_version(&String::from_utf8_lossy(&bytes)) {
            Some(v) => Reading::Value(v.version.text().to_string()),
            None => Reading::Unknown(REASON_FORMAT_UNRECOGNIZED),
        },
        Err(e) => Reading::Unknown(match e.kind() {
            std::io::ErrorKind::NotFound => REASON_FILE_MISSING,
            std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
            _ => REASON_IO_ERROR,
        }),
    }
}

fn reading_json(reading: &Reading<String>, reason: &Option<ReasonValue>) -> ReadingJson<String> {
    match reading {
        Reading::Value(v) => ReadingJson::value(v.clone()),
        Reading::Unknown(r) => match reason {
            Some(reason) => ReadingJson::Unknown {
                unknown: reason.clone(),
            },
            None => ReadingJson::unknown(r),
        },
    }
}

fn since_json(reading: &Reading<u64>, reason: &Option<ReasonValue>) -> ReadingJson<u64> {
    match reading {
        Reading::Value(v) => ReadingJson::value(*v),
        Reading::Unknown(r) => match reason {
            Some(reason) => ReadingJson::Unknown {
                unknown: reason.clone(),
            },
            None => ReadingJson::unknown(r),
        },
    }
}

/// Run both items of the check and return one record. `since_*_reason` carry the
/// command-failure reason when the corresponding merge-time reading came from a failed
/// command.
pub fn check(
    ctx: &mut RunCtx,
    pass: Pass,
    readings: RefreshReadings,
    expected: &str,
) -> CheckRecord {
    let (since_before, since_after, since_before_reason, since_after_reason, unmerge_confirmed) =
        readings;
    let refresh_ok = match (&since_before, &since_after) {
        (Reading::Value(a), Reading::Value(b)) => unmerge_confirmed && a != b,
        _ => false,
    };
    let (smi, smi_reason) = nvidia_smi_version(ctx);
    let proc = proc_version(ctx);
    let version_ok = smi.value().map(|v| v == expected).unwrap_or(false)
        && proc.value().map(|v| v == expected).unwrap_or(false);
    let record = CheckRecord {
        pass,
        since_before: since_json(&since_before, &since_before_reason),
        since_after: since_json(&since_after, &since_after_reason),
        refresh_ok,
        nvidia_smi: reading_json(&smi, &smi_reason),
        proc_version: reading_json(&proc, &None),
        expected: expected.to_string(),
        version_ok,
        passed: refresh_ok && version_ok,
    };
    // One line per check item, each carrying the readings its `check.reason` sentence needs.
    // The `pass` is visible from the surrounding lines and is not part of either line.
    ctx.log(
        LogKey::CheckItem,
        json!({
            "check_item": "refresh",
            "passed": refresh_ok,
            "since_before": record.since_before.clone(),
            "since_after": record.since_after.clone(),
        }),
    );
    ctx.log(
        LogKey::CheckItem,
        json!({
            "check_item": "version",
            "passed": version_ok,
            "nvidia_smi": record.nvidia_smi.clone(),
            "proc_version": record.proc_version.clone(),
            "expected": record.expected.clone(),
        }),
    );
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_json_shapes() {
        let v = since_json(&Reading::Value(5), &None);
        assert!(matches!(v, ReadingJson::Value { .. }));
        let v = since_json(&Reading::Unknown(REASON_IO_ERROR), &None);
        assert!(matches!(v, ReadingJson::Unknown { .. }));
        let v = since_json(
            &Reading::Unknown(REASON_TIMED_OUT),
            &Some(ReasonValue::new(REASON_TIMED_OUT, json!({"seconds": 180}))),
        );
        assert!(matches!(v, ReadingJson::Unknown { .. }));
    }
}

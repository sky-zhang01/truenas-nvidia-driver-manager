//! Local driver rows.
//!
//! A row is one directory under `drivers/` holding `nvidia.raw` and `record.json`. The runner
//! only creates rows; the web process only changes `keep` and deletes whole rows.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::codes::*;
use crate::host::Host;
use crate::store::{self, JsonRead, Paths, Validate};
use crate::task_state::RowKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalOf {
    pub kernel: String,
    pub truenas_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub kind: RowKind,
    pub version: String,
    pub sha256: String,
    pub size: u64,
    pub target_kernel: String,
    pub created_at: u64,
    pub fingerprint: Option<String>,
    pub original_of: Option<OriginalOf>,
    pub keep: Option<bool>,
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl Validate for Record {
    fn validate(&self) -> Result<(), String> {
        if crate::facts::parse_version(&self.version).is_none() {
            return Err("bad version".into());
        }
        if !is_hex(&self.sha256, 64) {
            return Err("bad checksum".into());
        }
        if self.target_kernel.is_empty() {
            return Err("bad target kernel".into());
        }
        match self.kind {
            RowKind::Upload => {
                if !self
                    .fingerprint
                    .as_deref()
                    .map(|f| is_hex(f, FINGERPRINT_HEX))
                    .unwrap_or(false)
                {
                    return Err("bad fingerprint".into());
                }
                if self.original_of.is_some() || self.keep.is_some() {
                    return Err("upload row with original fields".into());
                }
            }
            RowKind::Official => {
                if self.fingerprint.is_some() || self.original_of.is_some() || self.keep.is_some() {
                    return Err("official row with unexpected fields".into());
                }
            }
            RowKind::Original => {
                if self.fingerprint.is_some() || self.keep.is_none() {
                    return Err("original row with unexpected fields".into());
                }
                let Some(of) = &self.original_of else {
                    return Err("original row without original_of".into());
                };
                if of.kernel.is_empty() || of.truenas_version.is_empty() {
                    return Err("bad original_of".into());
                }
            }
        }
        Ok(())
    }
}

impl Record {
    /// Kind must match the row id suffix.
    pub fn kind_matches(&self, row_id: &str) -> bool {
        if row_id.ends_with("-build") {
            matches!(self.kind, RowKind::Official | RowKind::Upload)
        } else if row_id.ends_with("-original") {
            self.kind == RowKind::Original
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    InUse,
    Available,
    KernelMismatch,
    Unknown,
}

impl RowStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RowStatus::InUse => "in_use",
            RowStatus::Available => "available",
            RowStatus::KernelMismatch => "kernel_mismatch",
            RowStatus::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Debug)]
pub struct Row {
    pub id: String,
    pub record: JsonRead<Record>,
    pub file_size: Option<u64>,
}

impl Row {
    pub fn record_ok(&self) -> Option<&Record> {
        match &self.record {
            JsonRead::Ok(r) if r.kind_matches(&self.id) => Some(r),
            _ => None,
        }
    }
}

/// List the rows of `drivers/` whose names are row ids.
pub fn list(h: &dyn Host, p: &Paths) -> Vec<Row> {
    let mut out = Vec::new();
    let Ok(names) = h.read_dir(&p.drivers()) else {
        return out;
    };
    for name in names {
        let id = name.to_string_lossy().into_owned();
        if !store::row_id_ok(&id) {
            continue;
        }
        let dir = p.row_dir(&id);
        let record = store::read_json::<Record>(h, &dir.join("record.json"));
        let file_size = h.lstat(&dir.join("nvidia.raw")).ok().map(|m| m.len());
        out.push(Row {
            id,
            record,
            file_size,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The status of one row.
pub fn status(
    row: &Row,
    canonical_sha256: &crate::facts::Reading<String>,
    kernel: &str,
) -> RowStatus {
    let Some(record) = row.record_ok() else {
        return RowStatus::Unknown;
    };
    if record.target_kernel != kernel {
        return RowStatus::KernelMismatch;
    }
    match canonical_sha256 {
        crate::facts::Reading::Unknown(_) => RowStatus::Unknown,
        crate::facts::Reading::Value(canonical) => {
            if record.sha256 == *canonical {
                RowStatus::InUse
            } else {
                RowStatus::Available
            }
        }
    }
}

/// Whether "Update to this version" is available. rule 3: a permanent-path checksum that
/// cannot be read leaves the row as UNKNOWN but does not disable the update entry; only an
/// unreadable/invalid record or a kernel mismatch does.
pub fn can_update(row: &Row, kernel: &str) -> bool {
    let Some(record) = row.record_ok() else {
        return false;
    };
    if record.target_kernel != kernel {
        return false;
    }
    true
}

/// Why a row cannot be deleted. "In use" is decided on its own, independent of the displayed
/// status: a row whose file is the permanent path can never be deleted, even when its target
/// kernel differs from the running kernel. A permanent-path checksum that cannot be read is
/// not a per-row block.
pub fn delete_blocked(
    row: &Row,
    canonical_sha256: &crate::facts::Reading<String>,
) -> Option<&'static str> {
    let Some(record) = row.record_ok() else {
        return Some("unknown");
    };
    if let crate::facts::Reading::Value(canonical) = canonical_sha256 {
        if record.sha256 == *canonical {
            return Some("in_use");
        }
    }
    if record.keep == Some(true) {
        return Some("kept");
    }
    None
}

/// Result of finding the original for a (kernel, TrueNAS version) pair.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum OriginalLookup {
    Found(Row),
    NotFound,
    Unreadable,
}

/// Find the newest original driver row for the pair.
pub fn find_original(h: &dyn Host, p: &Paths, kernel: &str, truenas: &str) -> OriginalLookup {
    let rows = list(h, p);
    let mut found: Option<Row> = None;
    let mut unreadable = false;
    for row in rows {
        if !row.id.ends_with("-original") {
            continue;
        }
        match &row.record {
            JsonRead::Ok(r) if r.kind_matches(&row.id) => {
                if let Some(of) = &r.original_of {
                    if of.kernel == kernel
                        && of.truenas_version == truenas
                        && found.as_ref().map(|f| row.id > f.id).unwrap_or(true)
                    {
                        found = Some(row);
                    }
                }
            }
            JsonRead::Ok(_) => {
                unreadable = true;
            }
            _ => {
                unreadable = true;
            }
        }
    }
    match found {
        Some(row) => OriginalLookup::Found(row),
        None if unreadable => OriginalLookup::Unreadable,
        None => OriginalLookup::NotFound,
    }
}

/// Move a staged row directory into `drivers/<row_id>`.
pub fn publish(h: &dyn Host, p: &Paths, staged: &Path, row_id: &str) -> std::io::Result<()> {
    h.fsync_dir(staged)?;
    h.rename(staged, &p.row_dir(row_id))
}

/// One row refused by driver deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedRow {
    pub id: String,
    pub reason: &'static str,
    pub version: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteError {
    Missing(Vec<String>),
    Refused(Vec<RefusedRow>),
    Failed {
        deleted: Vec<String>,
        reason: String,
    },
}

/// Delete the selected rows.
pub fn delete(
    h: &dyn Host,
    p: &Paths,
    ids: &[String],
    canonical_sha256: &str,
) -> Result<Vec<String>, DeleteError> {
    let rows = list(h, p);
    let mut missing = Vec::new();
    let mut selected: Vec<&Row> = Vec::new();
    for id in ids {
        match rows.iter().find(|r| &r.id == id) {
            Some(row) => selected.push(row),
            None => missing.push(id.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(DeleteError::Missing(missing));
    }
    let mut refused = Vec::new();
    for row in &selected {
        let canonical = crate::facts::Reading::Value(canonical_sha256.to_string());
        if let Some(reason) = delete_blocked(row, &canonical) {
            let record = row.record_ok();
            refused.push(RefusedRow {
                id: row.id.clone(),
                reason,
                version: record.map(|r| r.version.clone()),
                kind: record.map(|r| match r.kind {
                    RowKind::Official => "official".to_string(),
                    RowKind::Upload => "upload".to_string(),
                    RowKind::Original => "original".to_string(),
                }),
            });
        }
    }
    if !refused.is_empty() {
        return Err(DeleteError::Refused(refused));
    }
    let mut deleted = Vec::new();
    for row in &selected {
        let from = p.row_dir(&row.id);
        let to = p.trash().join(&row.id);
        if let Err(e) = h.rename(&from, &to) {
            return Err(DeleteError::Failed {
                deleted,
                reason: e.to_string(),
            });
        }
        let _ = h.remove_dir_all(&to);
        deleted.push(row.id.clone());
    }
    Ok(deleted)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepError {
    Missing,
    Unreadable,
    NotOriginal,
    Unsaved(String),
}

/// Atomically rewrite only the `keep` field of `record.json`.
pub fn set_keep(h: &dyn Host, p: &Paths, id: &str, keep: bool) -> Result<Row, KeepError> {
    let dir = p.row_dir(id);
    let path = dir.join("record.json");
    let mut record = match store::read_json::<Record>(h, &path) {
        JsonRead::Ok(r) => r,
        JsonRead::Missing | JsonRead::DirUnreadable(_) => return Err(KeepError::Missing),
        _ => return Err(KeepError::Unreadable),
    };
    // The same admission test as the table: a record whose kind does not match the row id
    // suffix is unreadable and must not be rewritten.
    if !record.kind_matches(id) {
        return Err(KeepError::Unreadable);
    }
    if record.kind != RowKind::Original {
        return Err(KeepError::NotOriginal);
    }
    record.keep = Some(keep);
    let bytes =
        serde_json::to_vec_pretty(&record).map_err(|e| KeepError::Unsaved(e.to_string()))?;
    store::write_atomic(h, &path, &bytes, 0o644).map_err(|e| KeepError::Unsaved(e.to_string()))?;
    Ok(Row {
        id: id.to_string(),
        record: JsonRead::Ok(record),
        file_size: h.lstat(&dir.join("nvidia.raw")).ok().map(|m| m.len()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RealHost;
    use crate::test_support::TestDir;

    fn setup() -> (TestDir, Paths) {
        let tmp = TestDir::new("drivers");
        let p = Paths::new(&*tmp);
        store::prepare_dirs(&RealHost::on_host(), &p).unwrap();
        (tmp, p)
    }

    fn write_row(h: &dyn Host, p: &Paths, id: &str, record: &Record, payload: &[u8]) {
        let dir = p.row_dir(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nvidia.raw"), payload).unwrap();
        std::fs::write(
            dir.join("record.json"),
            serde_json::to_vec_pretty(record).unwrap(),
        )
        .unwrap();
        let _ = h;
    }

    fn official(version: &str, sha: &str, kernel: &str) -> Record {
        Record {
            kind: RowKind::Official,
            version: version.into(),
            sha256: sha.into(),
            size: 3,
            target_kernel: kernel.into(),
            created_at: 1,
            fingerprint: None,
            original_of: None,
            keep: None,
        }
    }

    #[test]
    fn validate_records() {
        let mut r = official("580.178.04", &"a".repeat(64), "6.12.105");
        assert!(r.validate().is_ok());
        r.kind = RowKind::Original;
        assert!(r.validate().is_err());
        r.original_of = Some(OriginalOf {
            kernel: "6.12.105".into(),
            truenas_version: "25.10.7".into(),
        });
        r.keep = Some(true);
        assert!(r.validate().is_ok());
        r.keep = None;
        assert!(r.validate().is_err());
        let mut up = official("580.178.04", &"a".repeat(64), "6.12.105");
        up.kind = RowKind::Upload;
        assert!(up.validate().is_err());
        up.fingerprint = Some("b".repeat(12));
        assert!(up.validate().is_ok());
    }

    #[test]
    fn kind_matches_row_id() {
        let r = official("1.2.3", &"a".repeat(64), "k");
        assert!(r.kind_matches("20260928T101530Z-3f9a0c12-build"));
        assert!(!r.kind_matches("20260928T101530Z-3f9a0c12-original"));
    }

    #[test]
    fn status_order() {
        let (_tmp, p) = setup();
        let h = RealHost::on_host();
        let sha = "a".repeat(64);
        write_row(
            &h,
            &p,
            "20260928T101530Z-3f9a0c12-build",
            &official("580.178.04", &sha, "6.12.105"),
            b"abc",
        );
        let rows = list(&h, &p);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file_size, Some(3));
        assert_eq!(
            status(
                &rows[0],
                &crate::facts::Reading::Value(sha.clone()),
                "6.12.105"
            ),
            RowStatus::InUse
        );
        assert_eq!(
            status(
                &rows[0],
                &crate::facts::Reading::Value("b".repeat(64)),
                "6.12.105"
            ),
            RowStatus::Available
        );
        assert_eq!(
            status(&rows[0], &crate::facts::Reading::Value(sha.clone()), "6.9"),
            RowStatus::KernelMismatch
        );
        assert_eq!(
            status(
                &rows[0],
                &crate::facts::Reading::Unknown(REASON_IO_ERROR),
                "6.12.105"
            ),
            RowStatus::Unknown
        );
    }

    #[test]
    fn delete_refuses_and_then_deletes() {
        let (_tmp, p) = setup();
        let h = RealHost::on_host();
        let sha = "a".repeat(64);
        write_row(
            &h,
            &p,
            "20260928T101530Z-3f9a0c12-build",
            &official("580.178.04", &sha, "6.12.105"),
            b"abc",
        );
        let ids = vec!["20260928T101530Z-3f9a0c12-build".to_string()];
        let refused = delete(&h, &p, &ids, &sha).unwrap_err();
        assert!(matches!(refused, DeleteError::Refused(_)));
        let deleted = delete(&h, &p, &ids, &"b".repeat(64)).unwrap();
        assert_eq!(deleted.len(), 1);
        assert!(list(&h, &p).is_empty());
    }

    #[test]
    fn can_update_and_in_use_protection_are_independent() {
        let (_tmp, p) = setup();
        let h = RealHost::on_host();
        let sha = "a".repeat(64);
        write_row(
            &h,
            &p,
            "20260928T101530Z-3f9a0c12-build",
            &official("580.178.04", &sha, "6.12.105"),
            b"abc",
        );
        let rows = list(&h, &p);
        let row = &rows[0];
        let unknown = crate::facts::Reading::Unknown(REASON_IO_ERROR);
        // rule 3: the update entry stays available.
        assert!(can_update(row, "6.12.105"));
        assert_eq!(delete_blocked(row, &unknown), None);
        // Kernel mismatch disables the update entry but not the in-use
        // protection when the file is the permanent path.
        let in_use = crate::facts::Reading::Value(sha.clone());
        assert!(!can_update(row, "6.9.0"));
        assert_eq!(delete_blocked(row, &in_use), Some("in_use"));
        // Kept rows and unreadable records keep their own reasons.
        let mut kept = official("580.178.04", &"b".repeat(64), "6.12.105");
        kept.kind = RowKind::Original;
        kept.original_of = Some(OriginalOf {
            kernel: "6.12.105".into(),
            truenas_version: "25.10.7".into(),
        });
        kept.keep = Some(true);
        write_row(&h, &p, "20260928T101530Z-3f9a0c12-original", &kept, b"abc");
        let rows = list(&h, &p);
        let kept_row = rows.iter().find(|r| r.id.ends_with("-original")).unwrap();
        assert_eq!(delete_blocked(kept_row, &in_use), Some("kept"));
    }

    #[test]
    fn set_keep_refuses_kind_suffix_mismatch() {
        let (_tmp, p) = setup();
        let h = RealHost::on_host();
        let id = "20260928T101530Z-3f9a0c12-build";
        let mut r = official("580.178.04", &"a".repeat(64), "6.12.105");
        r.kind = RowKind::Original;
        r.original_of = Some(OriginalOf {
            kernel: "6.12.105".into(),
            truenas_version: "25.10.7".into(),
        });
        r.keep = Some(true);
        write_row(&h, &p, id, &r, b"abc");
        assert!(list(&h, &p)[0].record_ok().is_none());
        let before = std::fs::read(p.row_dir(id).join("record.json")).unwrap();
        assert!(matches!(
            set_keep(&h, &p, id, false),
            Err(KeepError::Unreadable)
        ));
        assert_eq!(
            std::fs::read(p.row_dir(id).join("record.json")).unwrap(),
            before
        );
    }

    #[test]
    fn original_lookup_and_keep() {
        let (_tmp, p) = setup();
        let h = RealHost::on_host();
        let mut r = official("580.173.02", &"a".repeat(64), "6.12.105");
        r.kind = RowKind::Original;
        r.original_of = Some(OriginalOf {
            kernel: "6.12.105".into(),
            truenas_version: "25.10.7".into(),
        });
        r.keep = Some(true);
        let id = "20260928T101530Z-3f9a0c12-original";
        write_row(&h, &p, id, &r, b"abc");
        assert!(matches!(
            find_original(&h, &p, "6.12.105", "25.10.7"),
            OriginalLookup::Found(_)
        ));
        assert!(matches!(
            find_original(&h, &p, "6.12.105", "25.04"),
            OriginalLookup::NotFound
        ));
        let updated = set_keep(&h, &p, id, false).unwrap();
        assert_eq!(updated.record_ok().unwrap().keep, Some(false));
        assert!(matches!(
            set_keep(&h, &p, id, true).map(|r| r.record_ok().unwrap().keep),
            Ok(Some(true))
        ));
        assert!(matches!(
            set_keep(&h, &p, "20260928T101530Z-3f9a0c12-build", true),
            Err(KeepError::Missing)
        ));
    }
}

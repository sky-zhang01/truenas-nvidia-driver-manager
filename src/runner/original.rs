//! Saving the original driver and judging a saved original.

use serde_json::{json, Value};

use crate::codes::*;
use crate::drivers;
use crate::drivers::OriginalOf;
use crate::facts::Reading;
use crate::host::Create;
use crate::runner::RunCtx;
use crate::store;
use crate::task_state::RowKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveFailure {
    pub reason: &'static str,
    pub params: Value,
    pub raw: Option<String>,
    /// The host command that failed, so the terminal state can carry
    /// `error.command`/`error.exit` like every other command failure.
    pub command: Option<(Vec<String>, crate::host::Exit)>,
}

impl Default for SaveFailure {
    fn default() -> SaveFailure {
        SaveFailure {
            reason: "",
            params: Value::Null,
            raw: None,
            command: None,
        }
    }
}

/// A saved original that can be restored.
#[derive(Debug, Clone)]
pub struct OriginalRow {
    pub row_id: String,
    pub version: String,
    pub saved_at: u64,
    pub path: std::path::PathBuf,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoOriginal {
    pub reason: &'static str,
    pub read_reason: Option<&'static str>,
}

/// save the file at the permanent path as the original.
pub fn save(ctx: &mut RunCtx) -> Result<(), SaveFailure> {
    let plan = ctx.state.plan.as_ref().unwrap();
    let (kernel, truenas) = (plan.kernel.clone(), plan.truenas_version.clone());
    let existing = drivers::find_original(ctx.host.as_ref(), &ctx.paths, &kernel, &truenas);
    if let drivers::OriginalLookup::Found(_) = existing {
        ctx.log(LogKey::OriginalExists, json!({}));
        return Ok(());
    }

    let row_dir = ctx.task_dir().join("row-original");
    if ctx.host.create_dir(&row_dir, 0o755).is_err() {
        return Err(SaveFailure {
            reason: SAVE_WRITE_FAILED,
            params: json!({}),
            raw: None,

            ..SaveFailure::default()
        });
    }
    let dest = row_dir.join("nvidia.raw");
    let copied = {
        let mut src = match ctx.host.open(std::path::Path::new(CANONICAL_PATH)) {
            Ok(f) => f,
            Err(e) => {
                let _ = ctx.host.remove_dir_all(&row_dir);
                return Err(SaveFailure {
                    reason: SAVE_CANONICAL_UNREADABLE,
                    params: json!({}),
                    raw: Some(e.to_string()),

                    ..SaveFailure::default()
                });
            }
        };
        let mut dst = match ctx.host.create(&dest, Create::New, 0o644) {
            Ok(f) => f,
            Err(e) => {
                let _ = ctx.host.remove_dir_all(&row_dir);
                return Err(SaveFailure {
                    reason: SAVE_WRITE_FAILED,
                    params: json!({}),
                    raw: Some(e.to_string()),

                    ..SaveFailure::default()
                });
            }
        };
        let result =
            store::copy_hashed(
                &mut *src,
                &mut *dst,
                |_| std::ops::ControlFlow::Continue(()),
            );
        match result {
            Ok(copied) => {
                if let Err(e) = dst.sync_all() {
                    let _ = ctx.host.remove_dir_all(&row_dir);
                    return Err(SaveFailure {
                        reason: SAVE_WRITE_FAILED,
                        params: json!({}),
                        raw: Some(e.to_string()),

                        ..SaveFailure::default()
                    });
                }
                copied
            }
            Err(e) => {
                let _ = ctx.host.remove_dir_all(&row_dir);
                return Err(SaveFailure {
                    reason: SAVE_WRITE_FAILED,
                    params: json!({}),
                    raw: Some(e.to_string()),

                    ..SaveFailure::default()
                });
            }
        }
    };

    match store::hash_file(ctx.host.as_ref(), &dest) {
        Ok(back) if back.sha256 == copied.sha256 && back.size == copied.size => {}
        Ok(_) => {
            let _ = ctx.host.remove_dir_all(&row_dir);
            return Err(SaveFailure {
                reason: SAVE_READBACK_MISMATCH,
                params: json!({}),
                raw: None,

                ..SaveFailure::default()
            });
        }
        Err(e) => {
            let _ = ctx.host.remove_dir_all(&row_dir);
            return Err(SaveFailure {
                reason: SAVE_WRITE_FAILED,
                params: json!({}),
                raw: Some(e.to_string()),

                ..SaveFailure::default()
            });
        }
    }

    // read the version and target kernel from the copy. The command goes through
    // `RunCtx::cmd`, so the task log carries its `log.cmd` line and the failure carries the
    // argv, the exit status and the verbatim external output.
    let listing_cmd = crate::facts::image_facts_command(&dest, HOST_CMD_TIMEOUT);
    let listing = ctx.cmd(&listing_cmd);
    let facts = match crate::facts::image_facts_from_output(&listing) {
        Ok(f) => f,
        Err(_) => {
            let _ = ctx.host.remove_dir_all(&row_dir);
            // The verbatim external output is kept as the error's `raw`, like every other
            // command failure: the last `ERROR_RAW_LINES` lines. An empty stdout must not
            // prepend a spurious blank line.
            let raw = listing.raw_tail(ERROR_RAW_LINES);
            let raw = raw.strip_prefix('\n').unwrap_or(&raw).to_string();
            return Err(SaveFailure {
                reason: SAVE_FACTS_UNREADABLE,
                params: json!({}),
                raw: (!raw.is_empty()).then_some(raw),
                command: Some((listing_cmd.argv(), listing.exit)),
            });
        }
    };
    let (Reading::Value(version), Reading::Value(target_kernel)) =
        (&facts.file_version, &facts.target_kernel)
    else {
        let _ = ctx.host.remove_dir_all(&row_dir);
        return Err(SaveFailure {
            reason: SAVE_FACTS_UNREADABLE,
            params: json!({}),
            raw: None,

            ..SaveFailure::default()
        });
    };
    let record = drivers::Record {
        kind: RowKind::Original,
        version: version.clone(),
        sha256: copied.sha256.clone(),
        size: copied.size,
        target_kernel: target_kernel.clone(),
        created_at: ctx.host.wall_ms(),
        fingerprint: None,
        original_of: Some(OriginalOf {
            kernel,
            truenas_version: truenas,
        }),
        keep: Some(true),
    };
    let record_bytes = match serde_json::to_vec_pretty(&record) {
        Ok(b) => b,
        Err(e) => {
            let _ = ctx.host.remove_dir_all(&row_dir);
            return Err(SaveFailure {
                reason: SAVE_WRITE_FAILED,
                params: json!({}),
                raw: Some(e.to_string()),

                ..SaveFailure::default()
            });
        }
    };
    let record_path = row_dir.join("record.json");
    if let Err(e) = store::write_atomic(ctx.host.as_ref(), &record_path, &record_bytes, 0o644) {
        let _ = ctx.host.remove_dir_all(&row_dir);
        return Err(SaveFailure {
            reason: SAVE_WRITE_FAILED,
            params: json!({}),
            raw: Some(e.to_string()),

            ..SaveFailure::default()
        });
    }
    let row_id = format!("{}-original", ctx.id);
    if let Err(e) = drivers::publish(ctx.host.as_ref(), &ctx.paths, &row_dir, &row_id) {
        let _ = ctx.host.remove_dir_all(&row_dir);
        return Err(SaveFailure {
            reason: SAVE_WRITE_FAILED,
            params: json!({}),
            raw: Some(e.to_string()),

            ..SaveFailure::default()
        });
    }
    ctx.log(
        LogKey::OriginalSaved,
        json!({
            "version": version,
            "target_kernel": target_kernel,
            "checksum": copied.sha256,
        }),
    );
    Ok(())
}

/// The restore decision.
pub fn usable_original(ctx: &RunCtx) -> Result<OriginalRow, NoOriginal> {
    let plan = ctx.state.plan.as_ref().unwrap();
    let choice = plan.original;
    match drivers::find_original(
        ctx.host.as_ref(),
        &ctx.paths,
        &plan.kernel,
        &plan.truenas_version,
    ) {
        drivers::OriginalLookup::Found(row) => {
            let record = row.record_ok().unwrap();
            let path = ctx.paths.row_dir(&row.id).join("nvidia.raw");
            match store::hash_file(ctx.host.as_ref(), &path) {
                Ok(copied) if copied.sha256 == record.sha256 => Ok(OriginalRow {
                    row_id: row.id.clone(),
                    version: record.version.clone(),
                    saved_at: record.created_at,
                    path,
                    sha256: record.sha256.clone(),
                    size: record.size,
                }),
                Ok(_) => Err(NoOriginal {
                    reason: RESTORE_CHECKSUM_MISMATCH,
                    read_reason: None,
                }),
                Err(e) => Err(NoOriginal {
                    reason: RESTORE_UNREADABLE,
                    read_reason: Some(crate::log::io_reason_key(&e)),
                }),
            }
        }
        drivers::OriginalLookup::Unreadable => {
            if choice == crate::task_state::OriginalChoice::Skip {
                Err(NoOriginal {
                    reason: RESTORE_NOT_SAVED,
                    read_reason: None,
                })
            } else {
                Err(NoOriginal {
                    reason: RESTORE_UNREADABLE,
                    read_reason: Some(REASON_RECORD_UNREADABLE),
                })
            }
        }
        drivers::OriginalLookup::NotFound => {
            if choice == crate::task_state::OriginalChoice::Skip {
                Err(NoOriginal {
                    reason: RESTORE_NOT_SAVED,
                    read_reason: None,
                })
            } else {
                Err(NoOriginal {
                    reason: RESTORE_UNREADABLE,
                    read_reason: Some(REASON_FILE_MISSING),
                })
            }
        }
    }
}

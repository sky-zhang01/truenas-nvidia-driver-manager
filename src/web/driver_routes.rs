use super::*;

// ---- driver list / driver deletion / driver retention
// -------------------------------------------------------

pub(super) async fn list_drivers(State((ctx, _)): State<AppState>, headers: HeaderMap) -> Response {
    // Reconciliation and listing share one response budget, including canonical reads.
    // When it expires, `delete_unavailable` prevents deletion based on incomplete facts.
    match whole_response(&ctx, headers, |ctx, reconciled, reads| {
        drivers_response(ctx, reconciled, reads)
    })
    .await
    {
        WholeResponse::Ready(response) => with_common_headers(response),
        WholeResponse::Failed => internal().into_response(),
        WholeResponse::TimedOut => with_common_headers(drivers_response(
            &ctx,
            Some(crate::tasks::Reconciled::unreadable(reading_timed_out())),
            false,
        )),
    }
}

fn driver_row_json(
    row: &drivers::Row,
    canonical: &Reading<String>,
    kernel: &str,
    keep: Option<bool>,
) -> Value {
    let record = row.record_ok();
    let status = drivers::status(row, canonical, kernel);
    let delete_blocked = drivers::delete_blocked(row, canonical);
    let can_update = drivers::can_update(row, kernel);
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), json!(row.id));
    match record {
        Some(record) => {
            obj.insert(
                "kind".into(),
                json!(match record.kind {
                    crate::task_state::RowKind::Official => "official",
                    crate::task_state::RowKind::Upload => "upload",
                    crate::task_state::RowKind::Original => "original",
                }),
            );
            obj.insert("version".into(), json!(record.version));
            obj.insert(
                "fingerprint".into(),
                record
                    .fingerprint
                    .clone()
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            );
            obj.insert("created_at".into(), json!(record.created_at));
            obj.insert("size".into(), json!(record.size));
            obj.insert("target_kernel".into(), json!(record.target_kernel));
            obj.insert("status".into(), json!(status.as_str()));
            obj.insert(
                "keep".into(),
                keep.or(record.keep).map(Value::from).unwrap_or(Value::Null),
            );
            obj.insert("can_update".into(), json!(can_update));
            obj.insert(
                "delete_blocked".into(),
                delete_blocked.map(Value::from).unwrap_or(Value::Null),
            );
        }
        None => {
            for field in [
                "kind",
                "version",
                "fingerprint",
                "created_at",
                "size",
                "target_kernel",
            ] {
                obj.insert(field.into(), Value::Null);
            }
            obj.insert("status".into(), json!("UNKNOWN"));
            obj.insert("keep".into(), Value::Null);
            obj.insert("can_update".into(), json!(false));
            obj.insert("delete_blocked".into(), json!("unknown"));
        }
    }
    Value::Object(obj)
}

fn drivers_response(
    ctx: &Ctx,
    reconciled: Option<crate::tasks::Reconciled>,
    reads: bool,
) -> Response {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    // The timed-out path answers from the timeout reason alone: no host read is attempted
    // after the budget passed.
    let (canonical, kernel, rows) = if reads {
        let canonical = facts::canonical_sha256(h);
        let kernel = facts::kernel(h);
        let rows = drivers::list(h, p);
        (canonical, kernel, rows)
    } else {
        (
            facts::Reading::Unknown(REASON_TIMED_OUT),
            facts::Reading::Unknown(REASON_TIMED_OUT),
            Vec::new(),
        )
    };
    let kernel_str = kernel.value().cloned().unwrap_or_default();
    let rows_json: Vec<Value> = rows
        .iter()
        .map(|r| driver_row_json(r, &canonical, &kernel_str, None))
        .collect();
    let delete_unavailable = match reconciled.as_ref().map(|r| r.active.blocker()) {
        None => json!({
            "key": "task_state_unreadable",
            "reason": startup_check_reason(),
        }),
        Some(Some(crate::tasks::Blocker::Running { .. })) => json!({"key": "task_running"}),
        Some(Some(crate::tasks::Blocker::Unreadable { reason })) => json!({
            "key": "task_state_unreadable",
            "reason": {"key": reason.key, "params": reason.params},
        }),
        Some(None) => {
            if canonical.unknown_reason().is_some() {
                json!({"key": "canonical_unreadable"})
            } else {
                Value::Null
            }
        }
    };
    json_response(json!({
        "rows": rows_json,
        "kernel": kernel.value().cloned().map(Value::from).unwrap_or(Value::Null),
        "delete_unavailable": delete_unavailable,
    }))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteRequest {
    ids: Vec<String>,
}

pub(super) async fn delete_drivers(
    State((ctx, _)): State<AppState>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<DeleteRequest>,
) -> Response {
    if req.ids.is_empty() {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        let h = ctx2.host.as_ref();
        let p = &ctx2.paths;
        for id in &req.ids {
            if !store::row_id_ok(id) {
                return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
            }
        }
        // `pid: host` failed: no reconciliation and no deletion.
        if !ctx2.startup.pid_host_ok {
            return ApiError::new(StatusCode::CONFLICT, "delete.task_state_unknown")
                .params(json!({
                    "reason": startup_check_reason(),
                }))
                .into_response();
        }
        // Cross-process ordering: the reconciliation, the state check and the deletion itself
        // run inside the global control lock, in the fixed order `process_lock -> control ->
        // execution`. The core is called directly because this call already holds the control
        // lock; taking it a second time on another descriptor would look busy.
        let _control = match crate::admission::control_lock(h, crate::admission::LOCK_WAIT) {
            Ok(lock) => lock,
            Err(blocked) => {
                return ApiError::new(StatusCode::CONFLICT, "delete.task_state_unknown")
                    .params(json!({"reason": {"key": blocked.key, "params": blocked.params}}))
                    .into_response()
            }
        };
        // The lock is kept until this critical section (including the row
        // deletion below) is finished; it is released when the guard drops.
        let reconciled = crate::tasks::reconcile(h, p);
        match reconciled.active.blocker() {
            Some(crate::tasks::Blocker::Running { .. }) => {
                return ApiError::new(StatusCode::CONFLICT, "delete.task_running").into_response()
            }
            Some(crate::tasks::Blocker::Unreadable { reason }) => {
                return ApiError::new(StatusCode::CONFLICT, "delete.task_state_unknown")
                    .params(json!({"reason": {"key": reason.key, "params": reason.params}}))
                    .into_response()
            }
            None => {}
        }
        let canonical = facts::canonical_sha256(h);
        let Reading::Value(canonical) = canonical else {
            return ApiError::new(StatusCode::CONFLICT, "delete.canonical_unreadable")
                .into_response();
        };
        let mut ids = req.ids.clone();
        ids.sort();
        ids.dedup();
        match drivers::delete(h, p, &ids, &canonical) {
            Ok(deleted) => {
                crate::log::info(
                    Event::DriversDeleted,
                    &[("rows", &deleted.len().to_string())],
                );
                json_response(json!({"deleted": deleted}))
            }
            Err(drivers::DeleteError::Missing(ids)) => {
                ApiError::new(StatusCode::NOT_FOUND, "delete.row_missing")
                    .params(json!({"ids": ids}))
                    .into_response()
            }
            Err(drivers::DeleteError::Refused(rows)) => {
                let rows_json: Vec<Value> = rows
                    .iter()
                    .map(|r| {
                        json!({
                            "id": r.id,
                            "reason": r.reason,
                            "version": r.version,
                            "kind": r.kind,
                        })
                    })
                    .collect();
                ApiError::new(StatusCode::CONFLICT, "delete.refused")
                    .params(json!({"rows": rows_json}))
                    .into_response()
            }
            Err(drivers::DeleteError::Failed { deleted, reason }) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "delete.failed")
                    .params(json!({"deleted": deleted, "reason": reason}))
                    .into_response()
            }
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KeepRequest {
    keep: bool,
}

pub(super) async fn set_driver_keep(
    State((ctx, _)): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<KeepRequest>,
) -> Response {
    if !store::row_id_ok(&id) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        let h = ctx2.host.as_ref();
        let p = &ctx2.paths;
        match drivers::set_keep(h, p, &id, req.keep) {
            Ok(row) => {
                let canonical = facts::canonical_sha256(h);
                let kernel = facts::kernel(h);
                let value = driver_row_json(
                    &row,
                    &canonical,
                    kernel.value().map(|s| s.as_str()).unwrap_or(""),
                    Some(req.keep),
                );
                json_response(value)
            }
            Err(drivers::KeepError::Missing) => {
                ApiError::new(StatusCode::NOT_FOUND, "delete.row_missing").into_response()
            }
            Err(drivers::KeepError::Unreadable) => {
                ApiError::new(StatusCode::CONFLICT, "keep.row_unreadable").into_response()
            }
            Err(drivers::KeepError::NotOriginal) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "keep.not_original").into_response()
            }
            Err(drivers::KeepError::Unsaved(reason)) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "keep.unsaved")
                    .params(json!({"reason": reason}))
                    .into_response()
            }
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

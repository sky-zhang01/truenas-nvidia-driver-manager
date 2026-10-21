use super::*;

// ---- task list / task detail -------------------------------------------------------------

pub(super) async fn list_tasks(State((ctx, _)): State<AppState>, headers: HeaderMap) -> Response {
    // The whole response is bounded by `READING_TIMEOUT`: while a pause or a drain holds the
    // OS locks the endpoint answers with the timeout reason instead of blocking, and the
    // response work that follows the reconcile shares the same budget.
    match whole_response(&ctx, headers, |ctx, reconciled, reads| {
        tasks_response(ctx, reconciled, reads)
    })
    .await
    {
        WholeResponse::Ready(response) => with_common_headers(response),
        WholeResponse::Failed => internal().into_response(),
        WholeResponse::TimedOut => with_common_headers(tasks_response(
            &ctx,
            Some(crate::tasks::Reconciled::unreadable(reading_timed_out())),
            false,
        )),
    }
}

pub(super) fn task_source_json(state: Option<&TaskState>) -> Value {
    match state.and_then(|s| s.plan.as_ref()) {
        None => Value::Null,
        Some(plan) => match &plan.source {
            crate::task_state::PlanSource::Official => json!({"kind": "official"}),
            crate::task_state::PlanSource::Upload { fingerprint, .. } => {
                json!({"kind": "upload", "fingerprint": fingerprint})
            }
            crate::task_state::PlanSource::Local {
                row_kind,
                fingerprint,
                ..
            } => {
                let mut obj = serde_json::Map::new();
                obj.insert("kind".into(), json!("local"));
                obj.insert(
                    "row_kind".into(),
                    json!(match row_kind {
                        crate::task_state::RowKind::Official => "official",
                        crate::task_state::RowKind::Upload => "upload",
                        crate::task_state::RowKind::Original => "original",
                    }),
                );
                if let Some(f) = fingerprint {
                    obj.insert("fingerprint".into(), json!(f));
                }
                Value::Object(obj)
            }
        },
    }
}

fn tasks_response(
    ctx: &Ctx,
    reconciled: Option<crate::tasks::Reconciled>,
    reads: bool,
) -> Response {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    // The timed-out path answers without another host read.
    let state_dir_unreadable = if reads {
        h.read_dir(&p.state_dir()).err().map(|e| {
            json!({
                "reason": {"key": match e.kind() {
                    std::io::ErrorKind::PermissionDenied => REASON_PERMISSION_DENIED,
                    _ => REASON_IO_ERROR,
                }, "params": {}},
            })
        })
    } else {
        Some(json!({
            "reason": {"key": REASON_TIMED_OUT, "params": {"seconds": READING_TIMEOUT.as_secs()}},
        }))
    };
    let Some(reconciled) = reconciled else {
        // `pid: host` failed: no reconciliation, no writes; the unit query is reported as
        // unreadable.
        return json_response(json!({
            "tasks": [],
            "active_task": {"unknown": startup_check_reason()},
            "unit_query": {"reason": startup_check_reason()},
            "state_dir": state_dir_unreadable,
        }));
    };
    let mut tasks = Vec::new();
    let mut sorted = reconciled.tasks.clone();
    sorted.sort_by(|a, b| b.id.cmp(&a.id));
    for t in &sorted {
        let times = crate::tasks::project_task_times(t.status, t.state.as_ref());
        let (source, version, phase) = match &t.state {
            Some(state) => (
                task_source_json(Some(state)),
                state.plan.as_ref().map(|p| p.version.clone()),
                state.phase.map(|p| p.as_str()),
            ),
            None => (Value::Null, None, None),
        };
        let mut obj = serde_json::Map::new();
        obj.insert("task_id".into(), json!(t.id));
        obj.insert("status".into(), json!(t.status_for_ui()));
        obj.insert("phase".into(), json!(phase));
        obj.insert("source".into(), source);
        obj.insert(
            "version".into(),
            version.map(Value::from).unwrap_or(Value::Null),
        );
        obj.insert("created_at".into(), opt_millis(times.created_at));
        obj.insert("finished_at".into(), opt_millis(times.finished_at));
        obj.insert(
            "interrupt_discovered_at".into(),
            opt_millis(times.interrupt_discovered_at),
        );
        if let Some(state_read_error) = &t.state_read_error {
            obj.insert("state_read_error".into(), json!(state_read_error));
        }
        tasks.push(Value::Object(obj));
    }
    let active_task = match reconciled.active.blocker() {
        None => Value::Null,
        Some(crate::tasks::Blocker::Running { task_ids }) => match task_ids.first() {
            Some(id) => {
                let t = reconciled.state_of(id);
                let status = t
                    .map(|t| t.status_for_ui())
                    .unwrap_or_else(|| "UNKNOWN".into());
                json!({"task_id": id, "status": status})
            }
            None => Value::Null,
        },
        Some(crate::tasks::Blocker::Unreadable { reason }) => json!({
            "unknown": {"key": reason.key, "params": reason.params},
        }),
    };
    let unit_query = reconciled
        .unit_query_error
        .as_ref()
        .map(|r| json!({"reason": {"key": r.key, "params": r.params}}));
    json_response(json!({
        "tasks": tasks,
        "active_task": active_task,
        "unit_query": unit_query,
        "state_dir": state_dir_unreadable,
    }))
}

pub(super) async fn release_task(
    State((ctx, _)): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if !store::task_id_ok(&id) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx, &headers) {
            return response;
        }
        if !ctx.startup.pid_host_ok {
            return ApiError::new(StatusCode::CONFLICT, "release.state_unknown")
                .params(json!({"reason": startup_check_reason()}))
                .into_response();
        }
        let h = ctx.host.as_ref();
        let _control = match crate::admission::control_lock(h, crate::admission::LOCK_WAIT) {
            Ok(lock) => lock,
            Err(blocked) => {
                return ApiError::new(StatusCode::CONFLICT, "release.lock_busy")
                    .params(json!({"reason": {"key": blocked.key, "params": blocked.params}}))
                    .into_response();
            }
        };
        match crate::tasks::release_recovery(h, &ctx.paths, &id) {
            Ok(_) => {
                terminal_task_detail(&ctx, &id, None).unwrap_or_else(|| internal().into_response())
            }
            Err(refusal) => {
                let mut error = ApiError::new(StatusCode::CONFLICT, refusal.key);
                error.raw = refusal.raw;
                error.into_response()
            }
        }
    })
    .await;
    match result {
        Ok(response) => response,
        Err(_) => internal().into_response(),
    }
}

pub(super) async fn task_detail(
    State((ctx, _)): State<AppState>,
    AxumPath(id): AxumPath<String>,
    request: Request,
) -> Response {
    let query = request.uri().query().unwrap_or("").to_string();
    let after = match query_param(&query, "after") {
        QueryParam::Missing => None,
        QueryParam::Invalid => {
            return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response()
        }
        QueryParam::Value(v) => {
            // A strict decimal u64: a plus sign or any other byte is a format error, never an
            // accepted value.
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
            }
            match v.parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid")
                        .into_response()
                }
            }
        }
    };
    if !store::task_id_ok(&id) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    // A terminal record's outcome is fixed, so its detail (including any
    // persisted release marker) is answered from the state file; the whole-host
    // reconciliation runs only while a task is running or its record is not
    // trustworthy. The projection keeps the reconciled shape.
    if ctx.startup.pid_host_ok {
        let ctx2 = ctx.clone();
        let id2 = id.clone();
        if let Ok(Some(response)) =
            tokio::task::spawn_blocking(move || terminal_task_detail(&ctx2, &id2, after)).await
        {
            return with_common_headers(response);
        }
    }
    // The reconcile and the detail work share one `READING_TIMEOUT`, so a held pause/drain
    // lock cannot block this read.
    let id_for_work = id.clone();
    let headers = request.headers().clone();
    match whole_response(&ctx, headers, move |ctx, reconciled, reads| {
        reconciled_task_response(ctx, &id_for_work, after, reconciled, reads)
    })
    .await
    {
        WholeResponse::Ready(response) => with_common_headers(response),
        WholeResponse::Failed => internal().into_response(),
        WholeResponse::TimedOut => with_common_headers(reconciled_task_response(
            &ctx,
            &id,
            after,
            Some(crate::tasks::Reconciled::unreadable(reading_timed_out())),
            false,
        )),
    }
}

fn reconciled_task_response(
    ctx: &Ctx,
    id: &str,
    after: Option<u64>,
    reconciled: Option<crate::tasks::Reconciled>,
    reads: bool,
) -> Response {
    if !reads {
        // The budget passed: answer the unknown shape without touching the host again, and
        // never as a missing task.
        return unknown_task_response(
            id,
            None,
            reconciled
                .as_ref()
                .and_then(|r| r.unit_query_error.as_ref()),
        );
    }
    let Some(reconciled) = reconciled else {
        return unreconciled_task_detail(ctx, id, after);
    };
    let Some(t) = reconciled.state_of(id) else {
        // only a trusted complete absence is a 404. An incomplete or unreadable
        // collection, a running unit and any id the listing still carries stay UNKNOWN
        // instead of disappearing from the API.
        let listed = reconciled.tasks.iter().any(|t| t.id == id);
        let running = matches!(
            &reconciled.active,
            crate::tasks::ActiveState::Running { task_ids } if task_ids.iter().any(|t| t == id)
        );
        let unproven = !reconciled.collection_complete
            || reconciled.unit_query_error.is_some()
            || matches!(
                reconciled.active,
                crate::tasks::ActiveState::Unreadable { .. }
            );
        if listed || running || unproven {
            return unknown_task_response(
                id,
                if running { Some("missing") } else { None },
                reconciled.unit_query_error.as_ref(),
            );
        }
        return ApiError::new(StatusCode::NOT_FOUND, "not_found").into_response();
    };
    task_detail_response(ctx, id, after, t, reconciled.unit_query_error.as_ref())
}

/// Project a terminal record without scanning host inventory or the state directory.
/// Missing, unreadable and running records fall back to reconciliation, which decides interruption.
fn terminal_task_detail(ctx: &Ctx, id: &str, after: Option<u64>) -> Option<Response> {
    let state = match crate::task_state::read(ctx.host.as_ref(), &ctx.paths, id) {
        store::JsonRead::Ok(state) => state,
        _ => return None,
    };
    if state.status == Status::Running {
        return None;
    }
    let task = crate::tasks::ReconciledTask {
        id: id.to_string(),
        status: Some(state.status),
        phase: state.phase,
        state_note: state.state_note.clone(),
        state: Some(state),
        state_read_error: None,
    };
    Some(task_detail_response(ctx, id, after, &task, None))
}

/// The `UNKNOWN` detail of a task whose record could not be shown: a running unit without a
/// state file, or a collection that is not trustworthy.
fn unknown_task_response(
    id: &str,
    state_read_error: Option<&str>,
    unit_query_error: Option<&crate::task_state::ReasonValue>,
) -> Response {
    json_response(json!({
        "task_id": id,
        "status": "UNKNOWN",
        "state_read_error": state_read_error,
        "state_note": null,
        "recovery_material_kept": false,
        "phase": null,
        "created_at": null,
        "finished_at": null,
        "interrupt_discovered_at": null,
        "plan": null,
        "loaded_version_start": null,
        "loaded_version_end": null,
        "download": null,
        "replace": [],
        "checks": [],
        "restore": null,
        "error": null,
        "gpu_users": null,
        "nvidia_smi_gpus": null,
        "version_change": false,
        "notice": null,
        "log": {"lines": [], "next": 0, "reset": false},
        "unit_query": unit_query_error.map(|r| json!({
            "reason": {"key": r.key, "params": r.params},
        })),
    }))
}

/// task detail when reconciliation is skipped (`pid: host` failed): the state files and the
/// unit list are left untouched; only the state file and the log are read.
fn unreconciled_task_detail(ctx: &Ctx, id: &str, after: Option<u64>) -> Response {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    let read = crate::task_state::read(h, p, id);
    let (status, state_read_error, state) = match read {
        store::JsonRead::Ok(state) => (state.status.as_str().to_string(), Value::Null, Some(state)),
        store::JsonRead::Missing => {
            // A trusted disk `Missing` is a real absence: this path lists the state directory
            // directly, so the 404 stays.
            return ApiError::new(StatusCode::NOT_FOUND, "not_found").into_response();
        }
        other => (
            "UNKNOWN".to_string(),
            json!(store::state_read_error_key(&other)),
            None,
        ),
    };
    let times = crate::tasks::project_task_times(state.as_ref().map(|s| s.status), state.as_ref());
    let log = task_state::log_tail(h, p, id, after);
    let log_json = json!({
        "lines": log.lines,
        "next": log.next,
        "reset": log.reset,
    });
    let version_change = state.as_ref().map(version_change).unwrap_or(false);
    let notice = state.as_ref().and_then(notice_json);
    let body = json!({
        "task_id": id,
        "status": status,
        "state_read_error": state_read_error,
        "state_note": state.as_ref().and_then(|s| s.state_note.as_ref()).map(|n| json!(n)),
        "recovery_material_kept": h.lstat(&p.task_dir(id)).is_ok(),
        "released_by": state.as_ref().and_then(|s| s.released_by.as_ref()),
        "released_at": state.as_ref().and_then(|s| s.released_at),
        "phase": state.as_ref().and_then(|s| s.phase).map(|p| p.as_str()),
        "created_at": opt_millis(times.created_at),
        "finished_at": opt_millis(times.finished_at),
        "interrupt_discovered_at": opt_millis(times.interrupt_discovered_at),
        "plan": state.as_ref().and_then(|s| s.plan.as_ref().map(plan_json)),
        "loaded_version_start": state.as_ref().and_then(|s| s.loaded_version_start.as_ref().map(reading_json_generic)),
        "loaded_version_end": state.as_ref().and_then(|s| s.loaded_version_end.as_ref().map(reading_json_generic)),
        "download": state.as_ref().and_then(|s| s.download.as_ref()).map(|d| json!(d)),
        "replace": state.as_ref().map(|s| json!(s.replace)).unwrap_or(json!([])),
        "checks": state.as_ref().map(|s| json!(s.checks)).unwrap_or(json!([])),
        "restore": state.as_ref().and_then(|s| s.restore.as_ref()).map(|r| json!(r)),
        "error": state.as_ref().and_then(|s| s.error.as_ref()).map(|e| json!({
            "code": e.code,
            "params": e.params,
            "command": e.command,
            "exit": e.exit,
            "raw": e.raw,
        })),
        "gpu_users": state.as_ref().and_then(|s| s.gpu_users.as_ref()).map(|g| json!(g)),
        "nvidia_smi_gpus": state.as_ref().and_then(|s| s.nvidia_smi_gpus.as_ref()).map(|g| json!(g)),
        "version_change": version_change,
        "notice": notice,
        "log": log_json,
        "unit_query": {"reason": startup_check_reason()},
    });
    json_response(body)
}

/// Build the task detail body from one reconciled task.
fn task_detail_response(
    ctx: &Ctx,
    id: &str,
    after: Option<u64>,
    t: &crate::tasks::ReconciledTask,
    unit_query_error: Option<&crate::task_state::ReasonValue>,
) -> Response {
    let h = ctx.host.as_ref();
    let p = &ctx.paths;
    let state = t.state.clone();
    let status = t.status_for_ui();
    let times = crate::tasks::project_task_times(t.status, state.as_ref());
    let version_change = state.as_ref().map(version_change).unwrap_or(false);
    let notice = state.as_ref().and_then(notice_json);
    let log = task_state::log_tail(h, p, id, after);
    let log_json = json!({
        "lines": log.lines,
        "next": log.next,
        "reset": log.reset,
    });
    let mut body = json!({
        "task_id": id,
        "status": status,
        "state_read_error": t.state_read_error,
        "state_note": t.state_note,
        "recovery_material_kept": h.lstat(&p.task_dir(id)).is_ok(),
        "released_by": state.as_ref().and_then(|s| s.released_by.as_ref()),
        "released_at": state.as_ref().and_then(|s| s.released_at),
        "phase": state.as_ref().and_then(|s| s.phase).map(|p| p.as_str()),
        "created_at": opt_millis(times.created_at),
        "finished_at": opt_millis(times.finished_at),
        "interrupt_discovered_at": opt_millis(times.interrupt_discovered_at),
        "plan": state.as_ref().and_then(|s| s.plan.as_ref().map(plan_json)),
        "loaded_version_start": state.as_ref().and_then(|s| s.loaded_version_start.as_ref().map(reading_json_generic)),
        "loaded_version_end": state.as_ref().and_then(|s| s.loaded_version_end.as_ref().map(reading_json_generic)),
        "download": state.as_ref().and_then(|s| s.download.as_ref()).map(|d| json!(d)),
        "replace": state.as_ref().map(|s| json!(s.replace)).unwrap_or(json!([])),
        "checks": state.as_ref().map(|s| json!(s.checks)).unwrap_or(json!([])),
        "restore": state.as_ref().and_then(|s| s.restore.as_ref()).map(|r| json!(r)),
        "error": state.as_ref().and_then(|s| s.error.as_ref()).map(|e| json!({
            "code": e.code,
            "params": e.params,
            "command": e.command,
            "exit": e.exit,
            "raw": e.raw,
        })),
        "gpu_users": state.as_ref().and_then(|s| s.gpu_users.as_ref()).map(|g| json!(g)),
        "nvidia_smi_gpus": state.as_ref().and_then(|s| s.nvidia_smi_gpus.as_ref()).map(|g| json!(g)),
        "version_change": version_change,
        "notice": notice,
        "log": log_json,
        "unit_query": unit_query_error.map(|r| json!({
            "reason": {"key": r.key, "params": r.params},
        })),
    });
    if status == "RUNNING" && state.as_ref().is_some_and(|s| s.waiting_gpu == Some(true)) {
        body["waiting_gpu"] = json!(true);
    }
    json_response(body)
}

fn reading_json_generic<T: serde::Serialize>(r: &crate::task_state::ReadingJson<T>) -> Value {
    json!(r)
}
fn plan_json(plan: &crate::task_state::Plan) -> Value {
    json!(plan)
}

/// version-change notice is computed at read time.
pub(super) fn version_change(state: &TaskState) -> bool {
    if !matches!(
        state.status,
        Status::Succeeded | Status::StockRestored | Status::Failed
    ) {
        return false;
    }
    let start = state
        .loaded_version_start
        .as_ref()
        .and_then(|r| r.as_value())
        .cloned();
    let end = state
        .loaded_version_end
        .as_ref()
        .and_then(|r| r.as_value())
        .cloned();
    let (Some(start), Some(end)) = (start, end) else {
        return false;
    };
    if start == end {
        return false;
    }
    state
        .gpu_users
        .as_ref()
        .map(|g| !g.users.is_empty() || !g.errors.is_empty())
        .unwrap_or(false)
}

/// The result notification of a finished task.
pub(super) fn notice_json(state: &TaskState) -> Option<Value> {
    match state.status {
        Status::Succeeded => {
            // The check passed, but a module of the effective pass may not have
            // come back: the success notice then names it and says what to do,
            // instead of leaving the failure only in the task log.
            let failed: Vec<&str> = state
                .replace
                .last()
                .map(|r| {
                    r.final_reloaded()
                        .iter()
                        .filter(|x| !x.ok)
                        .map(|x| x.module.as_str())
                        .collect()
                })
                .unwrap_or_default();
            let version = state
                .plan
                .as_ref()
                .map(|p| p.version.clone())
                .unwrap_or_default();
            if failed.is_empty() {
                Some(json!({
                    "key": "notice.update_complete",
                    "params": {"version": version},
                }))
            } else {
                Some(json!({
                    "key": "notice.update_complete",
                    "variant": "reload_warning",
                    "params": {"version": version, "modules": failed.join(", ")},
                }))
            }
        }
        Status::StockRestored => {
            let restore = state.restore.as_ref()?;
            let original = restore.original.as_ref()?;
            Some(json!({
                "key": "notice.restored",
                "params": {
                    "target": state.plan.as_ref().map(|p| p.version.clone()).unwrap_or_default(),
                    "original": original.version,
                    "date": original.saved_at,
                },
            }))
        }
        Status::Failed => {
            let error = state.error.as_ref()?;
            let code = error.code.as_str();
            let reload_failed = |pass: crate::task_state::Pass| {
                state
                    .replace
                    .iter()
                    .find(|r| r.pass == pass)
                    .map(|r| r.final_reloaded().iter().any(|x| !x.ok))
                    .unwrap_or(false)
            };
            match ErrorCode::parse(code) {
                Some(ErrorCode::GpuInUse) => Some(json!({
                    "key": "notice.gpu_in_use",
                    "variant": if error.params.get("occupancy").and_then(|v| v.as_str()) == Some("unreadable") {
                        "task_unreadable"
                    } else {
                        "task"
                    },
                    "params": {},
                })),
                Some(ErrorCode::ModuleUnloadRefused) => {
                    let mut params = json!({
                        "reload_failed": reload_failed(crate::task_state::Pass::Update),
                    });
                    if error.params.get("waited_in_use").and_then(|v| v.as_bool()) == Some(true) {
                        params["waited_in_use"] = json!(true);
                    }
                    Some(json!({
                        "key": "notice.unload_refused",
                        "params": params,
                    }))
                }
                Some(ErrorCode::RestoreAborted) => {
                    let cause = error
                        .params
                        .get("cause")
                        .and_then(|v| v.as_str())
                        .unwrap_or("busy");
                    let mut params = json!({
                        "reload_failed": reload_failed(crate::task_state::Pass::Restore),
                    });
                    if error.params.get("waited_in_use").and_then(|v| v.as_bool()) == Some(true) {
                        params["waited_in_use"] = json!(true);
                    }
                    Some(json!({
                        "key": "notice.restore_stopped",
                        "variant": cause,
                        "params": params,
                    }))
                }
                Some(ErrorCode::RestoreNotWritten) => Some(json!({
                    "key": "notice.restore_failed",
                    "variant": "before_write",
                    "params": {"reload_failed": reload_failed(crate::task_state::Pass::Restore)},
                })),
                Some(ErrorCode::RestoreCheckFailed) => Some(json!({
                    "key": "notice.restore_failed",
                    "variant": "written",
                    "params": {"reload_failed": false},
                })),
                Some(ErrorCode::NoUsableOriginal) => Some(json!({
                    "key": "notice.no_restore",
                    "params": {},
                })),
                Some(ErrorCode::SaveOriginalFailed) => Some(json!({
                    "key": "code.explanation",
                    "params": {"code": code, "reload_failed": false},
                })),
                Some(ErrorCode::WriteFailed) => Some(json!({
                    "key": "code.explanation",
                    "params": {"code": code, "reload_failed": reload_failed(crate::task_state::Pass::Update)},
                })),
                Some(ErrorCode::ReplaceDeadlineExceeded) => Some(json!({
                    "key": "code.explanation",
                    "params": {"code": code, "reload_failed": reload_failed(crate::task_state::Pass::Update)},
                })),
                Some(ErrorCode::DownloadFailed) | Some(ErrorCode::BuildFailed) => {
                    Some(json!({"key": "code.explanation", "params": {"code": code}}))
                }
                Some(ErrorCode::BuildTimeout) => {
                    Some(json!({"key": "code.explanation", "params": {"code": code}}))
                }
                None => None,
            }
        }
        Status::Running | Status::Interrupted => None,
    }
}

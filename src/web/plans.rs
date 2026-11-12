use super::*;

pub(super) async fn cancel_plan(
    State((ctx, _)): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if !store::group_id_ok(&id) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let _guard = ctx.process_lock.lock().await;
    if let Some(response) = reject_if_session_lost(&ctx, &headers) {
        return response;
    }
    plan::cancel(&ctx, &id);
    json_response(json!({}))
}

// ---- plan creation --------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanRequest {
    source: SourceRequest,
}

pub(super) async fn create_plan(
    State((ctx, _)): State<AppState>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<PlanRequest>,
) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let versions = if matches!(&req.source, SourceRequest::Official { .. }) {
            catalog::read_versions(ctx2.host.as_ref())
        } else {
            Ok(Vec::new())
        };
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        match plan::precheck(&ctx2, &req.source, &versions) {
            Ok((Precheck::Ready(dialog), snapshot)) => {
                let plan_id = plan::remember(&ctx2, snapshot);
                json_response(json!({
                    "result": "ready",
                    "plan_id": plan_id,
                    "dialog": dialog,
                }))
            }
            Ok((other, _)) => plan_response(other),
            Err(e) => source_invalid_response(e),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

fn plan_response(precheck: Precheck) -> Response {
    match precheck {
        Precheck::Ready(dialog) => json_response(json!({"result": "ready", "dialog": dialog})),
        Precheck::Blocked { failures, gpu } => {
            let mut obj = serde_json::Map::new();
            obj.insert("result".into(), json!("blocked"));
            obj.insert("failures".into(), json!(failures));
            obj.insert(
                "gpu".into(),
                match gpu {
                    Some(g) => json!(g),
                    None => Value::Null,
                },
            );
            json_response(Value::Object(obj))
        }
        Precheck::Uncomputable { items } => json_response(json!({
            "result": "uncomputable",
            "items": items.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
        })),
    }
}

fn source_invalid_response(e: plan::SourceInvalid) -> Response {
    use plan::SourceInvalid as S;
    let status = match &e {
        S::SetupBlocked
        | S::VersionUnconfirmed { .. }
        | S::BranchUndecidable { .. }
        | S::GpuUndecidable { .. }
        | S::GpusChanged
        | S::UploadBusy => StatusCode::CONFLICT,
        S::RequestInvalid => StatusCode::BAD_REQUEST,
        S::LocalRowMissing => StatusCode::NOT_FOUND,
        S::VersionFormat
        | S::VersionBelow470
        | S::BranchUnbuildable { .. }
        | S::VersionNotOfficial { .. }
        | S::GpuUnsupported { .. }
        | S::UploadGone
        | S::UploadIncomplete { .. }
        | S::UploadProblem
        | S::UploadVersionRequired
        | S::UploadVersionNotCandidate { .. }
        | S::ToolkitTooOld { .. }
        | S::GpuUnlockUnsupported => StatusCode::UNPROCESSABLE_ENTITY,
    };
    let (key, params) = source_invalid_details(e);
    ApiError::new(status, key).params(params).into_response()
}

fn source_invalid_details(e: plan::SourceInvalid) -> (&'static str, Value) {
    use plan::SourceInvalid as S;
    match e {
        S::SetupBlocked => ("setup.blocked", json!({})),
        S::RequestInvalid => ("request.invalid", json!({})),
        S::VersionFormat => ("plan.source_invalid.version_format", json!({})),
        S::VersionBelow470 => ("plan.source_invalid.version_below_470", json!({})),
        S::BranchUnbuildable {
            version,
            from,
            reasons,
        } => (
            "plan.source_invalid.branch_unbuildable",
            json!({
                // Every reason field has the `{key, params}` shape; these phrases have no
                // placeholder.
                "version": version,
                "from": from,
                "reasons": reasons
                    .iter()
                    .map(|key| json!({"key": key, "params": {}}))
                    .collect::<Vec<_>>(),
            }),
        ),
        S::VersionNotOfficial { version } => (
            "plan.source_invalid.version_not_official",
            json!({"version": version}),
        ),
        S::VersionUnconfirmed {
            version,
            unreadable,
        } => (
            "plan.source_invalid.version_unconfirmed",
            json!({"version": version, "unreadable": unreadable}),
        ),
        S::BranchUndecidable { unreadable } => (
            "plan.source_invalid.branch_undecidable",
            json!({"unreadable": unreadable.iter().map(|u| u.to_json()).collect::<Vec<_>>()}),
        ),
        S::GpuUndecidable { unreadable } => (
            "plan.source_invalid.gpu_undecidable",
            json!({"unreadable": unreadable.iter().map(|u| u.to_json()).collect::<Vec<_>>()}),
        ),
        S::GpuUnsupported {
            version,
            gpus,
            conflict,
        } => (
            "plan.source_invalid.gpu_unsupported",
            json!({"version": version, "gpus": gpus, "conflict": conflict}),
        ),
        S::GpusChanged => ("plan.changed", json!({"changed": ["gpus"]})),
        S::UploadGone => ("plan.source_invalid.upload_group_gone", json!({})),
        S::UploadBusy => ("upload.busy", json!({})),
        S::UploadIncomplete { missing } => (
            "plan.source_invalid.upload_incomplete",
            json!({"missing": missing}),
        ),
        S::UploadProblem => ("plan.source_invalid.upload_problem", json!({})),
        S::UploadVersionRequired => ("plan.source_invalid.upload_version_required", json!({})),
        S::UploadVersionNotCandidate { version } => (
            "plan.source_invalid.upload_version_not_candidate",
            json!({"version": version}),
        ),
        S::ToolkitTooOld {
            driver_version,
            toolkit_version,
        } => (
            "plan.source_invalid.toolkit_too_old",
            json!({"driver_version": driver_version, "toolkit_version": toolkit_version}),
        ),
        S::LocalRowMissing => ("plan.source_invalid.local_row_missing", json!({})),
        S::GpuUnlockUnsupported => ("plan.source_invalid.gpu_unlock_unsupported", json!({})),
    }
}

// ---- task submission -------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskRequest {
    plan_id: String,
    button: plan::Button,
}

pub(super) async fn submit_task(
    State((ctx, _)): State<AppState>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<TaskRequest>,
) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        if !store::group_id_ok(&req.plan_id) {
            return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
        }
        match plan::submit(&ctx2, &req.plan_id, req.button) {
            Ok(task_id) => json_response_status(StatusCode::ACCEPTED, json!({"task_id": task_id})),
            Err(reject) => submit_reject_response(reject),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

fn submit_reject_response(reject: SubmitReject) -> Response {
    match reject {
        SubmitReject::Used => ApiError::new(StatusCode::CONFLICT, "plan.used").into_response(),
        SubmitReject::SetupBlocked => {
            ApiError::new(StatusCode::CONFLICT, "setup.blocked").into_response()
        }
        SubmitReject::ButtonInvalid => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "plan.button_invalid").into_response()
        }
        SubmitReject::TaskRunning => {
            ApiError::new(StatusCode::CONFLICT, "task.running").into_response()
        }
        SubmitReject::PrecheckFailed { failures } => {
            ApiError::new(StatusCode::CONFLICT, "plan.precheck_failed")
                .params(json!({"failures": failures}))
                .into_response()
        }
        SubmitReject::Uncomputable { items } => {
            ApiError::new(StatusCode::CONFLICT, "plan.uncomputable")
                .params(json!({
                    "items": items.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                }))
                .into_response()
        }
        SubmitReject::UploadBusy => {
            ApiError::new(StatusCode::CONFLICT, "upload.busy").into_response()
        }
        SubmitReject::Changed { changed } => ApiError::new(StatusCode::CONFLICT, "plan.changed")
            .params(json!({"changed": changed}))
            .into_response(),
        SubmitReject::Failed { reason, raw } => {
            let mut error = ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "submit.failed")
                .params(json!({"reason": {"key": reason.key, "params": reason.params}}));
            if let Some(raw) = raw {
                error = error.raw(raw);
            }
            error.into_response()
        }
    }
}

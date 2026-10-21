use super::*;

// ---- upload creation / file upload / current upload
// ----------------------------------------------------------

const UPLOAD_TOOLKIT_FALLBACK: &str = "1.20.1-1";

pub(super) async fn upload_toolkit_version(State((ctx, _)): State<AppState>) -> Response {
    match tokio::task::spawn_blocking(move || {
        let h = ctx.host.as_ref();
        // A request-specific scratch directory prevents concurrent readers sharing partial bytes.
        let dir = ctx
            .paths
            .manager()
            .join(format!(".toolkit-index-{}", store::random_hex(16)));
        let host_dir =
            crate::host::host_path_of_data_dir(ctx.startup.host_data_dir.as_deref(), &dir);
        let pick = host_dir.and_then(|host_dir| {
            if !ctx.startup.pid_host_ok || h.create_dir(&dir, 0o700).is_err() {
                return None;
            }
            crate::runner::download::read_toolkit_index(
                h,
                &dir,
                &host_dir,
                HOST_QUERY_TIMEOUT,
                |cmd| h.run(cmd),
            )
            .ok()
        });
        let _ = h.remove_dir_all(&dir);
        match pick {
            Some(pick) => json_response(json!({"version": pick.version, "fallback": false})),
            None => json_response(json!({"version": UPLOAD_TOOLKIT_FALLBACK, "fallback": true})),
        }
    })
    .await
    {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

pub(super) async fn cancel_current_upload_plan(
    State((ctx, _)): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let _guard = ctx.process_lock.lock().await;
    if let Some(response) = reject_if_session_lost(&ctx, &headers) {
        return response;
    }
    plan::cancel_current_upload(&ctx);
    json_response(json!({}))
}

pub(super) async fn open_upload(State((ctx, _)): State<AppState>, headers: HeaderMap) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        if let Some(response) = reject_if_session_lost(&ctx2, &headers) {
            return response;
        }
        match crate::upload::open_group(ctx2.host.as_ref(), &ctx2.paths, &ctx2.upload) {
            Ok(group) => json_response_status(StatusCode::CREATED, json!({"group_id": group})),
            Err(crate::upload::UploadReject::Busy { params }) => {
                ApiError::new(StatusCode::CONFLICT, "upload.busy")
                    .params(params)
                    .into_response()
            }
            Err(crate::upload::UploadReject::GroupReplaced) => {
                ApiError::new(StatusCode::CONFLICT, "upload.group_replaced").into_response()
            }
            Err(crate::upload::UploadReject::Unauthorized) => {
                ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response()
            }
            Err(crate::upload::UploadReject::Rejected {
                file_name,
                reason,
                status,
                params,
            }) => upload_reject_response(&file_name, reason, status, params),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

fn upload_reject_response(file_name: &str, reason: &str, status: u16, params: Value) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
    let mut params = params;
    if let Some(map) = params.as_object_mut() {
        map.insert("file_name".into(), json!(file_name));
        map.insert("reason".into(), json!(reason));
    }
    ApiError::new(code, "upload.rejected")
        .params(params)
        .into_response()
}

pub(super) async fn receive_upload_file(
    State((ctx, _)): State<AppState>,
    AxumPath(group_id): AxumPath<String>,
    request: Request,
) -> Response {
    // Parameter validation: the group id and the file name are checked before anything
    // is looked up or written; a name that is not valid UTF-8 after percent decoding, empty,
    // longer than 255 bytes or containing `/` or a control character is a format error.
    if !store::group_id_ok(&group_id) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let query = request.uri().query().unwrap_or("").to_string();
    let name = match query_param(&query, "name") {
        QueryParam::Value(name) => name,
        // Absent and not-valid-UTF-8 are both a format error here, but they stay distinct at
        // the parsing boundary.
        QueryParam::Missing | QueryParam::Invalid => {
            return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response()
        }
    };
    if !crate::upload::name_ok(&name) {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let content_length = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let headers = request.headers().clone();
    let body = request.into_body();
    let ctx2 = ctx.clone();
    let session_ctx = ctx.clone();
    let session_ok = move || {
        session_ctx
            .auth
            .check_session_at(session_ctx.host.as_ref(), &session_ctx.paths, &headers)
            .is_ok()
    };
    let result = crate::upload::receive(
        ctx2.host.clone(),
        ctx2.paths.clone(),
        ctx2.startup.host_data_dir.clone(),
        group_id,
        name,
        content_length,
        body,
        ctx2.upload.clone(),
        &ctx2.process_lock,
        session_ok,
    )
    .await;
    match result {
        Ok((file, group)) => {
            let _ = &group;
            json_response(json!({"file": file, "group": group}))
        }
        Err(crate::upload::UploadReject::Busy { params }) => {
            ApiError::new(StatusCode::CONFLICT, "upload.busy")
                .params(params)
                .into_response()
        }
        Err(crate::upload::UploadReject::GroupReplaced) => {
            ApiError::new(StatusCode::CONFLICT, "upload.group_replaced").into_response()
        }
        Err(crate::upload::UploadReject::Unauthorized) => {
            ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response()
        }
        Err(crate::upload::UploadReject::Rejected {
            file_name,
            reason,
            status,
            params,
        }) => {
            if reason == "interrupted" {
                ApiError::new(StatusCode::BAD_REQUEST, "upload.interrupted")
                    .params(json!({"file_name": file_name}))
                    .into_response()
            } else {
                upload_reject_response(&file_name, reason, status, params)
            }
        }
    }
}

pub(super) async fn current_upload(State((ctx, _)): State<AppState>) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.process_lock.blocking_lock();
        match ctx2.upload.current() {
            Some(group) => json_response(json!(crate::upload::summary(
                ctx2.host.as_ref(),
                &ctx2.paths,
                &group
            ))),
            None => json_response(Value::Null),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

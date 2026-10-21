use super::*;

// ---- login status / password login
// ---------------------------------------------------------------

pub(super) async fn login_status(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
    headers: HeaderMap,
) -> Response {
    let ctx2 = ctx.clone();
    let session2 = session.clone();
    let prepared =
        tokio::task::spawn_blocking(move || prepare_login_status(&ctx2, &session2)).await;
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(_) => return internal().into_response(),
    };
    // Only a signed-in reader consumes the reconcile result (the login branch of the response
    // below), so an anonymous login-page read must not take the process lock or run the host
    // unit query (cleanup 20261010). The wait for the process lock and the reconcile together
    // are bounded by `READING_TIMEOUT`: past the deadline the timeout reason is returned
    // instead of holding the response open.
    let active = if session.0.is_some() {
        match reconcile_bounded(&ctx, &headers).await {
            Ok(reconciled) => reconciled.map(|r| r.active),
            Err(SessionLost) => {
                return with_common_headers(
                    ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response(),
                );
            }
        }
    } else {
        None
    };
    with_common_headers(login_status_response(
        ctx.as_ref(),
        &session,
        prepared,
        active,
    ))
}

struct PreparedLoginStatus {
    credentials: CredentialState,
    link_missing: Option<bool>,
    problems: Vec<Value>,
    ssh_enabled: bool,
}

fn prepare_login_status(ctx: &Ctx, session: &SessionOpt) -> PreparedLoginStatus {
    let h = ctx.host.as_ref();
    // Everything that is not credential state runs before the credentials lock is taken: the
    // extension-link reading is a host fact, and a slow host read must not delay a login or a
    // one-time password change, which need the same lock. The lock below covers the
    // credentials alone.
    let link_missing = if ctx.startup.pid_host_ok {
        matches!(facts::link(h), facts::LinkReading::Missing).then_some(true)
    } else {
        None
    };
    let mut problems: Vec<Value> = ctx.settings.problems.iter().map(problem_json).collect();
    if let Some(problem) = startup_problem_json(&ctx.startup.problems, session.0.is_some()) {
        problems.push(problem);
    }
    let _guard = ctx.auth.credentials_lock.blocking_lock();
    let credentials = ctx.auth.load_state(h, &ctx.paths, &ctx.settings);
    if let Some(issued) = ctx
        .auth
        .ensure_setup_code(h, &ctx.paths, &ctx.settings, "reset")
    {
        issued.log();
    }
    // login status always carries the real `ssh_enabled`: true only when the credentials file
    // reads back valid and holds at least one registered key. It is read under the lock, with
    // the credentials it belongs to.
    let ssh_enabled = credentials.is_ok() && ctx.auth.ssh_key_count(h, &ctx.paths) > 0;
    PreparedLoginStatus {
        credentials,
        link_missing,
        problems,
        ssh_enabled,
    }
}

fn login_status_response(
    ctx: &Ctx,
    session: &SessionOpt,
    prepared: PreparedLoginStatus,
    active: Option<crate::tasks::ActiveState>,
) -> Response {
    let mut body = serde_json::Map::new();
    body.insert("logged_in".into(), json!(session.0.is_some()));
    if session.0.is_some() {
        body.insert(
            "update_blocked".into(),
            update_blocked_json(ctx, session, prepared.link_missing, active.as_ref()),
        );
    }
    let mut setup = json!({"problems": prepared.problems});
    if matches!(prepared.credentials, CredentialState::SetupRequired) {
        setup["username"] = json!(ctx.settings.username);
        setup["valid_hours"] = json!(24);
    }
    body.insert("setup".into(), setup);
    body.insert(
        "credentials".into(),
        credentials_state_json(&prepared.credentials),
    );
    body.insert(
        "show_reset_help".into(),
        json!(show_reset_help(ctx, &prepared.credentials)),
    );
    body.insert("ssh_enabled".into(), json!(prepared.ssh_enabled));
    let body = Value::Object(body);
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetupRequest {
    code: String,
    new_password: String,
}

pub(super) async fn setup_account(
    State((ctx, _)): State<AppState>,
    JsonBody(req): JsonBody<SetupRequest>,
) -> Response {
    let _guard = ctx.auth.credentials_lock.lock().await;
    let result = ctx
        .auth
        .setup(
            ctx.host.clone(),
            ctx.paths.clone(),
            ctx.settings.clone(),
            req.code,
            req.new_password,
        )
        .await;
    let response = match result {
        Ok(session) => {
            let mut response = json_response(json!({"token": session.token}));
            if let Ok(cookie) = HeaderValue::from_str(&auth::session_set_cookie(&session.cookie)) {
                response.headers_mut().insert(header::SET_COOKIE, cookie);
            }
            response
        }
        Err(auth::SetupError::NotAvailable) => {
            ApiError::new(StatusCode::CONFLICT, "setup.not_available").into_response()
        }
        Err(auth::SetupError::CodeInvalid {
            show_help,
            code_renewed,
        }) => ApiError::new(StatusCode::FORBIDDEN, "setup.code_invalid")
            .params(json!({"show_help": show_help, "code_renewed": code_renewed}))
            .into_response(),
        Err(auth::SetupError::RateLimited { retry_after_s }) => {
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "login.rate_limited")
                .params(json!({"retry_after_s":retry_after_s}))
                .into_response()
        }
        Err(auth::SetupError::Rule(rule)) => {
            ApiError::new(StatusCode::BAD_REQUEST, "password.rule")
                .params(json!({"rule":rule.key()}))
                .into_response()
        }
        Err(auth::SetupError::Credentials(state)) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "login.credentials_unusable",
        )
        .params(json!({"state":state.to_json()}))
        .into_response(),
        Err(auth::SetupError::Unsaved(reason)) => {
            crate::log::error_json(
                Event::CredentialsUnsaved,
                &[("reason", reason.log_reason_object(json!({})))],
            );
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "login.credentials_unsaved",
            )
            .params(json!({"reason":reason.as_str()}))
            .into_response()
        }
    };
    with_common_headers(response)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoginRequest {
    username: String,
    password: String,
}

pub(super) async fn password_login(
    State((ctx, _)): State<AppState>,
    JsonBody(req): JsonBody<LoginRequest>,
) -> Response {
    let ctx2 = ctx.clone();
    let credentials = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.auth.credentials_lock.blocking_lock();
        ctx2.auth
            .load_state(ctx2.host.as_ref(), &ctx2.paths, &ctx2.settings)
    })
    .await
    .unwrap_or(CredentialState::Unreadable(
        auth::UnreadableReason::FileUnreadable,
    ));
    if !credentials.is_ok() {
        let response = match &credentials {
            CredentialState::SetupRequired => {
                ApiError::new(StatusCode::CONFLICT, "login.setup_required").into_response()
            }
            CredentialState::Unsaved(_) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "login.credentials_unsaved",
            )
            .params(json!({"reason": state_json_reason(&credentials)}))
            .into_response(),
            _ => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "login.credentials_unusable",
            )
            .params(json!({"state": credentials_state_json(&credentials)}))
            .into_response(),
        };
        return with_common_headers(response);
    }
    let outcome = ctx
        .auth
        .login(
            ctx.host.clone(),
            ctx.paths.clone(),
            ctx.settings.clone(),
            req.username.clone(),
            req.password.clone(),
        )
        .await;
    let response = match outcome {
        auth::LoginOutcome::Ok { cookie, token } => {
            crate::log::info(Event::LoginSucceeded, &[]);
            let body = json!({"token": token});
            let mut response = Response::new(Body::from(body.to_string()));
            *response.status_mut() = StatusCode::OK;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            if let Ok(value) = HeaderValue::from_str(&auth::session_set_cookie(&cookie)) {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            response
        }
        auth::LoginOutcome::Failed {
            show_reset_help,
            consecutive_failures,
        } => {
            crate::log::info_json(
                Event::LoginFailed,
                &[("consecutive_failures", json!(consecutive_failures))],
            );
            ApiError::new(StatusCode::UNAUTHORIZED, "login.failed")
                .params(json!({"show_reset_help": show_reset_help}))
                .into_response()
        }
        auth::LoginOutcome::RateLimited {
            retry_after_s,
            show_reset_help,
        } => {
            crate::log::info(
                Event::LoginRateLimited,
                &[("retry_after_s", &retry_after_s.to_string())],
            );
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "login.rate_limited")
                .params(json!({
                    "retry_after_s": retry_after_s,
                    "show_reset_help": show_reset_help,
                }))
                .into_response()
        }
        auth::LoginOutcome::Credentials(CredentialState::SetupRequired) => {
            ApiError::new(StatusCode::CONFLICT, "login.setup_required").into_response()
        }
        auth::LoginOutcome::Credentials(state) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "login.credentials_unusable",
        )
        .params(json!({"state": credentials_state_json(&state)}))
        .into_response(),
    };
    with_common_headers(response)
}

fn state_json_reason(state: &CredentialState) -> Value {
    match state {
        CredentialState::Unsaved(r) => json!(r.as_str()),
        _ => json!("file"),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PasswordRequest {
    new_password: String,
}

pub(super) async fn change_password(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<PasswordRequest>,
) -> Response {
    if session.0.is_none() {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        // password change runs under the credentials lock.
        let _guard = ctx2.auth.credentials_lock.blocking_lock();
        // A request that already passed the session middleware may have queued on this lock
        // while a concurrent key revoke rotated the `session_key`; recheck the request's own
        // cookie under the lock so such a stale session cannot change the password or mint a
        // fresh cookie.
        if ctx2
            .auth
            .check_session_at(ctx2.host.as_ref(), &ctx2.paths, &headers)
            .is_err()
        {
            return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
        }
        match ctx2
            .auth
            .change_password(ctx2.host.as_ref(), &ctx2.paths, &req.new_password)
        {
            Ok(issued) => {
                crate::log::info(Event::PasswordChanged, &[]);
                let mut response = json_response(json!({"token": issued.token}));
                if let Ok(value) = HeaderValue::from_str(&auth::session_set_cookie(&issued.cookie))
                {
                    response.headers_mut().insert(header::SET_COOKIE, value);
                }
                response
            }
            Err(auth::PasswordChangeError::Rule(rule)) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, rule.key()).into_response()
            }
            Err(auth::PasswordChangeError::Unsaved(reason)) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "password.unsaved")
                    .params(json!({"reason": reason.as_str()}))
                    .into_response()
            }
            Err(auth::PasswordChangeError::Credentials(state)) => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "login.credentials_unusable",
            )
            .params(json!({"state": credentials_state_json(&state)}))
            .into_response(),
        }
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

// ---- SSH sign-in -----------------------------------------

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SshChallengeRequest {}

/// `POST /api/ssh/challenge`: public, blind entropy. The issue bucket (capacity 10, one token
/// per 10 seconds) and the outstanding cap (256) bound the memory an unauthenticated caller
/// can consume.
pub(super) async fn ssh_challenge(
    State((ctx, _)): State<AppState>,
    JsonBody(_req): JsonBody<SshChallengeRequest>,
) -> Response {
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _guard = ctx2.auth.credentials_lock.blocking_lock();
        if matches!(
            ctx2.auth
                .load_state(ctx2.host.as_ref(), &ctx2.paths, &ctx2.settings),
            CredentialState::SetupRequired
        ) {
            return Err(CredentialState::SetupRequired);
        }
        Ok::<_, CredentialState>(ctx2.auth.ssh_challenges.issue(ctx2.host.as_ref()))
    })
    .await;
    let result = match result {
        Ok(Err(CredentialState::SetupRequired)) => {
            return ApiError::new(StatusCode::CONFLICT, "login.setup_required").into_response()
        }
        Ok(Err(state)) => {
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "login.credentials_unusable",
            )
            .params(json!({"state":state.to_json()}))
            .into_response()
        }
        Ok(Ok(result)) => Ok(result),
        Err(error) => Err(error),
    };
    let issued = match result {
        Ok(Ok(issued)) => issued,
        Ok(Err(crate::ssh_login::IssueError::Busy)) => {
            return ApiError::new(StatusCode::TOO_MANY_REQUESTS, "ssh.challenge_busy")
                .into_response()
        }
        _ => return internal().into_response(),
    };
    let body = json!({
        "challenge_id": issued.id,
        "challenge": issued.message,
        "namespace": SSH_NAMESPACE,
        "expires_in_s": SSH_CHALLENGE_TTL.as_secs(),
    });
    with_common_headers(json_response(body))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SshLoginRequest {
    challenge_id: String,
    signature: String,
}

/// `POST /api/ssh/login`. The field limits are explicit — `JsonBody` only checks the media
/// type, the total body and the decode.
pub(super) async fn ssh_login_route(
    State((ctx, _)): State<AppState>,
    JsonBody(req): JsonBody<SshLoginRequest>,
) -> Response {
    if req.challenge_id.len() > SSH_CHALLENGE_ID_MAX || req.signature.len() > SSH_SIGNATURE_MAX {
        return ApiError::new(StatusCode::BAD_REQUEST, "request.invalid").into_response();
    }
    let outcome = crate::ssh_login::ssh_login(
        ctx.auth.as_ref(),
        ctx.host.clone(),
        &ctx.paths,
        req.challenge_id,
        req.signature,
    )
    .await;
    let response = match outcome {
        crate::ssh_login::SshLoginOutcome::Ok {
            cookie,
            token,
            fingerprint,
            algorithm,
        } => {
            // the success line names the verified registered key's public fingerprint and
            // algorithm — nothing else.
            crate::log::info_json(
                Event::SshLoginSucceeded,
                &[
                    ("fingerprint", json!(fingerprint)),
                    ("algorithm", json!(algorithm)),
                ],
            );
            let body = json!({"token": token});
            let mut response = json_response(body);
            if let Ok(value) = HeaderValue::from_str(&auth::session_set_cookie(&cookie)) {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            response
        }
        crate::ssh_login::SshLoginOutcome::ChallengeInvalid => {
            ApiError::new(StatusCode::UNAUTHORIZED, "ssh.challenge_invalid").into_response()
        }
        crate::ssh_login::SshLoginOutcome::Failed {
            kind,
            fingerprint,
            algorithm,
            show_reset_help,
        } => {
            // The log line carries only the public fingerprint, the algorithm and the fixed
            // reason category: the consecutive-failure count stays in the shared bucket and
            // in the password login-shaped response params below.
            let mut fields: Vec<(&str, serde_json::Value)> =
                vec![("reason", json!(kind.log_reason()))];
            if let Some(fingerprint) = &fingerprint {
                fields.push(("fingerprint", json!(fingerprint)));
            }
            if let Some(algorithm) = &algorithm {
                fields.push(("algorithm", json!(algorithm)));
            }
            crate::log::info_json(Event::SshLoginFailed, &fields);
            // Parsing and verification share one error code; an unregistered key has its own.
            let key = if kind == crate::ssh_login::SshFailKind::KeyNotRegistered {
                "ssh.key_not_registered"
            } else {
                "ssh.login_failed"
            };
            ApiError::new(StatusCode::UNAUTHORIZED, key)
                .params(json!({"show_reset_help": show_reset_help}))
                .into_response()
        }
        crate::ssh_login::SshLoginOutcome::RateLimited {
            retry_after_s,
            show_reset_help,
        } => {
            crate::log::info(
                Event::LoginRateLimited,
                &[("retry_after_s", &retry_after_s.to_string())],
            );
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "login.rate_limited")
                .params(json!({
                    "retry_after_s": retry_after_s,
                    "show_reset_help": show_reset_help,
                }))
                .into_response()
        }
        crate::ssh_login::SshLoginOutcome::Credentials(CredentialState::SetupRequired) => {
            ApiError::new(StatusCode::CONFLICT, "login.setup_required").into_response()
        }
        crate::ssh_login::SshLoginOutcome::Credentials(state) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "login.credentials_unusable",
        )
        .params(json!({"state": credentials_state_json(&state)}))
        .into_response(),
    };
    with_common_headers(response)
}

/// `GET /api/ssh/keys`: needs a session; a must-change session may read the (necessarily
/// empty) list.
pub(super) async fn ssh_keys_list(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
) -> Response {
    if session.0.is_none() {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        ctx2.auth
            .snapshot_credentials(ctx2.host.as_ref(), &ctx2.paths)
    })
    .await;
    match result {
        Ok(Ok(credentials)) => {
            let mut keys = credentials.ssh_keys.clone();
            keys.sort_by(|a, b| a.added_at.cmp(&b.added_at).then(a.id.cmp(&b.id)));
            let items: Vec<Value> = keys.iter().map(|key| key.to_item()).collect();
            json_response(json!({"items": items}))
        }
        // An unreadable file cannot validate any session — the same answer
        // `check_session_at` gives.
        _ => ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SshKeyRequest {
    public_key: String,
    #[serde(default)]
    comment: String,
}

/// The whole critical section of the two key-changing endpoints: under the
/// `credentials_lock`, revalidate the request's own cookie against a fresh read, then
/// read-modify-write the credentials in one atomic publication.
fn ssh_key_write(ctx: &Arc<Ctx>, headers: &HeaderMap, change: KeyChange) -> Response {
    let _guard = ctx.auth.credentials_lock.blocking_lock();
    // The middleware check may have passed before this request queued on the lock; a
    // concurrent revoke rotated the `session_key` meanwhile, so a stale session must be
    // refused here rather than write or re-mint anything.
    if ctx
        .auth
        .check_session_at(ctx.host.as_ref(), &ctx.paths, headers)
        .is_err()
    {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    let Ok(mut credentials) = ctx.auth.snapshot_credentials(ctx.host.as_ref(), &ctx.paths) else {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    };
    match change {
        KeyChange::Add { line, comment } => {
            match crate::ssh_login::add_ssh_key(
                ctx.host.as_ref(),
                &mut credentials,
                &line,
                &comment,
            ) {
                Ok(item) => {
                    match auth::write_credentials(ctx.host.as_ref(), &ctx.paths, &credentials) {
                        Ok(()) => {
                            crate::log::info_json(
                                Event::SshKeyAdded,
                                &[
                                    ("fingerprint", item["fingerprint"].clone()),
                                    ("algorithm", item["algorithm"].clone()),
                                ],
                            );
                            json_response_status(StatusCode::CREATED, item)
                        }
                        Err(reason) => ssh_keys_unsaved(reason),
                    }
                }
                Err(crate::ssh_login::AddKeyError::Invalid(kind)) => {
                    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "ssh.key_invalid")
                        .params(json!({"reason": kind.reason()}))
                        .into_response()
                }
                Err(crate::ssh_login::AddKeyError::Exists(fingerprint)) => {
                    ApiError::new(StatusCode::CONFLICT, "ssh.key_exists")
                        .params(json!({"fingerprint": fingerprint}))
                        .into_response()
                }
                Err(crate::ssh_login::AddKeyError::Limit) => {
                    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "ssh.key_limit").into_response()
                }
            }
        }
        KeyChange::Remove { id } => {
            match crate::ssh_login::remove_ssh_key(&mut credentials, &id) {
                Ok(fingerprint) => {
                    // One atomic publication removes the key and rotates the
                    // `session_key`; only after it succeeded are the
                    // logged-out cookies cleared and a new cookie issued.
                    match auth::write_credentials(ctx.host.as_ref(), &ctx.paths, &credentials) {
                        Ok(()) => {
                            ctx.auth.clear_logged_out(ctx.host.as_ref(), &ctx.paths);
                            crate::log::info_json(
                                Event::SshKeyRemoved,
                                &[("fingerprint", json!(fingerprint))],
                            );
                            let issued =
                                auth::issue_session(&credentials.session_key, ctx.host.wall_ms());
                            let mut response = json_response(json!({"token": issued.token}));
                            if let Ok(value) =
                                HeaderValue::from_str(&auth::session_set_cookie(&issued.cookie))
                            {
                                response.headers_mut().insert(header::SET_COOKIE, value);
                            }
                            response
                        }
                        Err(reason) => ssh_keys_unsaved(reason),
                    }
                }
                Err(crate::ssh_login::RemoveKeyError::NotFound) => {
                    ApiError::new(StatusCode::NOT_FOUND, "ssh.key_not_found").into_response()
                }
            }
        }
    }
}

enum KeyChange {
    Add { line: String, comment: String },
    Remove { id: String },
}

/// credentials-save-failure notice-mode write failure of the key endpoints: the old file
/// bytes stay the value of record, so the response reports the failure as it is.
fn ssh_keys_unsaved(reason: auth::UnsavedReason) -> Response {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "ssh.keys_unsaved")
        .params(json!({"reason": reason.as_str()}))
        .into_response()
}

/// `POST /api/ssh/keys`. The must-change gate blocks every non-GET, non-`/api/password`
/// route, so registration can only happen after the initial password was changed.
pub(super) async fn ssh_keys_add(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<SshKeyRequest>,
) -> Response {
    if session.0.is_none() {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    if req.public_key.len() > SSH_KEY_LINE_MAX || req.comment.len() > SSH_COMMENT_MAX {
        return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "ssh.key_invalid")
            .params(json!({"reason": crate::ssh_login::KeyInvalid::BadFormat.reason()}))
            .into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        ssh_key_write(
            &ctx2,
            &headers,
            KeyChange::Add {
                line: req.public_key,
                comment: req.comment,
            },
        )
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

/// `DELETE /api/ssh/keys/{id}`: one atomic revoke plus `session_key` rotation; the response
/// carries the caller's new cookie.
pub(super) async fn ssh_key_delete(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if session.0.is_none() {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    }
    let ctx2 = ctx.clone();
    let result = tokio::task::spawn_blocking(move || {
        ssh_key_write(&ctx2, &headers, KeyChange::Remove { id })
    })
    .await;
    match result {
        Ok(response) => with_common_headers(response),
        Err(_) => internal().into_response(),
    }
}

pub(super) async fn logout(
    State((ctx, _)): State<AppState>,
    Extension(session): Extension<SessionOpt>,
) -> Response {
    let Some(session) = session.0 else {
        return ApiError::new(StatusCode::UNAUTHORIZED, "auth.required").into_response();
    };
    let saved = ctx
        .auth
        .logout(ctx.host.as_ref(), &ctx.paths, &session, ctx.host.wall_ms());
    crate::log::info(Event::Logout, &[]);
    let mut response = if saved.is_ok() {
        json_response(json!({}))
    } else {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "logout.unsaved").into_response()
    };
    if let Ok(value) = HeaderValue::from_str(&auth::clear_set_cookie()) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    with_common_headers(response)
}

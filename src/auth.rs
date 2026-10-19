//! Credentials, login, sessions and logout.
//!
//! The credentials file is read before every authenticated request, the session cookie is an
//! HMAC over its issue time, and a login quota bucket protects the login endpoint.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::http::HeaderMap;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::codes::*;
use crate::host::{Create, Host};
use crate::settings::{self, Settings};
use crate::store::{self, JsonRead, Paths, Validate};

type HmacSha256 = Hmac<Sha256>;

pub const COOKIE_NAME: &str = "nvm_session";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password_hash: String,
    pub session_key: String,
    /// Registered SSH sign-in keys, defaulting to an empty table.
    #[serde(default)]
    pub ssh_keys: Vec<crate::ssh_login::SshKey>,
}

impl Validate for Credentials {
    fn validate(&self) -> Result<(), String> {
        if !settings::check_username(&self.username) {
            return Err("bad username".into());
        }
        let hash = PasswordHash::new(&self.password_hash).map_err(|e| e.to_string())?;
        // the field must parse as an Argon2id PHC string.
        if hash.algorithm.as_str() != "argon2id" {
            return Err("bad password hash algorithm".into());
        }
        if hash.salt.is_none() || hash.hash.is_none() {
            return Err("incomplete password hash".into());
        }
        // `ARGON2_PARAMS`: the salt is 16 bytes.
        if hash.salt.as_ref().map(|salt| salt.len()) != Some(ARGON2_SALT_BYTES) {
            return Err("unapproved password hash salt length".into());
        }
        // the Argon2id parameters are fixed. The PHC string stores only `m`, `t`, `p`; the
        // hash length is the output length.
        let expected_params = format!("m={ARGON2_M_COST},t={ARGON2_T_COST},p={ARGON2_P_COST}");
        if hash.params.to_string() != expected_params {
            return Err("unapproved password hash parameters".into());
        }
        match &hash.hash {
            Some(output) if output.len() == ARGON2_OUTPUT_BYTES => {}
            _ => return Err("unapproved password hash length".into()),
        }
        if self.session_key.len() != 64
            || !self
                .session_key
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("bad session key".into());
        }
        // Validate the entire SSH key table before accepting the credentials.
        if self.ssh_keys.len() > SSH_MAX_KEYS {
            return Err("too many ssh keys".into());
        }
        for key in &self.ssh_keys {
            crate::ssh_login::validate_stored_key(key)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadableReason {
    NotJson,
    FieldInvalid,
    FileUnreadable,
    DirectoryUnreadable,
}

impl UnreadableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnreadableReason::NotJson => "not_json",
            UnreadableReason::FieldInvalid => "field_invalid",
            UnreadableReason::FileUnreadable => "file_unreadable",
            UnreadableReason::DirectoryUnreadable => "directory_unreadable",
        }
    }

    /// The reason key of this state, used by the `docker logs` `credentials_unreadable` line.
    /// The file that itself cannot be read has no reason writing of its own
    /// (credentials-unreadable notice `.file_unreadable`), so its key is the general
    /// read-failure phrase.
    fn log_reason_key(self) -> &'static str {
        match self {
            UnreadableReason::NotJson => "credentials.reason.not_json",
            UnreadableReason::FieldInvalid => "credentials.reason.field_invalid",
            UnreadableReason::FileUnreadable => REASON_IO_ERROR,
            UnreadableReason::DirectoryUnreadable => "credentials.reason.directory_unreadable",
        }
    }

    /// The `{key, params}` reason object of the `docker logs` line.
    pub fn log_reason_object(self, params: serde_json::Value) -> serde_json::Value {
        crate::log::reason_object(self.log_reason_key(), params, None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsavedReason {
    File,
    Directory,
}

impl UnsavedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnsavedReason::File => "file",
            UnsavedReason::Directory => "directory",
        }
    }

    /// The credentials-save-failure notice reason key of this state for the `docker logs`
    /// `credentials_unsaved` line.
    fn log_reason_key(self) -> &'static str {
        match self {
            UnsavedReason::File => "credentials.unsaved_reason.file",
            UnsavedReason::Directory => "credentials.unsaved_reason.directory",
        }
    }

    /// The `{key, params}` reason object of the `docker logs` line.
    pub fn log_reason_object(self, params: serde_json::Value) -> serde_json::Value {
        crate::log::reason_object(self.log_reason_key(), params, None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialState {
    Ok,
    SetupRequired,
    UsernameInvalid,
    Unreadable(UnreadableReason),
    Unsaved(UnsavedReason),
}

impl CredentialState {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            CredentialState::Ok => serde_json::json!("ok"),
            CredentialState::UsernameInvalid => serde_json::json!("username_invalid"),
            CredentialState::SetupRequired => serde_json::json!("setup_required"),
            CredentialState::Unreadable(r) => serde_json::json!({ "unreadable": r.as_str() }),
            CredentialState::Unsaved(r) => serde_json::json!({ "unsaved": r.as_str() }),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, CredentialState::Ok)
    }
}

/// The session established by a valid cookie and its request token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub mac: String,
    pub issued: u64,
    pub nonce: String,
}

/// A newly issued cookie and the request token returned with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedSession {
    pub cookie: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoggedOutFile {
    entries: Vec<LoggedOutEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoggedOutEntry {
    mac: String,
    expiry: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    Unauthorized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    Ok {
        cookie: String,
        token: String,
    },
    Failed {
        show_reset_help: bool,
        /// The `consecutive_failures` field of the `login_failed` log line.
        consecutive_failures: u32,
    },
    RateLimited {
        retry_after_s: u64,
        show_reset_help: bool,
    },
    Credentials(CredentialState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordChangeError {
    Rule(settings::PasswordRule),
    Credentials(CredentialState),
    Unsaved(UnsavedReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupError {
    NotAvailable,
    CodeInvalid { show_help: bool, code_renewed: bool },
    RateLimited { retry_after_s: u64 },
    Rule(settings::PasswordRule),
    Credentials(CredentialState),
    Unsaved(UnsavedReason),
}

/// A one-time plaintext handoff to the startup logger or a test caller.
pub struct IssuedSetupCode {
    pub code: String,
    pub reason: &'static str,
}

impl IssuedSetupCode {
    pub fn log(&self) {
        crate::log::info_json(Event::SetupCodeIssued, &[
            ("msg", serde_json::json!(format!("SETUP CODE: {}. Open the manager page and enter it to create the administrator account. Valid for 24 hours.", self.code))),
            ("reason", serde_json::json!(self.reason)),
            ("valid_hours", serde_json::json!(crate::setup_code::VALID_HOURS)),
        ]);
    }
}

/// Global login quota bucket.
#[derive(Debug, Clone)]
pub struct LoginLimiter {
    tokens: f64,
    last_refill: Instant,
    pub consecutive_failures: u32,
}

impl LoginLimiter {
    pub fn new(now: Instant) -> LoginLimiter {
        LoginLimiter {
            tokens: LOGIN_QUOTA as f64,
            last_refill: now,
            consecutive_failures: 0,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill);
        let gained = elapsed.as_secs_f64() / LOGIN_REFILL.as_secs_f64();
        if gained > 0.0 {
            self.tokens = (self.tokens + gained).min(LOGIN_QUOTA as f64);
            self.last_refill = now;
        }
    }

    pub fn retry_after_s(&self) -> u64 {
        if self.tokens >= 1.0 {
            return 0;
        }
        let needed = 1.0 - self.tokens;
        (needed * LOGIN_REFILL.as_secs_f64()).ceil() as u64
    }

    /// Whether an attempt may be made; does not consume quota.
    pub fn available(&mut self, now: Instant) -> bool {
        self.refill(now);
        self.tokens >= 1.0
    }

    /// Spend one attempt after a failure.
    pub fn consume(&mut self) {
        self.tokens = (self.tokens - 1.0).max(0.0);
    }

    /// The current token count (the fake-host control plane summary).
    pub fn available_tokens(&self) -> u32 {
        self.tokens.floor() as u32
    }

    #[cfg(test)]
    fn take(&mut self, now: Instant) -> bool {
        if self.available(now) {
            self.consume();
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Default)]
pub struct AuthState {
    pub credentials_lock: tokio::sync::Mutex<()>,
    /// Cookies signed out through logout: MAC -> expiry (issue time + lifetime).
    pub logged_out: Mutex<BTreeMap<String, u64>>,
    /// The login quota bucket. The lock is held across password verification so quota checks,
    /// verification and failure deduction are one critical section.
    pub limiter: tokio::sync::Mutex<Option<LoginLimiter>>,
    pub failure_count: std::sync::atomic::AtomicU32,
    /// The in-memory one-time SSH challenge table and its issue bucket. Lives here so a
    /// rebuilt `AuthState` starts with an empty table — challenges never survive a rebuild or
    /// restart.
    pub ssh_challenges: crate::ssh_login::ChallengeState,
    setup_code: Mutex<Option<crate::setup_code::SetupCode>>,
}

impl AuthState {
    pub fn new() -> AuthState {
        AuthState::default()
    }

    /// The caller holds credentials_lock. A live code is never replaced or re-logged.
    pub fn ensure_setup_code(
        &self,
        h: &dyn Host,
        p: &Paths,
        s: &Settings,
        reason: &str,
    ) -> Option<IssuedSetupCode> {
        let mut guard = self.setup_code.lock().unwrap_or_else(|e| e.into_inner());
        if self.load_state(h, p, s) != CredentialState::SetupRequired {
            *guard = None;
            return None;
        }
        let now = h.now();
        if guard.as_ref().is_some_and(|code| code.live(now)) {
            return None;
        }
        let reason = if guard.is_some() {
            "expired"
        } else if reason == "start" {
            "start"
        } else {
            "reset"
        };
        let (code, text) = crate::setup_code::SetupCode::issue(now);
        *guard = Some(code);
        Some(IssuedSetupCode { code: text, reason })
    }

    /// Used by the fake-host control plane to exercise lazy expiry.
    pub fn expire_setup_code(&self) {
        if let Some(code) = self
            .setup_code
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            code.expire();
        }
    }

    /// The handler holds credentials_lock across the state check, creation and consumption.
    pub async fn setup(
        &self,
        h: std::sync::Arc<dyn Host>,
        p: Paths,
        s: Settings,
        code: String,
        new_password: String,
    ) -> Result<IssuedSession, SetupError> {
        match self.load_state(&*h, &p, &s) {
            CredentialState::SetupRequired => {}
            CredentialState::Ok => return Err(SetupError::NotAvailable),
            state => return Err(SetupError::Credentials(state)),
        }
        let mut guard = self.limiter.lock().await;
        let issued = self.ensure_setup_code(&*h, &p, &s, "reset");
        let code_renewed = issued
            .as_ref()
            .is_some_and(|issued| issued.reason == "expired");
        if let Some(issued) = issued {
            issued.log();
        }
        let now = h.now();
        let limiter = guard.get_or_insert_with(|| LoginLimiter::new(now));
        if !limiter.available(now) {
            return Err(SetupError::RateLimited {
                retry_after_s: limiter.retry_after_s(),
            });
        }
        let valid = crate::setup_code::matches(
            self.setup_code
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref(),
            &code,
            now,
        );
        if !valid {
            limiter.consume();
            limiter.consecutive_failures += 1;
            let failures = limiter.consecutive_failures;
            self.failure_count
                .store(failures, std::sync::atomic::Ordering::SeqCst);
            crate::log::warn_json(
                Event::SetupCodeRejected,
                &[("failures", serde_json::json!(failures))],
            );
            return Err(SetupError::CodeInvalid {
                show_help: failures >= LOGIN_RESET_HELP_AFTER,
                code_renewed,
            });
        }
        settings::check_password(&new_password).map_err(SetupError::Rule)?;
        let password_hash = tokio::task::spawn_blocking(move || hash_password(&new_password))
            .await
            .map_err(|_| SetupError::Unsaved(UnsavedReason::File))?
            .map_err(|_| SetupError::Unsaved(UnsavedReason::File))?;
        let credentials = Credentials {
            username: s
                .username
                .ok_or(SetupError::Credentials(CredentialState::UsernameInvalid))?,
            password_hash,
            session_key: store::random_hex(32),
            ssh_keys: Vec::new(),
        };
        create_credentials(&*h, &p, &credentials, || {
            *self.setup_code.lock().unwrap_or_else(|e| e.into_inner()) = None;
        })?;
        limiter.consecutive_failures = 0;
        self.failure_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
        self.clear_logged_out(&*h, &p);
        crate::log::info_json(Event::CredentialsCreated, &[]);
        Ok(issue_session(&credentials.session_key, h.wall_ms()))
    }

    /// Read the credential state without creating an account.
    pub fn load_state(&self, h: &dyn Host, p: &Paths, s: &Settings) -> CredentialState {
        match store::read_json_recovering::<Credentials>(h, &p.credentials()) {
            JsonRead::Ok(_) => CredentialState::Ok,
            JsonRead::Missing => {
                if s.username.is_some() {
                    CredentialState::SetupRequired
                } else {
                    CredentialState::UsernameInvalid
                }
            }
            JsonRead::DirUnreadable(_) => {
                CredentialState::Unreadable(UnreadableReason::DirectoryUnreadable)
            }
            JsonRead::FileUnreadable(_) => {
                CredentialState::Unreadable(UnreadableReason::FileUnreadable)
            }
            JsonRead::Incomplete | JsonRead::NotJson => {
                CredentialState::Unreadable(UnreadableReason::NotJson)
            }
            JsonRead::Invalid => CredentialState::Unreadable(UnreadableReason::FieldInvalid),
        }
    }

    fn read_credentials(&self, h: &dyn Host, p: &Paths) -> Option<Credentials> {
        match store::read_json_recovering::<Credentials>(h, &p.credentials()) {
            JsonRead::Ok(c) => Some(c),
            _ => None,
        }
    }

    /// A read-only credential snapshot for SSH sign-in, preserving the exact failure state.
    pub fn snapshot_credentials(
        &self,
        h: &dyn Host,
        p: &Paths,
    ) -> Result<Credentials, CredentialState> {
        match store::read_json_recovering::<Credentials>(h, &p.credentials()) {
            JsonRead::Ok(c) => Ok(c),
            JsonRead::Missing => Err(CredentialState::SetupRequired),
            JsonRead::FileUnreadable(_) => Err(CredentialState::Unreadable(
                UnreadableReason::FileUnreadable,
            )),
            JsonRead::DirUnreadable(_) => Err(CredentialState::Unreadable(
                UnreadableReason::DirectoryUnreadable,
            )),
            JsonRead::Incomplete | JsonRead::NotJson => {
                Err(CredentialState::Unreadable(UnreadableReason::NotJson))
            }
            JsonRead::Invalid => Err(CredentialState::Unreadable(UnreadableReason::FieldInvalid)),
        }
    }

    /// The number of registered SSH keys, `0` when the file is unreadable.
    pub fn ssh_key_count(&self, h: &dyn Host, p: &Paths) -> usize {
        self.read_credentials(h, p)
            .map(|c| c.ssh_keys.len())
            .unwrap_or(0)
    }

    /// password login: quota check, verification and failure deduction under one lock. A
    /// successful sign-in never spends quota.
    pub async fn login(
        &self,
        h: std::sync::Arc<dyn Host>,
        p: Paths,
        s: Settings,
        user: String,
        pass: String,
    ) -> LoginOutcome {
        let state = self.load_state(&*h, &p, &s);
        if !state.is_ok() {
            return LoginOutcome::Credentials(state);
        }
        let credentials = match self.read_credentials(&*h, &p) {
            Some(c) => c,
            None => {
                return LoginOutcome::Credentials(CredentialState::Unreadable(
                    UnreadableReason::FileUnreadable,
                ))
            }
        };
        let mut guard = self.limiter.lock().await;
        let now = h.now();
        {
            let limiter = guard.get_or_insert_with(|| LoginLimiter::new(now));
            if !limiter.available(now) {
                let retry_after_s = limiter.retry_after_s();
                let show_reset_help = self.failure_count.load(std::sync::atomic::Ordering::SeqCst)
                    >= LOGIN_RESET_HELP_AFTER;
                drop(guard);
                return LoginOutcome::RateLimited {
                    retry_after_s,
                    show_reset_help,
                };
            }
        }
        let expected_user = credentials.username.clone();
        let hash = credentials.password_hash.clone();
        let user_matches = user == expected_user && settings::check_username(&user);
        let verified = tokio::task::spawn_blocking(move || {
            if user_matches {
                verify_password(&hash, &pass)
            } else {
                dummy_verify(&pass);
                false
            }
        })
        .await
        .unwrap_or(false);
        if !verified {
            let failures = {
                let limiter = guard.as_mut().expect("login limiter");
                limiter.consume();
                limiter.consecutive_failures += 1;
                limiter.consecutive_failures
            };
            self.failure_count
                .store(failures, std::sync::atomic::Ordering::SeqCst);
            drop(guard);
            return LoginOutcome::Failed {
                show_reset_help: failures >= LOGIN_RESET_HELP_AFTER,
                consecutive_failures: failures,
            };
        }
        {
            let limiter = guard.as_mut().expect("login limiter");
            limiter.consecutive_failures = 0;
        }
        self.failure_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
        drop(guard);
        let issued = issue_session(&credentials.session_key, h.wall_ms());
        LoginOutcome::Ok {
            cookie: issued.cookie,
            token: issued.token,
        }
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.failure_count.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Every authenticated request re-reads the credentials file and checks the three cookie
    /// conditions.
    pub fn check_session_at(
        &self,
        h: &dyn Host,
        p: &Paths,
        headers: &HeaderMap,
    ) -> Result<Session, SessionError> {
        let cookie = headers
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .ok_or(SessionError::Unauthorized)?;
        let raw = cookie_value(cookie, COOKIE_NAME).ok_or(SessionError::Unauthorized)?;
        let Some((issued, nonce, mac)) = split_cookie(&raw) else {
            return Err(SessionError::Unauthorized);
        };
        let Some(credentials) = self.read_credentials(h, p) else {
            return Err(SessionError::Unauthorized);
        };
        if !verify_cookie(&credentials.session_key, issued, nonce, mac) {
            return Err(SessionError::Unauthorized);
        }
        let now = h.wall_ms();
        if now.saturating_sub(issued) >= SESSION_LIFETIME.as_millis() as u64 {
            return Err(SessionError::Unauthorized);
        }
        {
            let mut guard = self.logged_out.lock().unwrap_or_else(|e| e.into_inner());
            guard.retain(|_, expiry| *expiry > now);
            if guard.contains_key(mac) {
                return Err(SessionError::Unauthorized);
            }
        }
        let presented = headers.get("x-nvm-token").and_then(|v| v.to_str().ok());
        let expected = request_token(&credentials.session_key, issued, nonce);
        if !token_matches(&expected, presented) {
            return Err(SessionError::Unauthorized);
        }
        Ok(Session {
            mac: mac.to_string(),
            issued,
            nonce: nonce.to_string(),
        })
    }

    /// password change: change the password atomically. The caller holds the credentials
    /// lock; the password change handler takes it.
    pub fn change_password(
        &self,
        h: &dyn Host,
        p: &Paths,
        new_password: &str,
    ) -> Result<IssuedSession, PasswordChangeError> {
        settings::check_password(new_password).map_err(PasswordChangeError::Rule)?;
        let Some(mut credentials) = self.read_credentials(h, p) else {
            return Err(PasswordChangeError::Credentials(
                CredentialState::Unreadable(UnreadableReason::FileUnreadable),
            ));
        };
        let hash = hash_password(new_password)
            .map_err(|_| PasswordChangeError::Unsaved(UnsavedReason::File))?;
        credentials.password_hash = hash;
        credentials.session_key = store::random_hex(32);
        match write_credentials(h, p, &credentials) {
            Ok(()) => {
                // The new session key already rejects every previous mac. A
                // failure to clear the file does not undo the password change.
                self.clear_logged_out(h, p);
                Ok(issue_session(&credentials.session_key, h.wall_ms()))
            }
            Err(reason) => Err(PasswordChangeError::Unsaved(reason)),
        }
    }

    /// Remember this cookie so later requests with it are 401.
    ///
    /// The in-memory revocation is applied before the file write. A write
    /// failure leaves that revocation in place and reports the error.
    /// The error carries no extra detail: the caller answers `logout.unsaved`.
    #[allow(clippy::result_unit_err)]
    pub fn logout(
        &self,
        h: &dyn Host,
        p: &Paths,
        session: &Session,
        now_ms: u64,
    ) -> Result<(), ()> {
        let expiry = session.issued + SESSION_LIFETIME.as_millis() as u64;
        let mut guard = self.logged_out.lock().unwrap_or_else(|e| e.into_inner());
        guard.retain(|_, e| *e > now_ms);
        guard.insert(session.mac.clone(), expiry);
        write_logged_out(h, p, &guard)
    }

    /// Replace the in-memory list from the revocation file.
    ///
    /// A missing or unreadable file leaves the list empty. Expired entries
    /// are dropped. This is for process start; a fresh `AuthState` does not
    /// load the file by itself.
    pub fn load_logged_out(&self, h: &dyn Host, p: &Paths) {
        let now = h.wall_ms();
        let loaded = match h.read(&p.logged_out_file()) {
            Ok(bytes) => parse_logged_out(&bytes, now),
            Err(_) => BTreeMap::new(),
        };
        let mut guard = self.logged_out.lock().unwrap_or_else(|e| e.into_inner());
        *guard = loaded;
    }

    /// Drop every remembered mac and replace the file with an empty list.
    ///
    /// A file error is ignored: callers that rotated `session_key` already
    /// made the old macs fail verification.
    pub fn clear_logged_out(&self, h: &dyn Host, p: &Paths) {
        let mut guard = self.logged_out.lock().unwrap_or_else(|e| e.into_inner());
        guard.clear();
        let _ = write_logged_out(h, p, &guard);
    }

    /// The number of remembered logged-out cookies (the fake-host control
    /// plane summary).
    pub fn logged_out_len(&self) -> usize {
        self.logged_out
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

/// Publish complete initial credentials exclusively, without an empty reservation.
fn create_credentials(
    h: &dyn Host,
    p: &Paths,
    credentials: &Credentials,
    on_published: impl FnOnce(),
) -> Result<(), SetupError> {
    use std::io::Write;

    let path = p.credentials();
    let dir = path
        .parent()
        .ok_or(SetupError::Unsaved(UnsavedReason::Directory))?;
    h.create_dir(dir, 0o700)
        .map_err(|_| SetupError::Unsaved(UnsavedReason::Directory))?;
    let bytes = serde_json::to_vec_pretty(credentials)
        .map_err(|_| SetupError::Unsaved(UnsavedReason::File))?;
    // This uses the existing startup-cleanup namespace. A crash before the link leaves
    // only a temporary file; a crash afterwards leaves complete credentials at the target.
    let tmp = dir.join(format!(
        ".web-credentials.json.tmp-{}",
        store::random_hex(8)
    ));
    let mut published = false;
    let result = (|| {
        let mut file = h
            .create(&tmp, Create::New, 0o600)
            .map_err(|_| SetupError::Unsaved(UnsavedReason::File))?;
        file.write_all(&bytes)
            .map_err(|_| SetupError::Unsaved(UnsavedReason::File))?;
        file.sync_all()
            .map_err(|_| SetupError::Unsaved(UnsavedReason::File))?;
        drop(file);
        // hard_link atomically creates the final name only if absent. Never replace
        // another successful creator, even if it belongs to a different process.
        h.hard_link(&tmp, &path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                SetupError::NotAvailable
            } else {
                SetupError::Unsaved(UnsavedReason::File)
            }
        })?;
        published = true;
        // The one-time code is spent as soon as the account exists, even if the
        // following durability or cleanup steps fail and no session is returned.
        on_published();
        // Publication is irreversible here: a later error must not remove a complete
        // account another reader may already have observed. No session is returned on
        // failure; startup cleanup removes any retained temporary hard link.
        h.fsync_dir(dir)
            .map_err(|_| SetupError::Unsaved(UnsavedReason::Directory))?;
        h.remove_file(&tmp)
            .map_err(|_| SetupError::Unsaved(UnsavedReason::Directory))?;
        h.fsync_dir(dir)
            .map_err(|_| SetupError::Unsaved(UnsavedReason::Directory))?;
        Ok(())
    })();
    if result.is_err() && !published {
        let _ = h.remove_file(&tmp);
    }
    result
}

/// Atomically replace credentials, retaining protected recovery material on failed rollback.
pub fn write_credentials(
    h: &dyn Host,
    p: &Paths,
    credentials: &Credentials,
) -> Result<(), UnsavedReason> {
    let path = p.credentials();
    let dir = path.parent().ok_or(UnsavedReason::Directory)?;
    let _ = h.create_dir(dir, 0o700);
    // Put the recovery material of an earlier failed publish back before the previous
    // credentials are snapshotted, so a refused rollback is never superseded by the next
    // write.
    store::flush_recovery(h, dir, "web-credentials.json").map_err(|_| UnsavedReason::Directory)?;
    let before = match h.read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // The file exists but cannot be read: publishing would destroy the
        // only old copy that could be restored, so stop here.
        Err(_) => return Err(UnsavedReason::File),
    };
    let bytes = serde_json::to_vec_pretty(credentials).map_err(|_| UnsavedReason::File)?;
    let tmp = dir.join(format!(
        ".web-credentials.json.tmp-{}",
        store::random_hex(8)
    ));
    let result = write_credentials_at(h, dir, &tmp, &path, &bytes, before.as_deref());
    if result.is_err() {
        let _ = h.remove_file(&tmp);
    }
    result
}

/// The contract is that a failed save leaves the old credentials in place. The publish rolls
/// the old file back on a directory-sync failure and reads it back to confirm; only a
/// confirmed rollback is reported as `Unsaved`. If the old file could not be restored, the
/// recovery material is kept and the outcome is reported as applied only when the target
/// really holds the new credentials.
fn write_credentials_at(
    h: &dyn Host,
    dir: &std::path::Path,
    tmp: &std::path::Path,
    path: &std::path::Path,
    bytes: &[u8],
    before: Option<&[u8]>,
) -> Result<(), UnsavedReason> {
    use std::io::Write;
    let mut f = h
        .create(tmp, Create::New, 0o600)
        .map_err(|_| UnsavedReason::Directory)?;
    f.write_all(bytes).map_err(|_| UnsavedReason::File)?;
    f.sync_all().map_err(|_| UnsavedReason::File)?;
    drop(f);
    match store::publish_file(h, dir, path, tmp, "web-credentials.json", before) {
        Ok(()) => Ok(()),
        Err(r) => {
            // a publish whose directory sync
            // failed is a failure even when the new bytes ended up visible, so this never
            // turns into `Ok`. The log says which rollback outcome was read back: only
            // `verified` means the old bytes are back.
            let restore = if r.restored {
                "verified"
            } else if r.recovery.is_some() {
                "recovery_kept"
            } else {
                "unconfirmed"
            };
            crate::log::error_json(
                Event::CredentialsUnsaved,
                &[
                    (
                        "reason",
                        UnsavedReason::Directory.log_reason_object(serde_json::json!({})),
                    ),
                    ("restore", serde_json::json!(restore)),
                ],
            );
            Err(UnsavedReason::Directory)
        }
    }
}

fn params() -> Params {
    Params::new(
        ARGON2_M_COST,
        ARGON2_T_COST,
        ARGON2_P_COST,
        Some(ARGON2_OUTPUT_BYTES),
    )
    .expect("argon2 params")
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt_bytes = {
        let mut buf = [0u8; ARGON2_SALT_BYTES];
        getrandom::fill(&mut buf).map_err(|e| e.to_string())?;
        buf
    };
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params());
    argon2
        .hash_password_with_salt(password.as_bytes(), &salt_bytes)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(phc: &str, password: &str) -> bool {
    let Ok(hash) = PasswordHash::new(phc) else {
        return false;
    };
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params());
    argon2.verify_password(password.as_bytes(), &hash).is_ok()
}

fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password("nvm-placeholder-password").unwrap_or_default())
}

/// Same-cost verification for a non-matching username.
pub fn dummy_verify(password: &str) {
    let _ = verify_password(dummy_hash(), password);
}

/// `nvm_session=<issued>.<nonce>.<mac>`. The nonce is 8 random bytes.
pub fn issue_session(session_key: &str, issued: u64) -> IssuedSession {
    let nonce = store::random_hex(8);
    IssuedSession {
        cookie: sign_cookie(session_key, issued, &nonce),
        token: request_token(session_key, issued, &nonce),
    }
}

pub fn sign_cookie(session_key: &str, issued: u64, nonce: &str) -> String {
    let mac = cookie_mac(session_key, issued, nonce);
    format!("{issued}.{nonce}.{mac}")
}

pub fn cookie_mac(session_key: &str, issued: u64, nonce: &str) -> String {
    let key = store::unhex(session_key).unwrap_or_default();
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(&key).expect("hmac key");
    mac.update(b"nvm-session\n");
    mac.update(issued.to_string().as_bytes());
    mac.update(b"\n");
    mac.update(nonce.as_bytes());
    store::hex(&mac.finalize().into_bytes())
}

/// Request token: `HMAC(session_key, "nvm-token\n" + issued + nonce)`.
/// The browser does not send this value by itself.
pub fn request_token(session_key: &str, issued: u64, nonce: &str) -> String {
    let key = store::unhex(session_key).unwrap_or_default();
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(&key).expect("hmac key");
    mac.update(b"nvm-token\n");
    mac.update(issued.to_string().as_bytes());
    mac.update(nonce.as_bytes());
    store::hex(&mac.finalize().into_bytes())
}

pub fn verify_cookie(session_key: &str, issued: u64, nonce: &str, mac_hex: &str) -> bool {
    token_matches(&cookie_mac(session_key, issued, nonce), Some(mac_hex))
}

/// Compare a lowercase hex MAC. A missing or malformed value still walks the
/// full digest so the two outcomes take the same comparison path.
fn token_matches(expected_hex: &str, presented: Option<&str>) -> bool {
    let expected = store::unhex(expected_hex).unwrap_or_else(|| vec![0u8; 32]);
    let mut given = [0u8; 32];
    let mut diff = if expected.len() == 32 { 0u8 } else { 1 };
    let parsed = presented.and_then(|text| {
        if text.len() == 64 && is_lower_hex(text) {
            store::unhex(text)
        } else {
            None
        }
    });
    match parsed {
        Some(bytes) if bytes.len() == 32 => given.copy_from_slice(&bytes),
        _ => diff |= 1,
    }
    if expected.len() == 32 {
        for (a, b) in expected.iter().zip(given.iter()) {
            diff |= a ^ b;
        }
    }
    diff == 0
}

fn is_lower_hex(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// `issued.nonce.mac`, with an 8-byte lowercase nonce and a 32-byte mac.
fn split_cookie(raw: &str) -> Option<(u64, &str, &str)> {
    let mut parts = raw.split('.');
    let issued_text = parts.next()?;
    let nonce = parts.next()?;
    let mac = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if !issued_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let issued = issued_text.parse::<u64>().ok()?;
    if nonce.len() != 16 || !is_lower_hex(nonce) || mac.len() != 64 || !is_lower_hex(mac) {
        return None;
    }
    Some((issued, nonce, mac))
}

fn parse_logged_out(bytes: &[u8], now: u64) -> BTreeMap<String, u64> {
    let Ok(file) = serde_json::from_slice::<LoggedOutFile>(bytes) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for entry in file.entries {
        if entry.expiry > now && entry.mac.len() == 64 && is_lower_hex(&entry.mac) {
            out.insert(entry.mac, entry.expiry);
        }
    }
    out
}

fn write_logged_out(h: &dyn Host, p: &Paths, entries: &BTreeMap<String, u64>) -> Result<(), ()> {
    use std::io::Write;
    let path = p.logged_out_file();
    let dir = path.parent().ok_or(())?;
    let _ = h.create_dir(dir, 0o700);
    let file = LoggedOutFile {
        entries: entries
            .iter()
            .map(|(mac, expiry)| LoggedOutEntry {
                mac: mac.clone(),
                expiry: *expiry,
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&file).map_err(|_| ())?;
    let tmp = dir.join(format!(".logged-out.json.tmp-{}", store::random_hex(8)));
    let result = (|| -> Result<(), ()> {
        let mut f = h.create(&tmp, Create::New, 0o600).map_err(|_| ())?;
        f.write_all(&bytes).map_err(|_| ())?;
        f.sync_all().map_err(|_| ())?;
        drop(f);
        h.rename(&tmp, &path).map_err(|_| ())?;
        h.fsync_dir(dir).map_err(|_| ())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = h.remove_file(&tmp);
    }
    result
}

/// Extract one cookie value from a `Cookie` header.
pub fn cookie_value(header: &str, name: &str) -> Option<String> {
    for part in header.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(name) {
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// `Set-Cookie` value for a session.
pub fn session_set_cookie(value: &str) -> String {
    format!("{COOKIE_NAME}={value}; Path=/; Max-Age={SESSION_MAX_AGE}; HttpOnly; SameSite=Strict")
}

pub fn clear_set_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RealHost;
    use crate::settings::SettingProblem;
    use crate::test_support::TestDir;

    fn settings_with() -> Settings {
        Settings {
            username: Some("admin".into()),
            port: 8199,
            build_timeout_minutes: None,
            problems: Vec::<SettingProblem>::new(),
        }
    }

    fn setup(name: &str) -> (TestDir, Paths) {
        let tmp = TestDir::new(&format!("auth-{name}"));
        let p = Paths::new(&*tmp);
        store::prepare_dirs(&RealHost::on_host(), &p).unwrap();
        (tmp, p)
    }

    fn login_as<'a>(
        state: &'a AuthState,
        host: &std::sync::Arc<dyn Host>,
        paths: &Paths,
        settings: &Settings,
        password: &str,
    ) -> impl std::future::Future<Output = LoginOutcome> + 'a {
        state.login(
            host.clone(),
            paths.clone(),
            settings.clone(),
            "admin".into(),
            password.into(),
        )
    }

    fn create_test_account(state: &AuthState, p: &Paths, s: &Settings) {
        let credentials = Credentials {
            username: s.username.clone().unwrap(),
            password_hash: hash_password("hunter12").unwrap(),
            session_key: store::random_hex(32),
            ssh_keys: Vec::new(),
        };
        create_credentials(&RealHost::on_host(), p, &credentials, || {}).unwrap();
        assert!(state.load_state(&RealHost::on_host(), p, s).is_ok());
    }

    #[test]
    fn load_state_is_read_only_and_validates_username() {
        let (_tmp, p) = setup("states");
        let state = AuthState::new();
        let mut s = settings_with();
        assert_eq!(
            state.load_state(&RealHost::on_host(), &p, &s),
            CredentialState::SetupRequired
        );
        assert!(!p.credentials().exists());
        s.username = None;
        assert_eq!(
            state.load_state(&RealHost::on_host(), &p, &s),
            CredentialState::UsernameInvalid
        );
        assert!(state
            .ensure_setup_code(&RealHost::on_host(), &p, &s, "start")
            .is_none());
        assert!(!p.credentials().exists());
    }

    #[test]
    fn unreadable_credentials_keep_the_file() {
        let (_tmp, p) = setup("unreadable");
        let state = AuthState::new();
        std::fs::create_dir_all(p.manager().join("secrets")).unwrap();
        std::fs::write(p.credentials(), b"not json").unwrap();
        let before = std::fs::read(p.credentials()).unwrap();
        let s = settings_with();
        assert_eq!(
            state.load_state(&RealHost::on_host(), &p, &s),
            CredentialState::Unreadable(UnreadableReason::NotJson)
        );
        assert_eq!(std::fs::read(p.credentials()).unwrap(), before);
    }

    #[tokio::test]
    async fn six_concurrent_correct_logins_all_succeed() {
        let (_tmp, p) = setup("concurrent");
        let state = AuthState::new();
        let s = settings_with();
        create_test_account(&state, &p, &s);
        let h: std::sync::Arc<dyn Host> = std::sync::Arc::new(RealHost::on_host());
        let (ra, rb, rc, rd, re, rf) = tokio::join!(
            login_as(&state, &h, &p, &s, "hunter12"),
            login_as(&state, &h, &p, &s, "hunter12"),
            login_as(&state, &h, &p, &s, "hunter12"),
            login_as(&state, &h, &p, &s, "hunter12"),
            login_as(&state, &h, &p, &s, "hunter12"),
            login_as(&state, &h, &p, &s, "hunter12"),
        );
        for outcome in [ra, rb, rc, rd, re, rf] {
            assert!(
                matches!(outcome, LoginOutcome::Ok { .. }),
                "a correct sign-in was refused: {outcome:?}"
            );
        }
    }

    #[test]
    fn an_uppercase_mac_is_not_a_valid_session() {
        let (_tmp, p) = setup("upper-mac");
        let state = AuthState::new();
        let s = settings_with();
        create_test_account(&state, &p, &s);
        let cred: Credentials =
            serde_json::from_slice(&std::fs::read(p.credentials()).unwrap()).unwrap();
        let issued_ms = crate::host::Host::wall_ms(&RealHost::on_host());
        let session = issue_session(&cred.session_key, issued_ms);
        let mut parts = session.cookie.split('.');
        let issued = parts.next().unwrap();
        let nonce = parts.next().unwrap();
        let mac = parts.next().unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "cookie",
            format!("nvm_session={issued}.{nonce}.{mac}")
                .parse()
                .unwrap(),
        );
        headers.insert("x-nvm-token", session.token.parse().unwrap());
        assert!(state
            .check_session_at(&RealHost::on_host(), &p, &headers)
            .is_ok());
        headers.insert(
            "cookie",
            format!("nvm_session={issued}.{nonce}.{}", mac.to_uppercase())
                .parse()
                .unwrap(),
        );
        assert!(state
            .check_session_at(&RealHost::on_host(), &p, &headers)
            .is_err());
    }

    #[test]
    fn only_argon2id_phc_is_accepted() {
        let hash = hash_password("hunter12").unwrap();
        assert!(hash.contains("$argon2id$"));
        let mut cred = Credentials {
            username: "admin".into(),
            password_hash: hash.clone(),
            session_key: "a".repeat(64),
            ssh_keys: Vec::new(),
        };
        assert!(cred.validate().is_ok());
        cred.password_hash = hash.replacen("$argon2id$", "$argon2i$", 1);
        assert!(cred.validate().is_err());
        cred.password_hash = hash.replacen("$argon2id$", "$argon2d$", 1);
        assert!(cred.validate().is_err());
        // The parameters are fixed.
        cred.password_hash = hash.replacen("m=19456", "m=8", 1);
        assert!(cred.validate().is_err());
        cred.password_hash = hash.replacen("t=2", "t=9", 1);
        assert!(cred.validate().is_err());
        cred.password_hash = hash.replacen("p=1", "p=4", 1);
        assert!(cred.validate().is_err());
    }

    #[test]
    fn cookie_sign_and_verify() {
        let key = store::random_hex(32);
        let nonce = store::random_hex(8);
        let value = sign_cookie(&key, 1234, &nonce);
        let mut parts = value.split('.');
        let issued = parts.next().unwrap();
        let nonce_part = parts.next().unwrap();
        let mac = parts.next().unwrap();
        assert!(parts.next().is_none());
        assert_eq!(issued, "1234");
        assert_eq!(nonce_part, nonce);
        assert!(verify_cookie(&key, 1234, &nonce, mac));
        assert!(!verify_cookie(&key, 1235, &nonce, mac));
        let other = store::random_hex(32);
        assert!(!verify_cookie(&other, 1234, &nonce, mac));
        assert!(!verify_cookie(&key, 1234, &nonce, "00"));
        let again = issue_session(&key, 1234);
        assert_ne!(value, again.cookie);
        assert_ne!(request_token(&key, 1234, &nonce), again.token);
    }

    #[test]
    fn cookie_values_and_headers() {
        assert_eq!(
            cookie_value("a=1; nvm_session=5.ab; b=2", COOKIE_NAME).as_deref(),
            Some("5.ab")
        );
        assert!(cookie_value("x=1", COOKIE_NAME).is_none());
        let set = session_set_cookie("5.ab");
        assert!(set.contains("Path=/"));
        assert!(set.contains("Max-Age=43200"));
        assert!(set.contains("HttpOnly"));
        assert!(set.contains("SameSite=Strict"));
        assert!(!set.contains("Secure"));
        assert!(clear_set_cookie().contains("Max-Age=0"));
    }

    #[test]
    fn login_limiter_refills() {
        let now = Instant::now();
        let mut l = LoginLimiter::new(now);
        for _ in 0..LOGIN_QUOTA {
            assert!(l.available(now));
            l.consume();
        }
        assert!(!l.available(now));
        assert!(l.retry_after_s() <= LOGIN_REFILL.as_secs());
        assert!(l.take(now + LOGIN_REFILL));
        assert!(!l.take(now + LOGIN_REFILL));
        // A long wait caps at the quota.
        let later = now + LOGIN_REFILL * 100;
        for _ in 0..LOGIN_QUOTA {
            assert!(l.take(later));
        }
        assert!(!l.take(later));
    }

    #[tokio::test]
    async fn login_flow_verifies_and_limits() {
        let (_tmp, p) = setup("login");
        let state = AuthState::new();
        let s = settings_with();
        create_test_account(&state, &p, &s);
        let h: std::sync::Arc<dyn Host> = std::sync::Arc::new(RealHost::on_host());
        let out = login_as(&state, &h, &p, &s, "hunter12").await;
        assert!(matches!(out, LoginOutcome::Ok { .. }));
        let out = login_as(&state, &h, &p, &s, "wrong").await;
        assert!(matches!(
            out,
            LoginOutcome::Failed {
                show_reset_help: false,
                consecutive_failures: 1,
            }
        ));
        let out = login_as(&state, &h, &p, &s, "wrong").await;
        assert!(matches!(
            out,
            LoginOutcome::Failed {
                show_reset_help: false,
                consecutive_failures: 2,
            }
        ));
        let out = login_as(&state, &h, &p, &s, "wrong").await;
        assert!(matches!(
            out,
            LoginOutcome::Failed {
                show_reset_help: true,
                consecutive_failures: 3,
            }
        ));
        // Two more failures exhaust the bucket.
        let _ = login_as(&state, &h, &p, &s, "wrong").await;
        let _ = login_as(&state, &h, &p, &s, "wrong").await;
        let out = login_as(&state, &h, &p, &s, "hunter12").await;
        assert!(matches!(out, LoginOutcome::RateLimited { .. }));
    }

    #[test]
    fn change_password_swaps_the_key() {
        let (_tmp, p) = setup("change");
        let state = AuthState::new();
        let s = settings_with();
        create_test_account(&state, &p, &s);
        let issued_session = state
            .change_password(&RealHost::on_host(), &p, "newpass99")
            .unwrap();
        let cred: Credentials =
            serde_json::from_slice(&std::fs::read(p.credentials()).unwrap()).unwrap();
        let mut parts = issued_session.cookie.split('.');
        let issued = parts.next().unwrap();
        let nonce = parts.next().unwrap();
        let mac = parts.next().unwrap();
        assert!(verify_cookie(
            &cred.session_key,
            issued.parse().unwrap(),
            nonce,
            mac
        ));
        assert_eq!(
            issued_session.token,
            request_token(&cred.session_key, issued.parse().unwrap(), nonce)
        );
        assert!(verify_password(&cred.password_hash, "newpass99"));
        assert!(state
            .change_password(&RealHost::on_host(), &p, "newpass99")
            .is_ok());
        assert_eq!(
            state.change_password(&RealHost::on_host(), &p, "short"),
            Err(PasswordChangeError::Rule(settings::PasswordRule::TooShort))
        );
    }
}

//! SSHSIG challenge-response sign-in.
//!
//! This module owns the in-memory one-time challenge table, the SSHSIG
//! parsing/verification/fingerprint helpers, the stored [`SshKey`] shape and the `ssh_login`
//! grant method. It never reads or writes files itself: every persistence goes through the
//! existing `auth.rs` machinery (`read_credentials` snapshots, `write_credentials` atomic
//! publication).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use ssh_key::public::KeyData;
use ssh_key::{HashAlg, PublicKey, SshSig};

use crate::auth::{AuthState, CredentialState, Credentials};
use crate::codes::*;
use crate::host::Host;
use crate::store::{self, Paths};

/// One registered public key of the single administrator account. Serialized into the
/// existing `web-credentials.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SshKey {
    /// 16 lowercase hex digits (`store::random_hex(8)` at registration).
    pub id: String,
    /// Canonical `<algorithm> <base64>` line; the comment of the parsed key
    /// was stripped with `set_comment("")` before this was stored.
    pub public_key: String,
    /// `SHA256:<base64>` as recomputed by the server; client-supplied
    /// fingerprints are never stored.
    pub fingerprint: String,
    /// Free-form display note, rendered as plain text only.
    pub comment: String,
    /// `wall_ms` of the registration moment; the list is sorted by it.
    pub added_at: u64,
}

impl SshKey {
    /// The item shape of `GET /api/ssh/keys`.
    pub fn to_item(&self) -> serde_json::Value {
        let algorithm = parse_public_key(&self.public_key)
            .map(|(key, _)| key.algorithm().as_str().to_string())
            .unwrap_or_default();
        serde_json::json!({
            "id": self.id,
            "algorithm": algorithm,
            "fingerprint": self.fingerprint,
            "comment": self.comment,
            "public_key": self.public_key,
            "added_at": self.added_at,
        })
    }
}

/// Why a public key line was refused at registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInvalid {
    BadFormat,
    UnsupportedType(String),
    WeakKey,
}

impl KeyInvalid {
    /// The `ssh.key_invalid` reason object of the response.
    pub fn reason(&self) -> serde_json::Value {
        match self {
            KeyInvalid::BadFormat => {
                serde_json::json!({"key": "ssh.key_invalid.bad_format", "params": {}})
            }
            KeyInvalid::UnsupportedType(algorithm) => serde_json::json!({
                "key": "ssh.key_invalid.unsupported_type",
                "params": {"algorithm": algorithm},
            }),
            KeyInvalid::WeakKey => {
                serde_json::json!({"key": "ssh.key_invalid.weak_key", "params": {}})
            }
        }
    }
}

/// True for the three accepted algorithms: ed25519 and ecdsa-p256/p384. RSA is excluded
/// because its verification backend is the unpatched `rsa` crate (RUSTSEC-2023-0071), sk-*
/// requires a hardware-backed verifier and is outside the accepted algorithm set.
pub fn algorithm_allowed(algorithm: ssh_key::Algorithm) -> bool {
    matches!(
        algorithm,
        ssh_key::Algorithm::Ed25519
            | ssh_key::Algorithm::Ecdsa {
                curve: ssh_key::EcdsaCurve::NistP256,
            }
            | ssh_key::Algorithm::Ecdsa {
                curve: ssh_key::EcdsaCurve::NistP384,
            }
    )
}

/// The weak/low-order Ed25519 gate: `ssh-key` 0.6.7 accepts small-order encodings at parse
/// time and its verifier is not strict, so an identity-point public key allows keyless
/// forgeries. The `ed25519-dalek` `is_weak` check is the library's own judgement; no
/// hand-rolled crypto here.
fn ed25519_is_weak(key_data: &KeyData) -> bool {
    match key_data {
        KeyData::Ed25519(public) => ed25519_dalek::VerifyingKey::from_bytes(public.as_ref())
            .map(|key| key.is_weak())
            .unwrap_or(true),
        _ => false,
    }
}

/// The `SHA256:<base64>` fingerprint of a parsed key, always recomputed by the server.
pub fn fingerprint_of(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// The canonical comment-free single-line form that gets stored (`set_comment("")` then
/// `to_openssh`).
pub fn canonical_line(key: &PublicKey) -> String {
    let mut copy = key.clone();
    copy.set_comment("");
    copy.to_openssh().unwrap_or_default().trim_end().to_string()
}

/// Parse one OpenSSH public key line and run the full registration gate: parse, algorithm
/// whitelist, weak-key refusal. The returned key still carries the submitted comment; use
/// [`canonical_line`] for the stored form and [`fingerprint_of`] for the fingerprint.
pub fn parse_public_key(line: &str) -> Result<(PublicKey, String), KeyInvalid> {
    let key = PublicKey::from_openssh(line).map_err(|_| KeyInvalid::BadFormat)?;
    if !algorithm_allowed(key.algorithm()) {
        return Err(KeyInvalid::UnsupportedType(
            key.algorithm().as_str().to_string(),
        ));
    }
    if ed25519_is_weak(key.key_data()) {
        return Err(KeyInvalid::WeakKey);
    }
    let fingerprint = fingerprint_of(&key);
    Ok((key, fingerprint))
}

/// Validate one stored [`SshKey`] of a credentials file: shape limits, parseability,
/// whitelist, weak-key refusal and fingerprint consistency. Called on every read; a violation
/// makes the whole file `FieldInvalid` (the existing credentials-unreadable notice
/// fail-closed semantics).
pub fn validate_stored_key(key: &SshKey) -> Result<(), String> {
    if key.id.len() != 16
        || !key
            .id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("bad ssh key id".into());
    }
    if !fingerprint_shape_ok(&key.fingerprint) {
        return Err("bad ssh key fingerprint".into());
    }
    if key.public_key.len() > SSH_KEY_LINE_MAX {
        return Err("ssh key line too long".into());
    }
    if key.comment.len() > SSH_COMMENT_MAX {
        return Err("ssh key comment too long".into());
    }
    let (parsed, fingerprint) =
        parse_public_key(&key.public_key).map_err(|_| "bad ssh key line".to_string())?;
    // The stored line must already be the canonical comment-free form.
    if canonical_line(&parsed) != key.public_key {
        return Err("ssh key line not canonical".into());
    }
    if fingerprint != key.fingerprint {
        return Err("ssh key fingerprint mismatch".into());
    }
    Ok(())
}

/// The OpenSSH SHA256 fingerprint spelling: a `SHA256:` prefix and an
/// unpadded standard-base64 body.
fn fingerprint_shape_ok(fingerprint: &str) -> bool {
    match fingerprint.strip_prefix("SHA256:") {
        Some(body) => {
            !body.is_empty()
                && body
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
        }
        None => false,
    }
}

/// One issued challenge: 32 random bytes as 64 lowercase hex ASCII characters and the
/// wall-clock expiry of the 120-second window.
#[derive(Debug, Clone)]
pub struct IssuedChallenge {
    pub id: String,
    pub message: String,
    pub expires_at_ms: u64,
}

struct StoredChallenge {
    message: String,
    expires_at_ms: u64,
}

/// Why issuing a challenge failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueError {
    /// The issue token bucket is empty or the outstanding cap is reached:
    /// 429 `ssh.challenge_busy`.
    Busy,
    /// The system random source failed: 500 `internal`.
    Entropy,
}

struct ChallengeInner {
    map: HashMap<String, StoredChallenge>,
    tokens: f64,
    last_refill: Instant,
}

impl Default for ChallengeInner {
    fn default() -> Self {
        ChallengeInner {
            map: HashMap::new(),
            tokens: SSH_CHALLENGE_ISSUE_QUOTA as f64,
            last_refill: Instant::now(),
        }
    }
}

/// The in-memory challenge table and its issue token bucket. Challenges are never persisted:
/// a restart invalidates every outstanding challenge and the user simply fetches a new one.
#[derive(Default)]
pub struct ChallengeState {
    inner: Mutex<ChallengeInner>,
}

impl std::fmt::Debug for ChallengeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChallengeState").finish_non_exhaustive()
    }
}

impl ChallengeState {
    pub fn new() -> ChallengeState {
        ChallengeState::default()
    }

    /// Issue one challenge. The randomness comes from `getrandom::fill` (a failure is an
    /// error, never the panicking `random_hex`) and is encoded with the existing
    /// `store::hex`.
    pub fn issue(&self, h: &dyn Host) -> Result<IssuedChallenge, IssueError> {
        self.issue_clocked(h.now(), h.wall_ms())
    }

    fn issue_clocked(&self, now: Instant, now_wall: u64) -> Result<IssuedChallenge, IssueError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .map
            .retain(|_, stored| stored.expires_at_ms > now_wall);
        let elapsed = now.saturating_duration_since(inner.last_refill);
        let gained = elapsed.as_secs_f64() / SSH_CHALLENGE_ISSUE_REFILL.as_secs_f64();
        if gained > 0.0 {
            inner.tokens = (inner.tokens + gained).min(SSH_CHALLENGE_ISSUE_QUOTA as f64);
            inner.last_refill = now;
        }
        if inner.tokens < 1.0 || inner.map.len() >= SSH_CHALLENGE_MAX_OUTSTANDING {
            return Err(IssueError::Busy);
        }
        let mut id_bytes = [0u8; 16];
        getrandom::fill(&mut id_bytes).map_err(|_| IssueError::Entropy)?;
        let mut message_bytes = [0u8; SSH_CHALLENGE_BYTES];
        getrandom::fill(&mut message_bytes).map_err(|_| IssueError::Entropy)?;
        let mut id = store::hex(&id_bytes);
        // A 128-bit collision is not expected; regenerate once so a repeated
        // id can never bypass the outstanding cap.
        if inner.map.contains_key(&id) {
            getrandom::fill(&mut id_bytes).map_err(|_| IssueError::Entropy)?;
            id = store::hex(&id_bytes);
        }
        let message = store::hex(&message_bytes);
        let expires_at_ms = now_wall + SSH_CHALLENGE_TTL.as_millis() as u64;
        inner.tokens -= 1.0;
        inner.map.insert(
            id.clone(),
            StoredChallenge {
                message: message.clone(),
                expires_at_ms,
            },
        );
        Ok(IssuedChallenge {
            id,
            message,
            expires_at_ms,
        })
    }

    /// Take one challenge out of the table and delete it: consumed before verification, one
    /// attempt per challenge, never resurrected by a failed signature.
    pub fn consume(&self, id: &str, now_wall_ms: u64) -> Option<String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.map.remove(id) {
            Some(stored) if stored.expires_at_ms > now_wall_ms => Some(stored.message),
            _ => None,
        }
    }

    /// The number of live challenges (for the fake-host control plane).
    pub fn outstanding(&self, now_wall_ms: u64) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .map
            .values()
            .filter(|stored| stored.expires_at_ms > now_wall_ms)
            .count()
    }
}

/// Why an SSH sign-in attempt failed. The two crypto failures share one response code; the
/// reason kind only feeds the log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshFailKind {
    BadSignature,
    KeyNotRegistered,
}

impl SshFailKind {
    /// The closed reason category of the `ssh_login_failed` log line: a
    /// public fingerprint or a fixed word, never challenge or key material.
    pub fn log_reason(&self) -> &'static str {
        match self {
            SshFailKind::BadSignature => "signature",
            SshFailKind::KeyNotRegistered => "key_not_registered",
        }
    }
}

/// The outcome of `ssh_login`, mirroring `auth::LoginOutcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshLoginOutcome {
    Ok {
        cookie: String,
        token: String,
        /// The public fingerprint and algorithm of the verified registered key, carried from
        /// the same snapshot that authorized the grant: the only values the success log line
        /// may name.
        fingerprint: String,
        algorithm: String,
    },
    /// The challenge id is unknown, already used or expired: 401
    /// `ssh.challenge_invalid`, no quota burn.
    ChallengeInvalid,
    /// A verification failure: 401 with the matching key, one quota burn.
    /// `fingerprint`/`algorithm` are the public log fields and are only set when the armor
    /// parsed. The failure count stays in the shared bucket and in the password login-shaped
    /// response (`show_reset_help`); it is deliberately not carried here, so the
    /// `ssh_login_failed` event cannot name it.
    Failed {
        kind: SshFailKind,
        fingerprint: Option<String>,
        algorithm: Option<String>,
        show_reset_help: bool,
    },
    RateLimited {
        retry_after_s: u64,
        show_reset_help: bool,
    },
    Credentials(CredentialState),
}

/// The public key fingerprint of an SSHSIG armor, computed from the embedded key: the table
/// lookup happens on this value.
fn embedded_fingerprint(sig: &SshSig) -> String {
    sig.public_key().fingerprint(HashAlg::Sha256).to_string()
}

/// The algorithm name of the embedded key, for log lines only.
fn embedded_algorithm(sig: &SshSig) -> String {
    sig.public_key().algorithm().as_str().to_string()
}

fn record_failure(
    state: &AuthState,
    guard: &mut tokio::sync::MutexGuard<'_, Option<crate::auth::LoginLimiter>>,
) -> u32 {
    let limiter = guard.as_mut().expect("login limiter");
    limiter.consume();
    limiter.consecutive_failures += 1;
    let failures = limiter.consecutive_failures;
    state
        .failure_count
        .store(failures, std::sync::atomic::Ordering::SeqCst);
    failures
}

/// Read the registered keys and session key from one credentials snapshot.
/// SSH and password failures share a quota bucket. Consume the challenge before parsing
/// or verifying the signature so every attempted grant spends that one-time challenge.
/// A successful grant signs the cookie with the snapshot's session key and spends no quota.
///
/// The credentials lock is never taken: a concurrent revoke or password change rotates
/// the session key, so a cookie minted from this snapshot fails its next session check.
pub async fn ssh_login(
    state: &AuthState,
    h: std::sync::Arc<dyn Host>,
    p: &Paths,
    challenge_id: String,
    signature: String,
) -> SshLoginOutcome {
    // the single snapshot read, before any quota or challenge is spent. The read
    // failure keeps its exact reason with the same mapping the password paths use; a missing
    // file is a read-only failure here — the SSH path never creates credentials.
    let snapshot = match state.snapshot_credentials(h.as_ref(), p) {
        Ok(credentials) => credentials,
        Err(state) => return SshLoginOutcome::Credentials(state),
    };
    let mut guard = state.limiter.lock().await;
    let now = h.now();
    {
        let limiter = guard.get_or_insert_with(|| crate::auth::LoginLimiter::new(now));
        if !limiter.available(now) {
            let retry_after_s = limiter.retry_after_s();
            let show_reset_help = state
                .failure_count
                .load(std::sync::atomic::Ordering::SeqCst)
                >= LOGIN_RESET_HELP_AFTER;
            drop(guard);
            return SshLoginOutcome::RateLimited {
                retry_after_s,
                show_reset_help,
            };
        }
    }
    // one challenge, one attempt — consumed before any signature
    // work, so a replay can never race a second signature through.
    let Some(message) = state.ssh_challenges.consume(&challenge_id, h.wall_ms()) else {
        drop(guard);
        return SshLoginOutcome::ChallengeInvalid;
    };
    let failed = |state: &AuthState,
                  guard: &mut tokio::sync::MutexGuard<'_, Option<crate::auth::LoginLimiter>>,
                  kind: SshFailKind,
                  fingerprint: Option<String>,
                  algorithm: Option<String>| {
        // The shared bucket still spends one attempt and counts the consecutive failure; only
        // the reset-help notice trigger crosses into the outcome, never the count itself.
        let failures = record_failure(state, guard);
        SshLoginOutcome::Failed {
            kind,
            fingerprint,
            algorithm,
            show_reset_help: failures >= LOGIN_RESET_HELP_AFTER,
        }
    };
    let sig = match SshSig::from_pem(signature.as_bytes()) {
        Ok(sig) => sig,
        Err(_) => {
            let outcome = failed(state, &mut guard, SshFailKind::BadSignature, None, None);
            drop(guard);
            return outcome;
        }
    };
    let fingerprint = embedded_fingerprint(&sig);
    let algorithm = embedded_algorithm(&sig);
    // the embedded key must be registered in the snapshot.
    let Some(registered) = snapshot
        .ssh_keys
        .iter()
        .find(|key| key.fingerprint == fingerprint)
    else {
        let outcome = failed(
            state,
            &mut guard,
            SshFailKind::KeyNotRegistered,
            Some(fingerprint),
            Some(algorithm),
        );
        drop(guard);
        return outcome;
    };
    // `Validate` guarantees stored lines parse; a failure here still fails
    // closed through the generic signature refusal.
    let registered_key = PublicKey::from_openssh(&registered.public_key).ok();
    let verified = registered_key
        .as_ref()
        .is_some_and(|key| key.verify(SSH_NAMESPACE, message.as_bytes(), &sig).is_ok());
    if !verified {
        let outcome = failed(
            state,
            &mut guard,
            SshFailKind::BadSignature,
            Some(fingerprint),
            Some(algorithm),
        );
        drop(guard);
        return outcome;
    }
    // success spends no quota and writes no usage time; the cookie
    // is signed with the same snapshot's session key, and the grant outcome
    // carries the verified key's public identity for the success log.
    {
        let limiter = guard.as_mut().expect("login limiter");
        limiter.consecutive_failures = 0;
    }
    state
        .failure_count
        .store(0, std::sync::atomic::Ordering::SeqCst);
    drop(guard);
    let issued = crate::auth::issue_session(&snapshot.session_key, h.wall_ms());
    let verified_algorithm = registered_key
        .as_ref()
        .expect("a verified signature implies the registered key parsed")
        .algorithm()
        .as_str()
        .to_string();
    SshLoginOutcome::Ok {
        cookie: issued.cookie,
        token: issued.token,
        fingerprint: registered.fingerprint.clone(),
        algorithm: verified_algorithm,
    }
}

/// Register one public key. The caller holds the credentials lock, has revalidated the
/// session with the request cookie under that lock, and re-read the current credentials; this
/// only mutates the given value. The write itself is the caller's single atomic
/// `write_credentials`.
pub fn add_ssh_key(
    h: &dyn Host,
    credentials: &mut Credentials,
    line: &str,
    comment: &str,
) -> Result<serde_json::Value, AddKeyError> {
    if line.len() > SSH_KEY_LINE_MAX || comment.len() > SSH_COMMENT_MAX {
        return Err(AddKeyError::Invalid(KeyInvalid::BadFormat));
    }
    let (key, fingerprint) = parse_public_key(line).map_err(AddKeyError::Invalid)?;
    if credentials
        .ssh_keys
        .iter()
        .any(|k| k.fingerprint == fingerprint)
    {
        return Err(AddKeyError::Exists(fingerprint));
    }
    if credentials.ssh_keys.len() >= SSH_MAX_KEYS {
        return Err(AddKeyError::Limit);
    }
    let entry = SshKey {
        id: store::random_hex(8),
        public_key: canonical_line(&key),
        fingerprint,
        comment: comment.to_string(),
        added_at: h.wall_ms(),
    };
    let item = entry.to_item();
    credentials.ssh_keys.push(entry);
    Ok(item)
}

/// The registration failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddKeyError {
    Invalid(KeyInvalid),
    Exists(String),
    Limit,
}

/// Remove one key and rotate the `session_key` in the same credentials value: the caller
/// publishes both through one `write_credentials` call, so the revoke and the rotation cannot
/// split.
pub fn remove_ssh_key(credentials: &mut Credentials, id: &str) -> Result<String, RemoveKeyError> {
    let Some(position) = credentials.ssh_keys.iter().position(|k| k.id == id) else {
        return Err(RemoveKeyError::NotFound);
    };
    let removed = credentials.ssh_keys.remove(position);
    credentials.session_key = store::random_hex(32);
    Ok(removed.fingerprint)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveKeyError {
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RealHost;

    #[test]
    fn challenge_ids_and_messages_use_lowercase_hex() {
        let state = ChallengeState::new();
        let issued = state.issue(&RealHost::on_host()).expect("issue");
        assert_eq!(issued.id.len(), 32);
        assert!(issued
            .id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(issued.message.len(), SSH_CHALLENGE_BYTES * 2);
        assert!(issued
            .message
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        // One challenge, one attempt.
        let message = state
            .consume(&issued.id, RealHost::on_host().wall_ms())
            .expect("live");
        assert_eq!(message, issued.message);
        assert!(state
            .consume(&issued.id, RealHost::on_host().wall_ms())
            .is_none());
    }

    #[test]
    fn expired_challenges_are_not_consumed() {
        let state = ChallengeState::new();
        let issued = state.issue(&RealHost::on_host()).expect("issue");
        let late = RealHost::on_host().wall_ms() + SSH_CHALLENGE_TTL.as_millis() as u64 + 1;
        assert!(state.consume(&issued.id, late).is_none());
    }

    #[test]
    fn the_issue_bucket_drains_and_refills() {
        let state = ChallengeState::new();
        let t0 = Instant::now();
        for _ in 0..SSH_CHALLENGE_ISSUE_QUOTA {
            state
                .issue_clocked(t0, 1_000)
                .expect("the bucket starts full");
        }
        assert!(matches!(
            state.issue_clocked(t0, 1_000),
            Err(IssueError::Busy)
        ));
        // One token returns after the refill interval.
        state
            .issue_clocked(t0 + SSH_CHALLENGE_ISSUE_REFILL, 2_000)
            .ok();
        // The outstanding cap holds even with tokens left: fill up to the
        // cap with fresh buckets.
        let full = ChallengeState::new();
        let t1 = Instant::now();
        for i in 0..SSH_CHALLENGE_MAX_OUTSTANDING {
            full.issue_clocked(
                t1 + SSH_CHALLENGE_ISSUE_REFILL * (i as u32 + 1),
                3_000 + i as u64,
            )
            .expect("cap run");
        }
        assert!(matches!(
            full.issue_clocked(t1 + SSH_CHALLENGE_ISSUE_REFILL * 300, 9_000),
            Err(IssueError::Busy)
        ));
    }

    #[test]
    fn weak_and_unsupported_public_keys_are_refused() {
        // Identity-point Ed25519 encoding (all zero bytes): keyless forgeries verify against
        // it.
        let weak =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(parse_public_key(weak), Err(KeyInvalid::WeakKey));
        assert_eq!(parse_public_key("not a key"), Err(KeyInvalid::BadFormat));
    }

    #[test]
    fn a_valid_line_parses_with_stripped_comment() {
        let line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti user@example.com";
        let (key, fingerprint) = parse_public_key(line).expect("valid");
        assert_eq!(key.algorithm().as_str(), "ssh-ed25519");
        assert!(fingerprint.starts_with("SHA256:"));
        assert_eq!(
            canonical_line(&key),
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti"
        );
        // The stored-key validation accepts exactly the canonical form.
        let stored = SshKey {
            id: "0123456789abcdef".into(),
            public_key: canonical_line(&key),
            fingerprint: fingerprint.clone(),
            comment: String::new(),
            added_at: 1,
        };
        assert!(validate_stored_key(&stored).is_ok());
        // A stale fingerprint, a non-canonical line and a bad id are all
        // refused.
        let mut bad = stored.clone();
        bad.fingerprint = "SHA256:AAAA".into();
        assert!(validate_stored_key(&bad).is_err());
        let mut non_canonical = stored.clone();
        non_canonical.public_key = line.into();
        assert!(validate_stored_key(&non_canonical).is_err());
        let mut bad_id = stored.clone();
        bad_id.id = "XYZ".into();
        assert!(validate_stored_key(&bad_id).is_err());
        let mut weak_stored = stored.clone();
        weak_stored.public_key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                .into();
        assert!(validate_stored_key(&weak_stored).is_err());
    }
}

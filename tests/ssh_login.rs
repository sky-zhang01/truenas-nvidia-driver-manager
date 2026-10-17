//! SSH sign-in tests against the production router on a fake host, using real SSHSIGs produced
//! in-process with the same `ssh-key` crate as the server.

mod common;

use std::time::Duration;

use axum::http::{header, Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use tower::ServiceExt;
use truenas_nvidia_driver_manager::auth::{self, Credentials};
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::host::Host;
use truenas_nvidia_driver_manager::ssh_login;
use truenas_nvidia_driver_manager::store::{self, Paths, Validate};

const CREDENTIALS: &str = "/var/lib/nvm/manager/secrets/web-credentials.json";

/// A throwaway ecdsa-p256 key generated with `ssh-keygen` for this test suite (public material
/// only).
const P256_LINE: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBB81N0gm0cdTtOzqVDSnjtEEvJOCGnwJ/RnxxnWcGOWeTWoh1fOcQtwg2IkdG7p/I3g6N1i/guSkA07BvizWNfs= nvm-ssh-test";
/// A throwaway ecdsa-p384 key generated with `ssh-keygen` for this test suite (public material
/// only).
const P384_LINE: &str = "ecdsa-sha2-nistp384 AAAAE2VjZHNhLXNoYTItbmlzdHAzODQAAAAIbmlzdHAzODQAAABhBG7IIijACvZgjqHn4d3SeYi35rd9qGr7M0C9eFKx1maixN8vUSJoWO7vVyRtnrVq8vbWBTierYaTSkWE1iUkgSafgv3Q71SO1hbb97mQFQyUO4KEsqKHHraDkX+L1BIdhQ== nvm-ssh-test";
/// A throwaway rsa key generated with `ssh-keygen` (public material only): the algorithm the
/// registration gate must refuse.
const RSA_LINE: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDaHbc46bhqRsYqowei3smwtPsGxqzXPXGMQnz1FGnf7Tgdo5xUYY1YslfeyWO289L1pqRD80DRqy01PIkA4R8oVJYadSlMKVlFJbHja1AAyKFIn3Py6n0IGjDyXqrXNyqRnloqFND6UC1yfjyihLvAJtoA5xXIB8/sIStuQeKQN1pGRY2f2Nc4P80Yoc9/VEPICz4TH18ko8N9sDAPAvVxAywyALqbCEpC1rvFZd8I3qv25zgMt4v1x66aRnOgecWqjtKsZHXOaAmhTKEL9U6m9Ywg8XN+/v0+NsR/X/rsoNFXrXDWhArNIRPXhnHwH7dkEnngCM2Yj6G2QKhGNhyZ nvm-ssh-test";
/// The identity-point Ed25519 encoding: `ssh-key` accepts it at parse time, but keyless forgeries
/// verify against it.
const WEAK_LINE: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// A fresh Ed25519 keypair derived from a fixed seed: private material lives only in this test
/// process and is generated per run.
struct TestKey {
    public_line: String,
    private: ssh_key::PrivateKey,
}

fn make_key(seed: u8) -> TestKey {
    let mut seed_bytes = [0u8; 32];
    seed_bytes[0] = seed;
    let keypair = ssh_key::private::Ed25519Keypair::from_seed(&seed_bytes);
    let private =
        ssh_key::PrivateKey::new(ssh_key::private::KeypairData::Ed25519(keypair), "nvm-test")
            .expect("test key");
    let public_line = private
        .public_key()
        .to_openssh()
        .expect("public line")
        .trim_end()
        .to_string();
    TestKey {
        public_line,
        private,
    }
}

/// A real SSHSIG over `message` in `namespace`, as PEM armor.
fn sign_armor(key: &TestKey, namespace: &str, message: &[u8]) -> String {
    key.private
        .sign(namespace, ssh_key::HashAlg::Sha512, message)
        .expect("sign")
        .to_pem(ssh_key::LineEnding::LF)
        .expect("armor")
}

/// A fresh app with the initial password already changed, plus its cookie.
async fn setup_changed() -> (TestApp, String) {
    let host = default_host();
    signed_in_app(host).await
}

async fn issue_challenge(app: &TestApp) -> Value {
    let (status, body) = post(app, "/api/ssh/challenge", json!({}), None).await;
    assert_eq!(status, StatusCode::OK, "challenge failed: {body}");
    body
}

async fn register_key(app: &TestApp, cookie: &str, line: &str) -> (StatusCode, Value) {
    post(
        app,
        "/api/ssh/keys",
        json!({"public_key": line, "comment": "test key"}),
        Some(cookie),
    )
    .await
}

async fn ssh_login_attempt(app: &TestApp, key: &TestKey) -> (StatusCode, Value, Option<String>) {
    let challenge = issue_challenge(app).await;
    let signature = sign_armor(
        key,
        SSH_NAMESPACE,
        challenge["challenge"].as_str().unwrap().as_bytes(),
    );
    let (status, body, headers) = request(
        app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({
            "challenge_id": challenge["challenge_id"],
            "signature": signature,
        })),
        None,
    )
    .await;
    let cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim_start_matches("nvm_session=").to_string());
    (status, body, cookie)
}

fn read_credentials_file(app: &TestApp) -> Vec<u8> {
    app.host
        .read(&Paths::new(CONTAINER_DATA_DIR).credentials())
        .unwrap()
}

#[tokio::test]
async fn registration_accepts_the_three_algorithms_and_lists_them() {
    let (app, cookie) = setup_changed().await;
    for line in [
        make_key(1).public_line.clone(),
        P256_LINE.to_string(),
        P384_LINE.to_string(),
    ] {
        let (status, body) = register_key(&app, &cookie, &line).await;
        assert_eq!(status, StatusCode::CREATED, "registration failed: {body}");
        assert!(body["fingerprint"].as_str().unwrap().starts_with("SHA256:"));
        assert_eq!(body["public_key"].as_str().unwrap().split(' ').count(), 2);
        assert_eq!(body["comment"].as_str().unwrap(), "test key");
    }
    // The list is sorted by added_at and carries all three algorithms.
    let (status, body) = get(&app, "/api/ssh/keys", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    let algorithms: Vec<&str> = items
        .iter()
        .map(|i| i["algorithm"].as_str().unwrap())
        .collect();
    assert!(algorithms.contains(&"ssh-ed25519"));
    assert!(algorithms.contains(&"ecdsa-sha2-nistp256"));
    assert!(algorithms.contains(&"ecdsa-sha2-nistp384"));
    let added: Vec<u64> = items
        .iter()
        .map(|i| i["added_at"].as_u64().unwrap())
        .collect();
    let mut sorted = added.clone();
    sorted.sort();
    assert_eq!(added, sorted);
}

#[tokio::test]
async fn registration_refuses_garbage_rsa_sk_and_weak_keys() {
    let (app, cookie) = setup_changed().await;
    let (status, body) = register_key(&app, &cookie, "not a key").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "ssh.key_invalid");
    assert_eq!(
        body["params"]["reason"]["key"],
        "ssh.key_invalid.bad_format"
    );
    let (status, body) = register_key(&app, &cookie, RSA_LINE).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["params"]["reason"]["key"],
        "ssh.key_invalid.unsupported_type"
    );
    assert_eq!(body["params"]["reason"]["params"]["algorithm"], "ssh-rsa");
    let cert_line = format!("{}-cert-v01@openssh.com AAAAC3NzaC1lZDI1NTE5AAAAIIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "ssh-ed25519");
    let (status, body) = register_key(&app, &cookie, &cert_line).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let sk_line = "sk-ssh-ed25519@openssh.com AAAAGnNrLXNoYTItZWRkMjU1MTkAAAAIssh-ed25519-test sk";
    let (status, body) = register_key(&app, &cookie, sk_line).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = register_key(&app, &cookie, WEAK_LINE).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["params"]["reason"]["key"], "ssh.key_invalid.weak_key");
}

#[tokio::test]
async fn registration_enforces_session_limit_and_duplicates() {
    let (app, cookie) = setup_changed().await;
    let key = make_key(2);
    let (status, _) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    let with_comment = format!("{} another-comment", key.public_line);
    let (status, body) = register_key(&app, &cookie, &with_comment).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "ssh.key_exists");
    assert!(body["params"]["fingerprint"]
        .as_str()
        .unwrap()
        .starts_with("SHA256:"));
    for seed in 3..=6 {
        let (status, body) = register_key(&app, &cookie, &make_key(seed).public_line).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = register_key(&app, &cookie, &make_key(9).public_line).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "ssh.key_limit");
    let (status, body) = get(&app, "/api/ssh/keys", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth.required");
    let (status, body) = post(
        &app,
        "/api/ssh/keys",
        json!({"public_key": key.public_line}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth.required");
    let (status, body, _) = request(
        &app,
        Method::DELETE,
        "/api/ssh/keys/0123456789abcdef",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth.required");
}

#[tokio::test]
async fn a_must_change_session_can_read_but_not_register() {
    let host = default_host();
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    let cookie = login(&app, "admin", "hunter12").await;
    // GET is allowed by the must-change gate (read-only).
    let (status, body) = get(&app, "/api/ssh/keys", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    // POST is blocked before any write.
    let (status, body) = register_key(&app, &cookie, &make_key(4).public_line).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "auth.password_change_required");
    // The invariant holds on disk too: no key while the password must change. A hand-edited
    // violation fails the whole file closed.
    let mut file: Value = serde_json::from_slice(&read_credentials_file(&app)).unwrap();
    file["ssh_keys"] = json!([{
        "id": "0123456789abcdef",
        "public_key": make_key(4).public_line,
        "fingerprint": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "comment": "",
        "added_at": 1,
    }]);
    app.host.put_text(CREDENTIALS, &file.to_string());
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["credentials"],
        json!({"unreadable": "field_invalid"}),
        "keys while the password must change must fail closed"
    );
}

#[tokio::test]
async fn challenges_have_the_required_shape_and_die_after_one_use() {
    let (app, cookie) = setup_changed().await;
    let _ = register_key(&app, &cookie, &make_key(5).public_line).await;
    let body = issue_challenge(&app).await;
    assert_eq!(body["namespace"], SSH_NAMESPACE);
    assert_eq!(body["expires_in_s"], 120u64);
    let id = body["challenge_id"].as_str().unwrap();
    assert_eq!(id.len(), 32, "a 128-bit id as hex");
    assert!(id
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    let challenge = body["challenge"].as_str().unwrap();
    assert_eq!(challenge.len(), SSH_CHALLENGE_BYTES * 2);
    assert!(challenge
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    // The namespace is fixed server-side: sending one is a decode error.
    let (status, _) = post(
        &app,
        "/api/ssh/challenge",
        json!({"namespace": "other"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // A successful login consumes the challenge: the same id cannot be replayed, not even with
    // another valid signature.
    let key = make_key(5);
    let signature = sign_armor(&key, SSH_NAMESPACE, challenge.as_bytes());
    let (status, _, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({"challenge_id": id, "signature": signature.clone()})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({"challenge_id": id, "signature": signature})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "ssh.challenge_invalid");
    // An expired challenge answers the same code (fake clock +121s).
    let expired = issue_challenge(&app).await;
    app.host.advance(Duration::from_secs(121));
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({
            "challenge_id": expired["challenge_id"],
            "signature": sign_armor(
                &key,
                SSH_NAMESPACE,
                expired["challenge"].as_str().unwrap().as_bytes(),
            ),
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "ssh.challenge_invalid");
    // The issue bucket drains: ten challenges, then busy, one token back after ten fake seconds.
    for _ in 0..10 {
        issue_challenge(&app).await;
    }
    let (status, body) = post(&app, "/api/ssh/challenge", json!({}), None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"], "ssh.challenge_busy");
    app.host.advance(Duration::from_secs(10));
    issue_challenge(&app).await;
}

#[tokio::test]
async fn a_valid_signature_signs_in_without_touching_the_file() {
    let (app, cookie) = setup_changed().await;
    let key = make_key(6);
    let (status, _) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    let before = read_credentials_file(&app);
    let (status, body, ssh_cookie) = ssh_login_attempt(&app, &key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["must_change_password"], false);
    let cookie = ssh_cookie.expect("a session cookie");
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "the ssh session works: {body}");
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ssh_enabled"], true);
    assert_eq!(read_credentials_file(&app), before);
    let challenge = issue_challenge(&app).await;
    let crlf = sign_armor(
        &key,
        SSH_NAMESPACE,
        challenge["challenge"].as_str().unwrap().as_bytes(),
    )
    .replace('\n', "\r\n");
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({
            "challenge_id": challenge["challenge_id"],
            "signature": crlf,
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "CRLF armor must verify: {body}");
}

#[tokio::test]
async fn wrong_namespace_cross_challenge_and_tampered_armor_fail() {
    let (app, cookie) = setup_changed().await;
    let key = make_key(7);
    let (status, _) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    let challenge = issue_challenge(&app).await;
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({
            "challenge_id": challenge["challenge_id"],
            "signature": sign_armor(&key, "git", challenge["challenge"].as_str().unwrap().as_bytes()),
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "ssh.login_failed");
    // Cross challenge: the signature covers another challenge's bytes.
    let first = issue_challenge(&app).await;
    let second = issue_challenge(&app).await;
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({
            "challenge_id": first["challenge_id"],
            "signature": sign_armor(&key, SSH_NAMESPACE, second["challenge"].as_str().unwrap().as_bytes()),
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "ssh.login_failed");
    // Tampered armor: corrupting the signature blob of the armor fails cryptically; corrupting the
    // embedded key blob lands on the key_not_registered lookup instead — both are refused.
    let third = issue_challenge(&app).await;
    let mut armor = sign_armor(
        &key,
        SSH_NAMESPACE,
        third["challenge"].as_str().unwrap().as_bytes(),
    );
    let end = armor.trim_end().len();
    armor.replace_range(end - 8..end - 4, "AAAA");
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({"challenge_id": third["challenge_id"], "signature": armor})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "ssh.login_failed");
    let stranger = make_key(8);
    let (status, body, _) = ssh_login_attempt(&app, &stranger).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "ssh.key_not_registered");
    // Trailing newline in the signed message must fail (the signed bytes are the 64 ASCII
    // characters, nothing else).
    let fourth = issue_challenge(&app).await;
    let with_newline = format!("{}\n", fourth["challenge"].as_str().unwrap());
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({
            "challenge_id": fourth["challenge_id"],
            "signature": sign_armor(&key, SSH_NAMESPACE, with_newline.as_bytes()),
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "ssh.login_failed");
}

#[tokio::test]
async fn oversized_fields_are_rejected_before_any_challenge_work() {
    let (app, _cookie) = setup_changed().await;
    let (status, body) = post(
        &app,
        "/api/ssh/login",
        json!({"challenge_id": "x".repeat(65), "signature": "x"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "request.invalid");
    let (status, body) = post(
        &app,
        "/api/ssh/login",
        json!({"challenge_id": "x", "signature": "x".repeat(SSH_SIGNATURE_MAX + 1)}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // The oversized public key of a registration is a 422 with the bad_format reason.
    let (status, body) = post(
        &app,
        "/api/ssh/keys",
        json!({"public_key": "x".repeat(SSH_KEY_LINE_MAX + 1), "comment": ""}),
        Some(&_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "ssh.key_invalid");
}

#[tokio::test]
async fn ssh_failures_share_the_password_bucket() {
    let (app, cookie) = setup_changed().await;
    let key = make_key(10);
    let (status, _) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    // Five failed SSH attempts burn the shared bucket.
    for _ in 0..5 {
        let challenge = issue_challenge(&app).await;
        let (status, body, _) = request(
            &app,
            Method::POST,
            "/api/ssh/login",
            Some(json!({
                "challenge_id": challenge["challenge_id"],
                "signature": "-----BEGIN SSH SIGNATURE-----\ngarbage\n-----END SSH SIGNATURE-----\n",
            })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    }
    // The sixth SSH attempt is rate limited; login help appears after the third consecutive
    // failure, using the same parameters as password login.
    let challenge = issue_challenge(&app).await;
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({"challenge_id": challenge["challenge_id"], "signature": "x"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"], "login.rate_limited");
    assert!(body["params"]["retry_after_s"].as_u64().unwrap() <= 30);
    assert_eq!(body["params"]["show_reset_help"], true);
    // The very same bucket gates the password login.
    let (status, body) = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "longpass99"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"], "login.rate_limited");
    // After the refill window the correct password works again.
    app.host.advance(Duration::from_secs(31));
    let (status, body) = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "longpass99"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn revoking_a_key_rotates_sessions_and_keeps_everything_else() {
    let (app, cookie) = setup_changed().await;
    let first = make_key(11);
    let second = make_key(12);
    let (status, _) = register_key(&app, &cookie, &first.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = register_key(&app, &cookie, &second.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, _, ssh_cookie) = ssh_login_attempt(&app, &first).await;
    let ssh_cookie = ssh_cookie.expect("ssh session");
    let (status, body) = get(&app, "/api/ssh/keys", Some(&ssh_cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let expected = ssh_login::parse_public_key(&first.public_line)
        .expect("valid line")
        .1;
    let items = body["items"].as_array().unwrap().clone();
    let target = items
        .iter()
        .find(|i| i["fingerprint"].as_str() == Some(expected.as_str()))
        .expect("the first key is listed");
    let id = target["id"].as_str().unwrap().to_string();
    let (status, body, headers) = request(
        &app,
        Method::DELETE,
        &format!("/api/ssh/keys/{id}"),
        None,
        Some(&ssh_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("a new cookie after the revoke");
    assert!(new_cookie.contains("nvm_session="));
    let (status, _) = get(&app, "/api/home", Some(&ssh_cookie)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the old ssh cookie is dead"
    );
    let (status, body) = get(&app, "/api/home", Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the pre-revoke password cookie is dead too: {body}"
    );
    // The revoked key can no longer sign in; the other key still can; the password still works.
    let (status, body, _) = ssh_login_attempt(&app, &first).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "ssh.key_not_registered");
    let (status, _, _) = ssh_login_attempt(&app, &second).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "longpass99"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let saved: Credentials = serde_json::from_slice(&read_credentials_file(&app)).unwrap();
    saved.validate().expect("the file stays valid");
    assert_eq!(saved.username, "admin");
    assert!(verify_helper(&saved.password_hash, "longpass99"));
    assert_eq!(saved.ssh_keys.len(), 1);
    let (status, body, _) = request(
        &app,
        Method::DELETE,
        "/api/ssh/keys/ffffffffffffffff",
        None,
        Some(&ssh_cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the rotated session may not delete: {body}"
    );
}

fn verify_helper(hash: &str, password: &str) -> bool {
    auth::verify_password(hash, password)
}

#[tokio::test]
async fn a_failed_registration_write_changes_nothing() {
    let (app, cookie) = setup_changed().await;
    let before = read_credentials_file(&app);
    // Refusing temporary-file writes in the secrets directory makes credential publication fail.
    app.host.fail_write("/var/lib/nvm/manager/secrets/");
    let (status, body) = register_key(&app, &cookie, &make_key(13).public_line).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "ssh.keys_unsaved");
    assert_eq!(body["params"]["reason"], "directory");
    assert_eq!(
        read_credentials_file(&app),
        before,
        "the old bytes stay the value of record"
    );
    let (status, body, _) = request(
        &app,
        Method::DELETE,
        "/api/ssh/keys/0123456789abcdef",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn credentials_without_ssh_keys_allow_password_login_and_key_registration() {
    let host = default_host();
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    // The optional key list is absent.
    let credentials_without_keys = json!({
        "username": "admin",
        "password_hash": auth::hash_password("longpass99").unwrap(),
        "must_change_password": false,
        "session_key": store::random_hex(32),
    });
    app.host
        .put_text(CREDENTIALS, &credentials_without_keys.to_string());
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ssh_enabled"], false);
    assert_eq!(body["credentials"], "ok");
    let cookie = login(&app, "admin", "longpass99").await;
    let (status, body) = get(&app, "/api/ssh/keys", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    // The endpoints are available; a login attempt simply finds no key.
    let key = make_key(14);
    let (status, body, _) = ssh_login_attempt(&app, &key).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "ssh.key_not_registered");
    // Registering a key persists the optional list without changing the password hash.
    let (status, body) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let saved: Credentials = serde_json::from_slice(&read_credentials_file(&app)).unwrap();
    assert_eq!(saved.ssh_keys.len(), 1);
    assert!(verify_helper(&saved.password_hash, "longpass99"));
}

#[tokio::test]
async fn a_hand_edited_ssh_keys_field_fails_the_whole_file_closed() {
    let (app, cookie) = setup_changed().await;
    let (status, _) = register_key(&app, &cookie, &make_key(15).public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    let mut file: Value = serde_json::from_slice(&read_credentials_file(&app)).unwrap();
    file["ssh_keys"].as_array_mut().unwrap().push(json!({
        "id": "0123456789abcdef",
        "public_key": WEAK_LINE,
        "fingerprint": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "comment": "",
        "added_at": 1,
    }));
    app.host.put_text(CREDENTIALS, &file.to_string());
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credentials"], json!({"unreadable": "field_invalid"}));
    assert_eq!(body["ssh_enabled"], false);
    // The password entrance reports the credentials failure state through password login.
    let (status, body) = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "longpass99"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "login.credentials_unusable");
    assert_eq!(
        body["params"]["state"],
        json!({"unreadable": "field_invalid"})
    );
    let mut file: Value = serde_json::from_slice(&read_credentials_file(&app)).unwrap();
    file["ssh_keys"][0]["fingerprint"] =
        json!("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    app.host.put_text(CREDENTIALS, &file.to_string());
    let (status, body) = get(&app, "/api/login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credentials"], json!({"unreadable": "field_invalid"}));
}

#[test]
fn the_ssh_log_events_carry_the_required_keys() {
    assert_eq!(Event::SshLoginSucceeded.as_str(), "ssh_login_succeeded");
    assert_eq!(Event::SshLoginFailed.as_str(), "ssh_login_failed");
    assert_eq!(Event::SshKeyAdded.as_str(), "ssh_key_added");
    assert_eq!(Event::SshKeyRemoved.as_str(), "ssh_key_removed");
    assert!(!Event::SshKeyAdded.msg().is_empty());
    assert_eq!(
        ssh_login::SshFailKind::BadSignature.log_reason(),
        "signature"
    );
    assert_eq!(
        ssh_login::SshFailKind::KeyNotRegistered.log_reason(),
        "key_not_registered"
    );
}

#[tokio::test]
async fn one_challenge_grants_exactly_one_login_under_eight_races() {
    let (app, cookie) = setup_changed().await;
    let key = make_key(16);
    let (status, _) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    let challenge = issue_challenge(&app).await;
    let signature = sign_armor(
        &key,
        SSH_NAMESPACE,
        challenge["challenge"].as_str().unwrap().as_bytes(),
    );
    let mut joins = Vec::new();
    for _ in 0..8 {
        let app = app.clone();
        let body = json!({
            "challenge_id": challenge["challenge_id"],
            "signature": signature,
        });
        joins.push(tokio::spawn(async move {
            request(&app, Method::POST, "/api/ssh/login", Some(body), None)
                .await
                .0
        }));
    }
    let results = futures_join(joins).await;
    let ok = results.iter().filter(|s| **s == StatusCode::OK).count();
    let invalid = results
        .iter()
        .filter(|s| **s == StatusCode::UNAUTHORIZED)
        .count();
    assert_eq!(ok, 1, "exactly one login wins: {results:?}");
    assert_eq!(invalid, 7, "the rest consume a dead challenge: {results:?}");
}

/// `futures::join_all` equivalent on plain tokio task handles.
async fn futures_join(joins: Vec<tokio::task::JoinHandle<StatusCode>>) -> Vec<StatusCode> {
    let mut out = Vec::new();
    for join in joins {
        out.push(join.await.expect("join"));
    }
    out
}

#[tokio::test]
async fn no_live_session_survives_a_completed_revoke() {
    for _ in 0..8 {
        let (app, cookie) = setup_changed().await;
        let key = make_key(17);
        let (status, _) = register_key(&app, &cookie, &key.public_line).await;
        assert_eq!(status, StatusCode::CREATED);
        let items = get(&app, "/api/ssh/keys", Some(&cookie)).await.1;
        let id = items["items"][0]["id"].as_str().unwrap().to_string();
        // The login and the revoke run concurrently: whichever way they interleave, the cookie the
        // login obtained must be dead once the revoke completed.
        let login_app = app.clone();
        let login_key_public = key.public_line.clone();
        let login = tokio::spawn(async move {
            let challenge = issue_challenge(&login_app).await;
            let key = key_from_line(&login_key_public);
            let body = json!({
                "challenge_id": challenge["challenge_id"],
                "signature": sign_armor(&key, SSH_NAMESPACE, challenge["challenge"].as_str().unwrap().as_bytes()),
            });
            let (status, _, headers) =
                request(&login_app, Method::POST, "/api/ssh/login", Some(body), None).await;
            headers
                .get(header::SET_COOKIE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                .map(|v| v.trim_start_matches("nvm_session=").to_string())
                .filter(|_| status == StatusCode::OK)
        });
        let delete = tokio::spawn({
            let app = app.clone();
            let cookie = cookie.clone();
            let uri = format!("/api/ssh/keys/{id}");
            async move {
                request(&app, Method::DELETE, &uri, None, Some(&cookie))
                    .await
                    .0
            }
        });
        let (login, delete) = tokio::join!(login, delete);
        assert_eq!(delete.expect("delete join"), StatusCode::OK);
        let login_cookie: Option<String> = login.expect("login join");
        if let Some(cookie) = login_cookie {
            let (status, _, _) = request(&app, Method::GET, "/api/home", None, Some(&cookie)).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "a cookie minted by the racing login must not live"
            );
        }
    }
}

/// Rebuild the signing key of a registered line (the seed is the fixed test seed, so the public
/// half matches).
fn key_from_line(line: &str) -> TestKey {
    for seed in 1u8..=32 {
        let key = make_key(seed);
        if key.public_line == line {
            return key;
        }
    }
    panic!("the line was not produced by this test's seeds: {line}");
}

#[tokio::test]
async fn a_password_change_keeps_the_key_but_kills_the_sessions() {
    let (app, cookie) = setup_changed().await;
    let key = make_key(18);
    let (status, _) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, _, ssh_cookie) = ssh_login_attempt(&app, &key).await;
    let ssh_cookie = ssh_cookie.expect("ssh session");
    let (status, _, headers) = request(
        &app,
        Method::POST,
        "/api/password",
        Some(json!({"new_password": "newpass99"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.contains_key(header::SET_COOKIE));
    let (status, _) = get(&app, "/api/home", Some(&ssh_cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the ssh session died");
    // The key survived: a fresh ssh login works against the new session key.
    let (status, body, _) = ssh_login_attempt(&app, &key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let saved: Credentials = serde_json::from_slice(&read_credentials_file(&app)).unwrap();
    assert!(verify_helper(&saved.password_hash, "newpass99"));
    assert_eq!(saved.ssh_keys.len(), 1);
}

#[tokio::test]
async fn a_registration_and_a_password_change_do_not_lose_updates() {
    let (app, cookie) = setup_changed().await;
    let key = make_key(19);
    let (status, body) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/password",
        Some(json!({"new_password": "newpass99"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let saved: Credentials = serde_json::from_slice(&read_credentials_file(&app)).unwrap();
    assert!(verify_helper(&saved.password_hash, "newpass99"));
    assert_eq!(saved.ssh_keys.len(), 1);
    // Concurrent: serialized by the credentials lock; whichever wins, the final file never clobbers
    // the other effect with a stale read.
    let (app, cookie) = setup_changed().await;
    let cookie2 = cookie.clone();
    let register = tokio::spawn({
        let app = app.clone();
        let line = make_key(20).public_line;
        async move {
            post(
                &app,
                "/api/ssh/keys",
                json!({"public_key": line, "comment": ""}),
                Some(&cookie2),
            )
            .await
            .0
        }
    });
    let change = tokio::spawn({
        let app = app.clone();
        let cookie = cookie.clone();
        async move {
            post(
                &app,
                "/api/password",
                json!({"new_password": "newpass99"}),
                Some(&cookie),
            )
            .await
            .0
        }
    });
    let (register, change) = tokio::join!(register, change);
    let register = register.expect("register join");
    let change = change.expect("change join");
    assert_eq!(change, StatusCode::OK, "the password change itself wins");
    assert!(
        register == StatusCode::CREATED || register == StatusCode::UNAUTHORIZED,
        "the registration either ran before the rotation or was refused: {register}"
    );
    let saved: Credentials = serde_json::from_slice(&read_credentials_file(&app)).unwrap();
    // The new password hash is always there: a racing registration can never write a stale snapshot
    // over it.
    assert!(verify_helper(&saved.password_hash, "newpass99"));
    let expected_keys = if register == StatusCode::CREATED {
        1
    } else {
        0
    };
    assert_eq!(saved.ssh_keys.len(), expected_keys);
}

#[tokio::test]
async fn a_stale_session_cannot_write_keys_or_passwords_deterministically() {
    let (app, cookie) = setup_changed().await;
    let key = make_key(21);
    let (status, _) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    // Freeze the critical section: hold the credentials lock so the password change and key
    // requests queue after their middleware check.
    let guard = app.ctx.auth.credentials_lock.lock().await;
    app.host.clear_ops();
    let password_change_request = tokio::spawn({
        let router = app.router.clone();
        let cookie = cookie.clone();
        async move {
            let response = router
                .oneshot(
                    axum::http::Request::builder()
                        .method(Method::POST)
                        .uri("/api/password")
                        .header(header::HOST, "nas.example:8199")
                        .header(header::ORIGIN, "http://nas.example:8199")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::COOKIE, format!("nvm_session={cookie}"))
                        .header("x-nvm-token", token_for(&cookie).unwrap_or_default())
                        .body(axum::body::Body::from(
                            json!({"new_password": "newpass99"}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            (
                response.status(),
                response.headers().contains_key(header::SET_COOKIE),
                axum::body::to_bytes(response.into_body(), 1 << 20)
                    .await
                    .unwrap()
                    .to_vec(),
            )
        }
    });
    // Deterministically wait until the request has passed the session middleware: while the lock is
    // held here, the only credentials read it can perform is the middleware's own.
    let mut middleware_read = false;
    for _ in 0..50_000 {
        if app.host.ops_for(CREDENTIALS).contains(&"read") {
            middleware_read = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        middleware_read,
        "the password change request never passed the middleware"
    );
    // Model the completed concurrent revoke: rotate the session key and clear the logged-out table
    // exactly as DELETE does, without needing the lock this test holds.
    let paths = Paths::new(CONTAINER_DATA_DIR);
    let mut rotated = app
        .ctx
        .auth
        .snapshot_credentials(&app.host, &paths)
        .expect("snapshot");
    rotated.session_key = store::random_hex(32);
    rotated.ssh_keys.clear();
    auth::write_credentials(&app.host, &paths, &rotated).expect("rotate");
    app.ctx.auth.clear_logged_out(&app.host, &paths);
    let before = read_credentials_file(&app);
    // Release the lock: the queued password change acquires it and must recheck to 401.
    drop(guard);
    let (status, has_cookie, body) = password_change_request
        .await
        .expect("password_change_request join");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(!has_cookie, "no new cookie for a stale session");
    assert_eq!(
        read_credentials_file(&app),
        before,
        "the stale password change changed nothing"
    );
    // The same lock-held recheck gates the key endpoints: a queued registration with the
    // rotated-out cookie cannot write.
    let (status, body) = register_key(&app, &cookie, &make_key(22).public_line).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "auth.required");
}

/// Every recorded operation under the credentials directory that is not a read: a read-only failure
/// must not leave one behind.
fn secrets_mutations(app: &TestApp) -> Vec<(String, &'static str)> {
    app.host
        .ops()
        .into_iter()
        .filter(|op| {
            op.path.contains("/manager/secrets") && !matches!(op.kind, "read" | "read_dir" | "open")
        })
        .map(|op| (op.path, op.kind))
        .collect()
}

/// One `POST /api/ssh/login` against an unreadable credentials snapshot: the exact state, the same
/// state through the password entrance (the mapping is shared, not SSH-specific), then the shared
/// side-effect assertions — the challenge stays live, the shared failure bucket stays untouched and
/// no write touched the secrets directory.
async fn unreadable_attempt(app: &TestApp, expected: &str, context: &str) {
    let challenges_before = app.ctx.auth.ssh_challenges.outstanding(app.host.wall_ms());
    let challenge = issue_challenge(app).await;
    assert_eq!(
        app.ctx.auth.ssh_challenges.outstanding(app.host.wall_ms()),
        challenges_before + 1,
        "{context}: the challenge was issued"
    );
    app.host.clear_ops();
    let (status, body) = post(
        app,
        "/api/ssh/login",
        json!({"challenge_id": challenge["challenge_id"], "signature": "x"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{context}: {body}");
    assert_eq!(body["error"], "login.credentials_unusable", "{context}");
    assert_eq!(
        body["params"]["state"],
        json!({"unreadable": expected}),
        "{context}"
    );
    let (status, body) = post(
        app,
        "/api/login",
        json!({"username": "admin", "password": "hunter12"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{context}: {body}");
    assert_eq!(
        body["params"]["state"],
        json!({"unreadable": expected}),
        "{context}: the password entrance reports the same state"
    );
    assert_eq!(
        app.ctx.auth.ssh_challenges.outstanding(app.host.wall_ms()),
        challenges_before + 1,
        "{context}: the challenge must survive the unreadable snapshot"
    );
    assert_eq!(app.ctx.auth.consecutive_failures(), 0, "{context}");
    {
        let limiter = app.ctx.auth.limiter.lock().await;
        let tokens = limiter.as_ref().map(|l| l.available_tokens());
        assert!(
            tokens.is_none() || tokens == Some(LOGIN_QUOTA),
            "{context}: the shared bucket must stay full: {tokens:?}"
        );
    }
    assert_eq!(secrets_mutations(app), Vec::new(), "{context}");
}

#[tokio::test]
async fn an_unreadable_snapshot_keeps_its_exact_reason_and_side_effects_free() {
    let host = default_host();
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    app.host.put_text(CREDENTIALS, "not json");
    unreadable_attempt(&app, "not_json", "plain text").await;
    app.host.put_text(CREDENTIALS, r#"{"username": "adm"#);
    unreadable_attempt(&app, "not_json", "truncated body").await;
    app.host.put_text(
        CREDENTIALS,
        r#"{"username": "admin", "password_hash": "x", "must_change_password": false, "session_key": "short"}"#,
    );
    unreadable_attempt(&app, "field_invalid", "broken field").await;
}

#[tokio::test]
async fn a_refused_secrets_read_reports_file_or_directory_deterministically() {
    for (refuse_dir, expected, context) in [
        (false, "file_unreadable", "refused file"),
        (true, "directory_unreadable", "refused directory"),
    ] {
        let host = default_host();
        host.fail_read(CREDENTIALS);
        if refuse_dir {
            host.fail_read_dir("/var/lib/nvm/manager/secrets");
        }
        let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
        unreadable_attempt(&app, expected, context).await;
    }
}

#[tokio::test]
async fn a_missing_file_never_creates_credentials_and_keeps_the_challenge() {
    let host = default_host();
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    // No login status/password login call happened: there is no credentials file yet, and the SSH
    // read-only path must not create one.
    let key = make_key(30);
    let challenge = issue_challenge(&app).await;
    let (status, body, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({
            "challenge_id": challenge["challenge_id"],
            "signature": "-----BEGIN SSH SIGNATURE-----\ngarbage\n-----END SSH SIGNATURE-----\n",
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        body["params"]["state"],
        json!({"unreadable": "file_unreadable"}),
        "a missing file is an unreadable file on the SSH path"
    );
    assert_eq!(
        secrets_mutations(&app),
        Vec::new(),
        "the read-only SSH path created nothing under the secrets directory"
    );
    use truenas_nvidia_driver_manager::store::JsonRead;
    match truenas_nvidia_driver_manager::store::read_json_recovering::<Credentials>(
        &app.host,
        &Paths::new(CONTAINER_DATA_DIR).credentials(),
    ) {
        JsonRead::Missing => {}
        other => panic!("the SSH path created credentials: {other:?}"),
    }
    // The failed attempt consumed neither the challenge nor quota: with a valid registered key in
    // place, the same challenge id signs in.
    let (parsed, fingerprint) = ssh_login::parse_public_key(&key.public_line).expect("valid");
    let credentials = Credentials {
        username: "admin".into(),
        password_hash: auth::hash_password("longpass99").unwrap(),
        must_change_password: false,
        session_key: store::random_hex(32),
        ssh_keys: vec![ssh_login::SshKey {
            id: store::random_hex(8),
            public_key: ssh_login::canonical_line(&parsed),
            fingerprint,
            comment: String::new(),
            added_at: 1,
        }],
    };
    auth::write_credentials(&app.host, &Paths::new(CONTAINER_DATA_DIR), &credentials).unwrap();
    let signature = sign_armor(
        &key,
        SSH_NAMESPACE,
        challenge["challenge"].as_str().unwrap().as_bytes(),
    );
    let (status, body, headers) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({
            "challenge_id": challenge["challenge_id"],
            "signature": signature,
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.contains_key(header::SET_COOKIE));
    let (status, body) = post(
        &app,
        "/api/login",
        json!({"username": "admin", "password": "longpass99"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn the_success_log_names_only_the_verified_public_identity() {
    let sink = SharedBuf::default();
    truenas_nvidia_driver_manager::log::set_sink(Box::new(sink.clone()));
    let (app, cookie) = setup_changed().await;
    let key = make_key(31);
    let (status, _) = register_key(&app, &cookie, &key.public_line).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, _, ssh_cookie) = ssh_login_attempt(&app, &key).await;
    assert!(ssh_cookie.is_some(), "the ssh sign-in succeeded");
    // One failed attempt too, to pin the failure line's public field set: a real signature over
    // other bytes parses, so the embedded public identity is known and named.
    let challenge = issue_challenge(&app).await;
    let wrong_message = sign_armor(&key, SSH_NAMESPACE, b"not the challenge bytes at all ----");
    let (_, _, _) = request(
        &app,
        Method::POST,
        "/api/ssh/login",
        Some(json!({
            "challenge_id": challenge["challenge_id"],
            "signature": wrong_message,
        })),
        None,
    )
    .await;
    let raw = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    truenas_nvidia_driver_manager::log::set_sink(Box::new(std::io::sink()));
    let (_, expected_fingerprint) = ssh_login::parse_public_key(&key.public_line).unwrap();
    let lines: Vec<serde_json::Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).expect("log lines are JSON"))
        .collect();
    let success = lines
        .iter()
        .find(|line| {
            line["event"] == "ssh_login_succeeded" && line["fingerprint"] == expected_fingerprint
        })
        .expect("the success line names the verified fingerprint")
        .clone();
    let mut keys: Vec<&str> = success
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort();
    // The closed field set: the verified key's public identity and nothing else — no via, no
    // challenge, signature, key line or session key.
    assert_eq!(
        keys,
        vec!["algorithm", "event", "fingerprint", "level", "msg", "ts_ms"],
        "the success line carries only the public identity"
    );
    assert_eq!(success["algorithm"], "ssh-ed25519");
    assert!(success["fingerprint"]
        .as_str()
        .unwrap()
        .starts_with("SHA256:"));
    // The failure event is pinned to this test's own registered key: the process-wide sink also
    // receives the events of every concurrently running test, so neither lookup may take the first
    // line of its kind.
    let failure = lines
        .iter()
        .find(|line| {
            line["event"] == "ssh_login_failed" && line["fingerprint"] == expected_fingerprint
        })
        .expect("this test's failure line is present")
        .clone();
    let mut failure_keys: Vec<&str> = failure
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    failure_keys.sort();
    assert_eq!(
        failure_keys,
        vec![
            "algorithm",
            "event",
            "fingerprint",
            "level",
            "msg",
            "reason",
            "ts_ms"
        ],
        "the failure line carries the public identity and the reason category only"
    );
    assert_eq!(failure["reason"], "signature");
    assert_eq!(failure["algorithm"], "ssh-ed25519");
}

#[tokio::test]
async fn an_agent_reads_confirmation_submits_and_polls_verified_results() {
    for restore in [false, true] {
        let (app, admin_cookie) = signed_in_app(default_host()).await;
        prime_canonical(&app.host, "580.173.02");
        app.host.remove_mount("/usr");
        app.host
            .add_mount("tank/ROOT/default/usr", "/usr", "zfs", "ro");
        for module in ["nvidia_drm", "nvidia_modeset", "nvidia_uvm"] {
            app.host.unload_module(module);
        }
        app.host.load_module("nvidia", 0, &[]);
        let original_id = "20260928T101530Z-3f9a0c12-original";
        let original = format!("/var/lib/nvm/drivers/{original_id}");
        let original_bytes = b"driver image";
        app.host
            .put_file(&format!("{original}/nvidia.raw"), original_bytes.to_vec());
        app.host.set_listing(
            &format!("{original}/nvidia.raw"),
            &canonical_listing(KERNEL, "580.173.02"),
        );
        app.host.put_text(&format!("{original}/record.json"), &json!({
            "kind":"original", "version":"580.173.02", "sha256":sha256_hex(original_bytes),
            "size":original_bytes.len(), "target_kernel":KERNEL, "created_at":7,
            "fingerprint":null, "original_of":{"kernel":KERNEL,"truenas_version":TRUENAS_VERSION}, "keep":true
        }).to_string());
        let key = make_key(40);
        assert_eq!(
            register_key(&app, &admin_cookie, &key.public_line).await.0,
            StatusCode::CREATED
        );
        let (status, login_body, cookie) = ssh_login_attempt(&app, &key).await;
        assert_eq!(status, StatusCode::OK, "{login_body}");
        let cookie = cookie.unwrap();
        assert!(token_for(&cookie).is_some());
        let row_id = "20260928T101530Z-3f9a0c12-build";
        let bytes = b"agent requested driver image";
        let row = format!("/var/lib/nvm/drivers/{row_id}");
        app.host
            .put_file(&format!("{row}/nvidia.raw"), bytes.to_vec());
        app.host.set_listing(
            &format!("{row}/nvidia.raw"),
            &canonical_listing(KERNEL, "580.178.04"),
        );
        app.host.put_text(
            &format!("{row}/record.json"),
            &json!({
                "kind":"official", "version":"580.178.04", "sha256":sha256_hex(bytes),
                "size":bytes.len(), "target_kernel":KERNEL, "created_at":7,
                "fingerprint":null, "original_of":null, "keep":null
            })
            .to_string(),
        );
        let (status, plan) =
            post_plan(&app, &cookie, json!({"kind":"local", "row_id":row_id})).await;
        assert_eq!(status, StatusCode::OK, "{plan}");
        assert_eq!(plan["result"], "ready");
        assert_eq!(plan["dialog"]["version"], "580.178.04");
        assert_eq!(plan["dialog"]["confirmation"]["original"], "saved");
        assert_eq!(
            plan["dialog"]["confirmation"]["how"]["key"],
            "confirm.how.local"
        );
        assert_eq!(
            plan["dialog"]["confirmation"]["buttons"][0],
            json!({"button":"start", "enabled":true})
        );
        // Hold the runner before verification so the first poll observes acceptance, not
        // completion.
        let gate = app.host.set_gate("ldconfig", &[]);
        if restore {
            app.host.preset_once(
                "nvidia-smi",
                &["--query-gpu=driver_version", "--format=csv,noheader"],
                FakeOutput::stdout("0.0\n"),
            );
        }
        let (status, task) = post_task(&app, &cookie, &plan["plan_id"], "start").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        let id = task["task_id"].as_str().unwrap();
        let uri = format!("/api/tasks/{id}");
        let (status, running) = get(&app, &uri, Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "{running}");
        assert_eq!(running["status"], "RUNNING");
        gate.release();
        app.host.wait_for_units(Duration::from_secs(5));
        let after = running["log"]["next"].as_u64().unwrap();
        let (status, detail) = get(&app, &format!("{uri}?after={after}"), Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert_eq!(
            detail["status"],
            if restore {
                "STOCK_RESTORED"
            } else {
                "SUCCEEDED"
            },
            "{detail}"
        );
        let checks = detail["checks"].as_array().unwrap();
        let check = checks.last().unwrap();
        assert_eq!(check["pass"], if restore { "restore" } else { "update" });
        assert_eq!(check["passed"], true);
        assert_eq!(check["refresh_ok"], true);
        let since_before = check["since_before"]["value"].as_u64().unwrap();
        let since_after = check["since_after"]["value"].as_u64().unwrap();
        assert_ne!(since_before, since_after);
        assert_eq!(check["version_ok"], true);
        let expected = if restore { "580.173.02" } else { "580.178.04" };
        assert_eq!(check["expected"], expected);
        assert_eq!(check["nvidia_smi"]["value"], expected);
        assert_eq!(check["proc_version"]["value"], expected);
        assert!(detail["log"]["next"].as_u64().unwrap() >= after);
        if restore {
            assert_eq!(checks.len(), 2);
            assert_eq!(checks[0]["passed"], false);
        }
    }
}

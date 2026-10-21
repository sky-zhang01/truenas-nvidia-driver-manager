//! The credentials lock covers credential state only. login status reads the extension link for its
//! own response; that read is a host fact, not credential state, so it must not run under the lock:
//! a slow host read there would delay a concurrent login or password change, which need the same
//! lock. The test uses the fake host's path gate as a deterministic barrier: the next `lstat` of
//! the link blocks, login status stops inside that read, and password change must still answer
//! while login status is blocked.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;
use truenas_nvidia_driver_manager::codes::LINK_PATH;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocked_link_read_does_not_delay_a_password_change() {
    let host = default_host();
    let app = app(host.clone(), settings(), healthy_startup(&host));
    // A real sign-in first: password change needs a session cookie, and the initial credentials
    // must be in place before the barrier goes up.
    let cookie = setup_app(&app, "hunter12").await;

    // The host stalls the next `lstat` of the extension link, the way a busy runner stalls an I/O
    // inside login status. The fake host records the view-relative path, which is the host path
    // itself.
    let gate = host.set_path_gate(LINK_PATH);

    let _release_gate = ReleaseGate(gate.clone());
    let login_status_request = tokio::spawn({
        let app = app.clone();
        async move { request(&app, Method::GET, "/api/login", None, None).await.0 }
    });
    let wait_until = Instant::now() + Duration::from_secs(30);
    while !gate.blocked() {
        assert!(
            Instant::now() < wait_until,
            "login status never reached the extension-link read"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Password change must not wait for that read: the lock covers the credentials, not the
    // extension link. The gate is released before the assertion so a failure cannot leave the
    // blocked reader (and the runtime shutdown) hanging.
    let changed = tokio::time::timeout(
        Duration::from_secs(2),
        request(
            &app,
            Method::POST,
            "/api/password",
            Some(json!({"new_password": "changed-password-1"})),
            Some(&cookie),
        ),
    )
    .await;
    gate.release();
    let delayed = changed.is_err();
    assert!(
        !delayed,
        "the password change waited for the blocked extension-link read"
    );
    let (status, body, _) = changed.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");

    let status = login_status_request.await.expect("login status task");
    assert_eq!(status, StatusCode::OK);
}

#[test]
fn writes_waiting_for_the_process_lock_reject_a_logged_out_session() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(assert_waiting_writes_reject_logged_out_session());
}

async fn assert_waiting_writes_reject_logged_out_session() {
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Wake, Waker};

    #[derive(Default)]
    struct RequestWake {
        count: AtomicUsize,
        notified: tokio::sync::Notify,
    }

    impl Wake for RequestWake {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.notified.notify_one();
        }
    }

    let cases = [
        (Method::POST, "/api/uploads", None),
        (
            Method::POST,
            "/api/plans",
            Some(json!({"source": {"kind": "official", "version": "580.178.04", "gpus": []}})),
        ),
        (
            Method::POST,
            "/api/tasks",
            Some(json!({"plan_id": "0123456789abcdef0123456789abcdef", "button": "skip"})),
        ),
        (
            Method::PUT,
            "/api/drivers/20260928T101530Z-3f9a0c12-original/keep",
            Some(json!({"keep": true})),
        ),
    ];
    for (method, uri, body) in cases {
        let host = default_host();
        let app = app(host.clone(), settings(), healthy_startup(&host));
        let cookie = setup_app(&app, "hunter12").await;
        let guard = app.ctx.process_lock.lock().await;
        let mut pending = Box::pin(request(&app, method, uri, body, Some(&cookie)));
        // Fill the blocking pool before polling, so session_check's first worker must queue.
        let gates = [
            Arc::new(common::fake_host::Gate::default()),
            Arc::new(common::fake_host::Gate::default()),
        ];
        let _release_gates = gates.clone().map(ReleaseGate);
        let workers = gates
            .clone()
            .map(|gate| tokio::task::spawn_blocking(move || gate.wait_observed()));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !gates.iter().all(|gate| gate.blocked()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking pool did not reach its barriers");
        let wake = Arc::new(RequestWake::default());
        let waker = Waker::from(wake.clone());
        assert!(pending
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        // Only the queued middleware JoinHandle can wake this manually polled request.
        let observed = wake.count.load(Ordering::SeqCst);
        for gate in &gates {
            gate.release();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while wake.count.load(Ordering::SeqCst) == observed {
                wake.notified.notified().await;
            }
        })
        .await
        .expect("session middleware worker did not complete");
        for worker in workers {
            worker.await.unwrap();
        }
        // Its JoinHandle is ready, so this poll passes the authenticated middleware and the
        // in-memory body extractor, then waits on the handler worker behind process_lock.
        assert!(pending
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        let (status, body, _) =
            request(&app, Method::POST, "/api/logout", None, Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        drop(guard);
        let (status, body, _) = pending.await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}: {body}");
        assert_eq!(body["error"], "auth.required", "{uri}: {body}");
        assert!(app.ctx.plan.lock().unwrap().0.is_none(), "{uri}");
        assert_eq!(host.count("systemd-run"), 0, "{uri}");
    }
}

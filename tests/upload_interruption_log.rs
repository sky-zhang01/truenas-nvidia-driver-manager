//! An interrupted receive writes the `upload_rejected` line with `idle` or `disconnected`, and a
//! cancelled handler future cleans up and reports `disconnected`. The request bodies are controlled
//! in-process doubles (`ChannelBody`, `FaultBody`), so they do not exercise a real TCP disconnect
//! or host. The single test function resets the process-global log sink around each scenario, so no
//! other test uses it concurrently.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, StatusCode};
use common::*;
use http_body::{Body as HttpBody, Frame};
use serde_json::{json, Value};
use tower::ServiceExt;
use truenas_nvidia_driver_manager::facts::MountLine;
use truenas_nvidia_driver_manager::host::{Cmd, Create, Host, Output, SyncWrite, Watch};
use truenas_nvidia_driver_manager::log;

/// A request body that yields what its channel carries and stays pending afterwards.
struct ChannelBody {
    rx: tokio::sync::mpsc::Receiver<Bytes>,
    polled: Arc<AtomicBool>,
}

impl HttpBody for ChannelBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        self.polled.store(true, Ordering::SeqCst);
        self.rx.poll_recv(cx).map(|x| x.map(|b| Ok(Frame::data(b))))
    }
}

/// A body whose first frame is data and whose next poll is a read error.
struct FaultBody {
    first: bool,
}

impl HttpBody for FaultBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        if self.first {
            self.first = false;
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"partial upload")))))
        } else {
            Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "independent disconnected body",
            ))))
        }
    }
}

fn upload_request(cookie: &str, group: &str, body: Body, length: Option<u64>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::PUT)
        .uri(format!("/api/uploads/{group}/files?name=interruption.run"))
        .header(header::HOST, "nas.example:8199")
        .header(header::ORIGIN, "http://nas.example:8199")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::COOKIE, format!("nvm_session={cookie}"))
        .header("x-nvm-token", token_for(cookie).unwrap_or_default());
    if let Some(n) = length {
        builder = builder.header(header::CONTENT_LENGTH, n.to_string());
    }
    builder.body(body).unwrap()
}

async fn response(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// True when the sink holds the `upload_rejected` line for `reason`.
fn required_line(raw: &str, reason: &str) -> bool {
    raw.lines().any(|line| {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => return false,
        };
        value["event"] == "upload_rejected"
            && value["level"] == "INFO"
            && value["msg"] == "upload rejected"
            && value["reason"] == reason
    })
}

async fn wait_for_receive(app: &TestApp, polled: &AtomicBool) {
    for _ in 0..10_000 {
        if app.ctx.upload.receiving() && polled.load(Ordering::SeqCst) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the receive loop did not start");
}

/// The receiving flag is released by the writer thread, so it clears shortly after the receive
/// ends, together with the thread that owned the file.
async fn wait_receiving_clears(app: &TestApp) {
    // The writer is a real blocking thread, so wait on the wall clock: a loaded CI host can need
    // far more than a fixed number of yields.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.ctx.upload.receiving() {
        assert!(
            std::time::Instant::now() < deadline,
            "the receiving flag was never released"
        );
        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The log sink is process-wide and these tests run in parallel threads: a cancelled upload in one
/// test can log while another test is capturing. Every test in this file holds this lock for its
/// whole run.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(start_paused = true)]
async fn an_interrupted_receive_writes_idle_or_disconnected_and_a_cancel_cleans_up() {
    let _serial = SERIAL.lock().await;
    let host = default_host();
    prime_canonical(&host, "580.173.02");
    let app = app(host.clone(), settings("hunter12"), healthy_startup(&host));
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "changed123").await;
    let (status, body) = post(&app, "/api/uploads", json!({}), Some(&cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let group = body["group_id"].as_str().unwrap().to_string();
    let group_dir = format!("/var/lib/nvm/manager/upload/{group}");

    // 1. Idle: one frame, then no new byte for 61 seconds.
    {
        let sink = SharedBuf::default();
        log::set_sink(Box::new(sink.clone()));
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let polled = Arc::new(AtomicBool::new(false));
        tx.send(Bytes::from_static(b"partial upload"))
            .await
            .unwrap();
        let request = upload_request(
            &cookie,
            &group,
            Body::new(ChannelBody {
                rx,
                polled: polled.clone(),
            }),
            None,
        );
        let router = app.router.clone();
        let task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
        wait_for_receive(&app, &polled).await;
        assert!(!task.is_finished());
        host.advance(Duration::from_secs(61));
        tokio::time::advance(Duration::from_secs(61)).await;
        let (status, body) = response(task.await.unwrap()).await;
        log::set_sink(Box::new(std::io::stdout()));
        let raw = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "upload.interrupted", "{body}");
        wait_receiving_clears(&app).await;
        assert!(!host
            .children(&group_dir)
            .iter()
            .any(|n| n.starts_with(".incoming-")));
        assert!(required_line(&raw, "idle"), "no idle line in {raw:?}");
    }

    // 2. Disconnected: the request body reports a read error.
    {
        let sink = SharedBuf::default();
        log::set_sink(Box::new(sink.clone()));
        let request = upload_request(&cookie, &group, Body::new(FaultBody { first: true }), None);
        let (status, body) = response(app.router.clone().oneshot(request).await.unwrap()).await;
        log::set_sink(Box::new(std::io::stdout()));
        let raw = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "upload.interrupted", "{body}");
        wait_receiving_clears(&app).await;
        assert!(!host
            .children(&group_dir)
            .iter()
            .any(|n| n.starts_with(".incoming-")));
        assert!(
            required_line(&raw, "disconnected"),
            "no disconnected line in {raw:?}"
        );
    }

    // 3. A cancelled handler future: the cleanup and the `disconnected` line still happen.
    {
        let sink = SharedBuf::default();
        log::set_sink(Box::new(sink.clone()));
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let polled = Arc::new(AtomicBool::new(false));
        tx.send(Bytes::from_static(b"partial upload"))
            .await
            .unwrap();
        let request = upload_request(
            &cookie,
            &group,
            Body::new(ChannelBody {
                rx,
                polled: polled.clone(),
            }),
            None,
        );
        let router = app.router.clone();
        let task = tokio::spawn(async move {
            let _ = router.oneshot(request).await;
        });
        wait_for_receive(&app, &polled).await;
        assert!(!task.is_finished());
        task.abort();
        let _ = task.await;
        log::set_sink(Box::new(std::io::stdout()));
        let raw = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        wait_receiving_clears(&app).await;
        assert!(!host
            .children(&group_dir)
            .iter()
            .any(|n| n.starts_with(".incoming-")));
        assert!(
            required_line(&raw, "disconnected"),
            "no disconnected line for the cancelled handler in {raw:?}"
        );
        drop(tx);
    }

    // 4. A complete upload still writes no interruption line.
    {
        let sink = SharedBuf::default();
        log::set_sink(Box::new(sink.clone()));
        let (status, body) = request_raw(
            &app,
            Method::PUT,
            &format!("/api/uploads/{group}/files?name=complete.run"),
            b"#!/bin/sh\nlabel=\"NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04\"\n"
                .to_vec(),
            "application/octet-stream",
            Some(&cookie),
        )
        .await;
        log::set_sink(Box::new(std::io::stdout()));
        let raw = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            !raw.contains("upload_rejected"),
            "a complete upload wrote an interruption line: {raw:?}"
        );
    }
}

// ------------------------------------------------------------ slow disk --

/// A request body that counts every frame it handed out, so the test can see how much of the body
/// was actually consumed.
struct CountingBody {
    rx: tokio::sync::mpsc::Receiver<Bytes>,
    delivered: Arc<AtomicUsize>,
}

impl HttpBody for CountingBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        let delivered = self.delivered.clone();
        self.rx.poll_recv(cx).map(|x| {
            x.map(|b| {
                delivered.fetch_add(1, Ordering::SeqCst);
                Ok(Frame::data(b))
            })
        })
    }
}

/// A host whose created files block every write until released: the model of a disk slower than the
/// body read.
#[derive(Clone)]
struct StallingHost {
    inner: FakeHost,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl StallingHost {
    fn gated(inner: FakeHost) -> (StallingHost, Arc<(Mutex<bool>, Condvar)>) {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        (
            StallingHost {
                inner,
                gate: gate.clone(),
            },
            gate,
        )
    }

    fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
        let mut open = gate.0.lock().unwrap();
        *open = true;
        gate.1.notify_all();
    }
}

struct StallingWrite {
    file: Box<dyn SyncWrite + Send>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl Write for StallingWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut open = self.gate.0.lock().unwrap();
        while !*open {
            open = self.gate.1.wait(open).unwrap();
        }
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl SyncWrite for StallingWrite {
    fn sync_all(&mut self) -> std::io::Result<()> {
        self.file.sync_all()
    }
}

impl Host for StallingHost {
    fn run(&self, c: &Cmd) -> Output {
        self.inner.run(c)
    }
    fn run_watch(&self, c: &Cmd, e: Duration, w: &mut dyn Watch) -> Output {
        self.inner.run_watch(c, e, w)
    }
    fn view(&self, p: &Path) -> PathBuf {
        self.inner.view(p)
    }
    fn mount_table(&self) -> std::io::Result<Vec<MountLine>> {
        self.inner.mount_table()
    }
    fn read(&self, p: &Path) -> std::io::Result<Vec<u8>> {
        self.inner.read(p)
    }
    fn open(&self, p: &Path) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        self.inner.open(p)
    }
    fn create(
        &self,
        p: &Path,
        how: Create,
        mode: u32,
    ) -> std::io::Result<Box<dyn SyncWrite + Send>> {
        let file = self.inner.create(p, how, mode)?;
        // Only the received file body stalls; every other write (the group record, the credentials)
        // must let the test set itself up.
        let incoming = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with(".incoming-"))
            .unwrap_or(false);
        if incoming {
            Ok(Box::new(StallingWrite {
                file,
                gate: self.gate.clone(),
            }))
        } else {
            Ok(file)
        }
    }

    fn append(&self, p: &Path, mode: u32) -> std::io::Result<Box<dyn Write + Send>> {
        self.inner.append(p, mode)
    }
    fn lstat(&self, p: &Path) -> std::io::Result<std::fs::Metadata> {
        self.inner.lstat(p)
    }
    fn read_link(&self, p: &Path) -> std::io::Result<PathBuf> {
        self.inner.read_link(p)
    }
    fn read_dir(&self, p: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
        self.inner.read_dir(p)
    }
    fn create_dir(&self, p: &Path, mode: u32) -> std::io::Result<()> {
        self.inner.create_dir(p, mode)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.inner.rename(from, to)
    }
    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        self.inner.remove_file(p)
    }
    fn remove_dir_all(&self, p: &Path) -> std::io::Result<()> {
        self.inner.remove_dir_all(p)
    }
    fn fsync_dir(&self, p: &Path) -> std::io::Result<()> {
        self.inner.fsync_dir(p)
    }
    fn fsync_file(&self, p: &Path) -> std::io::Result<()> {
        self.inner.fsync_file(p)
    }
    fn uname_release(&self) -> std::io::Result<String> {
        self.inner.uname_release()
    }
    fn now(&self) -> std::time::Instant {
        self.inner.now()
    }
    fn wall_ms(&self) -> u64 {
        self.inner.wall_ms()
    }
}

/// A slow disk: the body read stops at the fixed queue bound instead of buffering the whole file,
/// and after a cancel the receiving flag is released only when the writer thread has really ended.
#[tokio::test]
async fn a_slow_writer_bounds_the_queue_and_holds_the_receiving_flag() {
    let _serial = SERIAL.lock().await;
    const FRAMES: usize = 400;
    let fake = default_host();
    prime_canonical(&fake, "580.173.02");
    let (stalling, gate) = StallingHost::gated(fake.clone());
    let app = app_with_host(Arc::new(stalling), fake.clone());
    let _ = get(&app, "/api/login", None).await;
    let cookie = login_changed(&app, "hunter12", "changed123").await;
    let (status, body) = post(&app, "/api/uploads", json!({}), Some(&cookie)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let group = body["group_id"].as_str().unwrap().to_string();

    let (tx, rx) = tokio::sync::mpsc::channel(FRAMES);
    let delivered = Arc::new(AtomicUsize::new(0));
    let request = upload_request(
        &cookie,
        &group,
        Body::new(CountingBody {
            rx,
            delivered: delivered.clone(),
        }),
        None,
    );
    for _ in 0..FRAMES {
        tx.send(Bytes::from(vec![b'x'; 64 * 1024])).await.unwrap();
    }
    let router = app.router.clone();
    let task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
    for _ in 0..100 {
        if delivered.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The writer stalls inside its first write, so the handler parks at the queue bound: everything
    // beyond the bounded prefix stays unread.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let taken = delivered.load(Ordering::SeqCst);
    assert!(
        taken <= 70,
        "the body read consumed {taken} of {FRAMES} frames while the disk was stalled"
    );

    // Cancelling the handler drops the body and the queue sender, but the flag stays with the
    // writer that still holds the file.
    task.abort();
    let _ = task.await;
    assert!(
        app.ctx.upload.receiving(),
        "the receiving flag must outlive the cancelled future while the writer runs"
    );
    StallingHost::release(&gate);
    wait_receiving_clears(&app).await;
    drop(tx);
}

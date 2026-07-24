//! Bridge to a local `grok agent serve` subprocess.
//!
//! Lifecycle:
//!   1. Resolve the `grok` binary on disk.
//!   2. Allocate a free loopback port.
//!   3. Generate a 12-byte hex secret.
//!   4. Spawn `grok agent serve --bind 127.0.0.1:<port> --secret <secret>`.
//!   5. Poll the TCP listener until it accepts.
//!   6. Open a WebSocket to `ws://127.0.0.1:<port>/ws?server-key=<secret>`.
//!   7. Send `initialize` to verify the connection is healthy.
//!   8. Spawn a reader task that dispatches responses to pending callers
//!      and forwards server-pushed notifications to the renderer as
//!      `agent:event` / `account:event` Tauri events.
//!
//! Wire protocol is JSON-RPC 2.0 over WebSocket text frames
//! (newline-delimited). See `crates/codegen/xai-grok-shell/src/agent/server.rs`
//! for the server-side reference.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::{SinkExt, StreamExt};
use rand::Rng;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::binary::resolve_grok;

/// JSON-RPC frame id allocator.
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

/// Default timeout for a single `call()` round-trip.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long we wait for the spawned `grok agent serve` to start accepting.
const SPAWN_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("agent not running: {0}")]
    NotRunning(String),
    #[error("json-rpc error ({code}): {message}")]
    Remote { code: i64, message: String },
    #[error("agent subprocess exited unexpectedly")]
    ChildExited,
    #[error("timeout waiting for agent")]
    Timeout,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

type PendingMap = HashMap<i64, oneshot::Sender<Result<Value, BridgeError>>>;

/// All state needed to talk to a running `grok agent serve`.
pub struct AgentBridge {
    port: u16,
    #[allow(dead_code)] // useful in logs / future IPC
    secret: String,
    write: Arc<Mutex<futures::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
    pending: Arc<Mutex<PendingMap>>,
    #[allow(dead_code)] // kept for future re-connect / log enrichment
    app: AppHandle,
}

impl AgentBridge {
    /// Spawn a new `grok agent serve` and connect to it.
    pub async fn connect(app: AppHandle) -> Result<Self> {
        let grok = resolve_grok().map_err(|e| anyhow!(e.to_string()))?;
        tracing::info!(binary = %grok.display(), "resolved grok binary");

        // 1. Kill any stale agent serve from previous runs (port conflict).
        let _ = tokio::process::Command::new("pkill")
            .args(["-f", "grok agent serve"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        tokio::time::sleep(Duration::from_millis(300)).await;

        // 2. Allocate a free loopback port.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind 127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);

        // 2. Generate the secret.
        let secret = generate_secret();
        tracing::info!(port, "allocated loopback port for agent serve");

        // 3. Spawn the subprocess.
        let mut cmd = tokio::process::Command::new(grok);
        cmd.arg("agent")
            .arg("serve")
            .arg("--bind")
            .arg(format!("127.0.0.1:{}", port))
            .arg("--secret")
            .arg(&secret);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn().context("spawn grok agent serve")?;

        // 4. Poll for readiness.
        wait_for_listen(port, SPAWN_WAIT).await?;
        tracing::info!(port, "agent serve is accepting connections");

        // 5. Open WebSocket with the secret as a query param
        //    (server falls back to ?server-key= when no Authorization
        //    header is present; see xai-grok-shell/src/agent/server.rs:94-107).
        let url = format!("ws://127.0.0.1:{}/ws?server-key={}", port, secret);
        let (ws, _resp) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("connect to {}", url))?;
        let (mut write, mut read) = ws.split();

        // 6. Send `initialize` and wait for the response before unlocking.
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let init_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let init_frame = json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientInfo": {"name": "grok-build-desktop-tauri", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {}
            }
        });
        let init_text = serde_json::to_string(&init_frame)?;
        write
            .send(Message::Text(init_text.into()))
            .await
            .context("send initialize")?;

        // Wait for the initialize response with a short timeout.
        //
        // Loop because `tokio-tungstenite 0.24` does NOT auto-reply to
        // Ping frames; `agent serve` often opens with an empty Ping as a
        // keepalive handshake. We must (a) skip past Pings/Pongs/Close
        // noise, (b) reply Pong to any Ping so the server doesn't time
        // us out, and (c) parse the next Text frame as the initialize
        // response.
        let init_text: String = loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), read.next())
                .await
                .map_err(|_| BridgeError::Timeout)?
                .ok_or_else(|| {
                    anyhow!("ws stream closed before initialize response")
                })?
                .map_err(|e| BridgeError::Other(anyhow!(e.to_string())))?;
            match msg {
                Message::Text(t) => break t.to_string(),
                Message::Ping(payload) => {
                    // Reply with Pong so the server keeps the connection open.
                    let _ = write.send(Message::Pong(payload)).await;
                    continue;
                }
                Message::Pong(_) => continue,
                Message::Close(c) => {
                    return Err(anyhow!("ws closed during init: {:?}", c));
                }
                Message::Binary(_) | Message::Frame(_) => {
                    tracing::warn!("ignoring non-text frame during init");
                    continue;
                }
            }
        };
        let v: Value = serde_json::from_str(&init_text)
            .map_err(|e| BridgeError::Other(anyhow!(e.to_string())))?;
        if let Some(err) = v.get("error") {
            return Err(anyhow!("initialize rejected: {}", err));
        }
        tracing::info!("agent serve initialized successfully");

        // 7. Spawn the reader task.
        let pending_for_reader = pending.clone();
        let app_for_reader = app.clone();
        tokio::spawn(async move {
            run_reader(read, pending_for_reader, app_for_reader).await;
        });

        // 8. Spawn the child watcher.
        let mut child_for_watcher = child;
        let app_for_watcher = app.clone();
        tokio::spawn(async move {
            if let Ok(status) = child_for_watcher.wait().await {
                tracing::warn!(?status, "agent serve subprocess exited");
                let _ = app_for_watcher.emit(
                    "agent:event",
                    json!({"type": "agent-exited", "status": format!("{:?}", status)}),
                );
            }
        });

        Ok(Self {
            port,
            secret,
            write: Arc::new(Mutex::new(write)),
            pending,
            app,
        })
    }

    /// Send a JSON-RPC request and await the response.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, BridgeError> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let text = serde_json::to_string(&frame)
            .map_err(|e| BridgeError::Other(anyhow!(e.to_string())))?;

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        {
            let mut write = self.write.lock().await;
            write
                .send(Message::Text(text.into()))
                .await
                .map_err(|e| BridgeError::Other(anyhow!(e.to_string())))?;
        }

        let resp = tokio::time::timeout(CALL_TIMEOUT, rx)
            .await
            .map_err(|_| BridgeError::Timeout)?
            .map_err(|_| BridgeError::NotRunning("response channel dropped".into()))??;

        Ok(resp)
    }

    /// Best-effort shutdown. The subprocess is owned by the watcher
    /// task spawned in `connect()`; killing it from here requires a
    /// channel. For v1 this is a no-op — exiting the Tauri window
    /// drops the process group thanks to `kill_on_drop(true)`.
    pub async fn shutdown(&self) {
        // No-op for now; see note above.
    }

    #[allow(dead_code)]
    pub fn port(&self) -> u16 {
        self.port
    }
}

fn generate_secret() -> String {
    let bytes: [u8; 12] = rand::thread_rng().gen();
    hex_encode(&bytes)
}

fn hex_encode(b: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(b.len() * 2);
    for &x in b {
        out.push(HEX[(x >> 4) as usize] as char);
        out.push(HEX[(x & 0x0f) as usize] as char);
    }
    out
}

/// Poll the loopback port until it accepts connections, or time out.
async fn wait_for_listen(port: u16, budget: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!("agent serve did not start listening within {:?}", budget));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Background reader: routes responses to pending callers and emits
/// server-pushed notifications as Tauri events.
async fn run_reader(
    mut read: futures::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    pending: Arc<Mutex<PendingMap>>,
    app: AppHandle,
) {
    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "agent ws read error");
                // Fail all pending callers.
                let mut p = pending.lock().await;
                for (_, tx) in p.drain() {
                    let _ = tx.send(Err(BridgeError::Other(anyhow!(e.to_string()))));
                }
                break;
            }
        };

        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(c) => {
                tracing::info!(?c, "agent ws closed by server");
                let mut p = pending.lock().await;
                for (_, tx) in p.drain() {
                    let _ = tx.send(Err(BridgeError::NotRunning("ws closed".into())));
                }
                break;
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            other => {
                tracing::warn!(?other, "ignoring non-text ws message");
                continue;
            }
        };

        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, %text, "ignoring non-json ws frame");
                continue;
            }
        };

        // Response frame (has id, no method).
        if let Some(id) = v.get("id").and_then(|x| x.as_i64()) {
            let result = if let Some(err) = v.get("error") {
                let code = err.get("code").and_then(|x| x.as_i64()).unwrap_or(-1);
                let message = err
                    .get("message")
                    .and_then(|x| x.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                Err(BridgeError::Remote { code, message })
            } else {
                Ok(v.get("result").cloned().unwrap_or(Value::Null))
            };
            let mut p = pending.lock().await;
            if let Some(tx) = p.remove(&id) {
                let _ = tx.send(result);
            }
            continue;
        }

        // Notification (has method, no id) — forward to renderer.
        if let Some(method) = v.get("method").and_then(|x| x.as_str()) {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            tracing::debug!(method, "forwarding agent notification");
            // The original Electron main routes `account:*` events on a
            // separate channel; we use the same "account:event" name so
            // SettingsView.tsx's `onAccountEvent` subscription still fires.
            let event = if method.starts_with("account/") || method.contains("account") {
                "account:event"
            } else {
                "agent:event"
            };
            let payload = json!({"method": method, "params": params});
            if let Err(e) = app.emit(event, payload) {
                tracing::warn!(error = %e, "failed to emit agent event");
            }
            continue;
        }

        tracing::warn!(?v, "unrecognized ws frame shape");
    }

    tracing::warn!("agent ws reader exited");
}
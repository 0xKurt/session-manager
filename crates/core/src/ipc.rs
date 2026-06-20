//! Local-only IPC over a Unix domain socket.
//!
//! Solves: with the supervisor running inside the GUI process, the CLI
//! used to fail to acquire the singleton lock. Now the GUI's supervisor
//! also serves a UDS at `state_dir/supervisor.sock`, and the CLI is a
//! client. The lock + the socket move together: if the socket exists,
//! something is holding the lock; if it's missing, no supervisor is up.
//!
//! Protocol — newline-delimited JSON, one frame per line.
//! Request:  `{"id":"<rid>","cmd":"<verb>","args":{...}}`
//! Response: `{"id":"<rid>","ok":true,"data":...}` or
//!           `{"id":"<rid>","ok":false,"error":"..."}`
//!
//! Subscribe: `{"id":"<rid>","cmd":"subscribe"}` returns the ack, then a
//! stream of `{"event":<CoreEvent>}` lines until the client disconnects.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, warn};

use crate::backend::BackendInfo;
use crate::config::AppPreferences;
use crate::events::CoreEvent;
use crate::state::{RuntimeState, SessionRuntime};
use crate::{Error, Result, SessionConfig, SessionsFile, Supervisor};

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub cmd: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub file: SessionsFile,
    pub runtime: RuntimeState,
}

/// Start the server. Returns once the listener is bound; serves indefinitely
/// in the background.
pub async fn serve(supervisor: Arc<Supervisor>, socket_path: PathBuf) -> Result<()> {
    if socket_path.exists() {
        // Stale socket from a previous run — safe to remove because we hold
        // the file lock that gates supervisor lifetime.
        let _ = std::fs::remove_file(&socket_path);
    }
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| Error::Other(format!("bind {}: {e}", socket_path.display())))?;
    // 0600 — same-user only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));
    }

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let sup = Arc::clone(&supervisor);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(sup, stream).await {
                            debug!("ipc connection ended: {e}");
                        }
                    });
                }
                Err(e) => {
                    warn!("ipc accept: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
    });
    Ok(())
}

async fn handle_connection(sup: Arc<Supervisor>, stream: UnixStream) -> Result<()> {
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response {
                    id: String::new(),
                    ok: false,
                    data: None,
                    error: Some(format!("malformed request: {e}")),
                };
                write_json(&mut wr, &resp).await?;
                continue;
            }
        };
        if req.cmd == "subscribe" {
            // Subscribe takes over the connection — after the ack we only
            // write events, never read further requests. We merge both the
            // critical event channel and the log channel; the writer
            // returns when the peer disconnects or both channels close.
            write_json(
                &mut wr,
                &Response {
                    id: req.id.clone(),
                    ok: true,
                    data: None,
                    error: None,
                },
            )
            .await?;
            let mut critical_rx = sup.subscribe();
            let mut logs_rx = sup.subscribe_logs();
            loop {
                let ev = tokio::select! {
                    r = critical_rx.recv() => r,
                    r = logs_rx.recv() => r,
                };
                match ev {
                    Ok(ev) => {
                        let payload = serde_json::json!({ "event": ev });
                        if write_line(&mut wr, &payload.to_string()).await.is_err() {
                            return Ok(());
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
        let resp = dispatch(&sup, req).await;
        if write_json(&mut wr, &resp).await.is_err() {
            break;
        }
    }
    Ok(())
}

async fn dispatch(sup: &Arc<Supervisor>, req: Request) -> Response {
    let rid = req.id.clone();
    let result = dispatch_inner(sup, &req).await;
    match result {
        Ok(v) => Response {
            id: rid,
            ok: true,
            data: Some(v),
            error: None,
        },
        Err(e) => Response {
            id: rid,
            ok: false,
            data: None,
            error: Some(e.to_string()),
        },
    }
}

async fn dispatch_inner(sup: &Arc<Supervisor>, req: &Request) -> Result<Value> {
    match req.cmd.as_str() {
        "snapshot" | "list" => {
            let (file, runtime) = sup.snapshot().await;
            Ok(serde_json::to_value(Snapshot { file, runtime })?)
        }
        "session_runtime" => {
            let id = arg_str(&req.args, "id")?;
            let (_, runtime) = sup.snapshot().await;
            let v: Option<SessionRuntime> = runtime.sessions.get(&id).cloned();
            Ok(serde_json::to_value(v)?)
        }
        "start" => {
            let id = arg_str(&req.args, "id")?;
            sup.start_session(&id).await?;
            Ok(Value::Null)
        }
        "stop" => {
            let id = arg_str(&req.args, "id")?;
            sup.stop_session(&id).await?;
            Ok(Value::Null)
        }
        "restart" => {
            let id = arg_str(&req.args, "id")?;
            sup.restart_session(&id).await?;
            Ok(Value::Null)
        }
        "stop_all" => {
            sup.stop_all().await?;
            Ok(Value::Null)
        }
        "reset_and_retry" => {
            let id = arg_str(&req.args, "id")?;
            sup.reset_and_retry(&id).await?;
            Ok(Value::Null)
        }
        "create" => {
            let session: SessionConfig =
                serde_json::from_value(req.args.get("session").cloned().unwrap_or(Value::Null))?;
            sup.create_session(session).await?;
            Ok(Value::Null)
        }
        "update" => {
            let session: SessionConfig =
                serde_json::from_value(req.args.get("session").cloned().unwrap_or(Value::Null))?;
            sup.update_session(session).await?;
            Ok(Value::Null)
        }
        "delete" => {
            let id = arg_str(&req.args, "id")?;
            sup.delete_session(&id).await?;
            Ok(Value::Null)
        }
        "registry" => {
            let v: Vec<BackendInfo> = sup.registry();
            Ok(serde_json::to_value(v)?)
        }
        "auth_states" => {
            let v = sup.auth_states().await;
            Ok(serde_json::to_value(v)?)
        }
        "external_sessions" => {
            let v = sup.external_sessions().await;
            Ok(serde_json::to_value(v)?)
        }
        "stop_external" => {
            let pid = req
                .args
                .get("pid")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| Error::Other("missing arg: pid".into()))?;
            sup.stop_external(pid as u32)?;
            Ok(Value::Null)
        }
        "adopt_external" => {
            let pid = req
                .args
                .get("pid")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| Error::Other("missing arg: pid".into()))?;
            let session: SessionConfig =
                serde_json::from_value(req.args.get("session").cloned().unwrap_or(Value::Null))?;
            sup.adopt_external(pid as u32, session).await?;
            Ok(Value::Null)
        }
        "config_path" => Ok(Value::String(
            sup.config_path().to_string_lossy().into_owned(),
        )),
        "update_preferences" => {
            let prefs: AppPreferences =
                serde_json::from_value(req.args.get("prefs").cloned().unwrap_or(Value::Null))?;
            sup.update_preferences(|p| *p = prefs).await?;
            let (file, _) = sup.snapshot().await;
            Ok(serde_json::to_value(file.preferences)?)
        }
        other => Err(Error::Other(format!("unknown command: {other}"))),
    }
}

fn arg_str(v: &Value, key: &str) -> Result<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Other(format!("missing arg: {key}")))
}

async fn write_json<T: Serialize>(wr: &mut (impl AsyncWriteExt + Unpin), v: &T) -> Result<()> {
    let s = serde_json::to_string(v)?;
    write_line(wr, &s).await
}

async fn write_line(wr: &mut (impl AsyncWriteExt + Unpin), s: &str) -> Result<()> {
    wr.write_all(s.as_bytes()).await?;
    wr.write_all(b"\n").await?;
    wr.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

pub struct Client {
    stream: tokio::io::BufStream<UnixStream>,
    seq: u64,
}

impl Client {
    /// Attempt to connect to a running supervisor's IPC socket. Returns
    /// `Ok(None)` when no socket exists (no supervisor running). Errors
    /// out for real connection failures.
    pub async fn try_connect(socket_path: &Path) -> Result<Option<Self>> {
        if !socket_path.exists() {
            return Ok(None);
        }
        match UnixStream::connect(socket_path).await {
            Ok(stream) => Ok(Some(Self {
                stream: tokio::io::BufStream::new(stream),
                seq: 0,
            })),
            Err(e)
                if e.kind() == std::io::ErrorKind::ConnectionRefused
                    || e.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(e) => Err(Error::Other(format!(
                "connect {}: {e}",
                socket_path.display()
            ))),
        }
    }

    /// Send a request, wait for the matching response.
    pub async fn call<T: for<'de> Deserialize<'de>>(
        &mut self,
        cmd: &str,
        args: Value,
    ) -> Result<T> {
        self.seq += 1;
        let id = format!("r{}", self.seq);
        let req = Request {
            id: id.clone(),
            cmd: cmd.into(),
            args,
        };
        let line = serde_json::to_string(&req)?;
        self.stream.write_all(line.as_bytes()).await?;
        self.stream.write_all(b"\n").await?;
        self.stream.flush().await?;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.stream.read_line(&mut buf).await?;
            if n == 0 {
                return Err(Error::Other("supervisor closed the connection".into()));
            }
            let resp: Response = serde_json::from_str(buf.trim_end())
                .map_err(|e| Error::Other(format!("bad response: {e}: {buf:?}")))?;
            if resp.id != id {
                // Possibly an event from a previous subscription — ignore.
                continue;
            }
            if !resp.ok {
                return Err(Error::Other(
                    resp.error.unwrap_or_else(|| "request failed".into()),
                ));
            }
            let value = resp.data.unwrap_or(Value::Null);
            return serde_json::from_value(value)
                .map_err(|e| Error::Other(format!("bad response payload: {e}")));
        }
    }

    /// `serve` subscribes to the event stream and reads frames one at a time.
    /// Returns when the connection closes.
    pub async fn subscribe(&mut self) -> Result<()> {
        let _: Value = self.call("subscribe", Value::Null).await?;
        Ok(())
    }

    pub async fn next_event(&mut self) -> Result<Option<CoreEvent>> {
        let mut buf = String::new();
        let n = self.stream.read_line(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        let v: Value = serde_json::from_str(buf.trim_end())?;
        if let Some(ev_v) = v.get("event") {
            let ev: CoreEvent = serde_json::from_value(ev_v.clone())?;
            return Ok(Some(ev));
        }
        Ok(None)
    }
}

pub fn socket_path() -> Result<PathBuf> {
    Ok(crate::paths::state_dir()?.join("supervisor.sock"))
}

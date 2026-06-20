//! Long-lived background actor that owns child processes (§7.2).
//!
//! Lives independently of the UI. UI and CLI talk to a [`Supervisor`] handle;
//! all state lives behind a single async lock so the public surface stays
//! straightforward.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use std::sync::OnceLock;

use chrono::Utc;
use fs2::FileExt;
use parking_lot::Mutex;
use regex::Regex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Notify};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::backend::{make_backend, registry, ActivityProbe, AuthState, BackendInfo, LaunchSpec};
use crate::config::SessionsFile;
use crate::events::{CoreEvent, SessionStatus};
use crate::os::{default_layer, KeepAwakeToken, OsLayer, SleepEvent};
use crate::state::{RuntimeState, SessionRuntime};
use crate::{paths, Error, Result, SessionConfig};

/// Critical event channel buffer — sized for occasional bursts of status
/// changes / permission prompts. Log lines have their own (larger) channel
/// so a torrent of agent output can't push a `NeedsPermission` out of the
/// queue before the UI sees it.
const EVENT_BUFFER: usize = 512;
/// Log-line channel buffer. Verbose agents print 100s of lines/s; a
/// slow subscriber dropping log lines is acceptable, dropping permission
/// prompts is not.
const LOG_BUFFER: usize = 4096;
/// How long between activity probes per session.
const PROBE_PERIOD: Duration = Duration::from_secs(3);
/// Cap restart backoff at 1 minute.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Graceful-stop deadline before we send SIGKILL.
const GRACEFUL_STOP_DEADLINE: Duration = Duration::from_secs(5);
/// How long stop_session waits for the worker to confirm shutdown.
const STOP_ACK_DEADLINE: Duration = Duration::from_secs(15);

pub struct Supervisor {
    inner: Arc<SupervisorInner>,
}

pub(crate) struct SupervisorInner {
    pub(crate) config_path: PathBuf,
    pub(crate) state_path: PathBuf,
    #[allow(dead_code)]
    lock_path: PathBuf,
    /// Held for the lifetime of the supervisor. Dropped on `shutdown()`,
    /// which releases the underlying advisory `flock`. We do this with
    /// `fs2` so the lock is auto-released on any process exit (including
    /// a kill -9), and there's no PID race with stale lockfiles.
    _lock_file: Mutex<Option<File>>,
    pub(crate) os: Box<dyn OsLayer>,
    pub(crate) events_tx: broadcast::Sender<CoreEvent>,
    pub(crate) logs_tx: broadcast::Sender<CoreEvent>,
    pub(crate) shared: tokio::sync::Mutex<SupervisorState>,
    keep_awake_token: Mutex<Option<Box<dyn KeepAwakeToken>>>,
    pub(crate) notify_reconcile: Notify,
}

pub(crate) struct SupervisorState {
    pub(crate) file: SessionsFile,
    pub(crate) runtime: RuntimeState,
    pub(crate) workers: HashMap<String, SessionWorker>,
    /// Sessions the user stopped during *this* supervisor lifetime. NOT
    /// persisted — reboot resets this so auto_restart sessions come back
    /// (§6 "Fleet at login").
    intentionally_stopped: HashSet<String>,
    /// Last resolved agent binary path per backend. We compare against this
    /// on the upgrade-watcher tick; a mismatch (e.g. Homebrew's
    /// Caskroom path embeds the version) emits `BinaryUpgraded`.
    binary_paths: HashMap<String, PathBuf>,
}

pub(crate) struct SessionWorker {
    handle: JoinHandle<()>,
    control: mpsc::UnboundedSender<WorkerCmd>,
}

#[derive(Debug)]
enum WorkerCmd {
    Stop {
        user_initiated: bool,
        ack: oneshot::Sender<()>,
    },
}

impl Supervisor {
    /// Construct, load config + runtime state from disk, return a handle.
    pub fn open() -> Result<Self> {
        paths::ensure_dirs()?;
        let config_path = paths::config_file()?;
        let state_path = paths::state_file()?;
        let lock_path = paths::state_dir()?.join("supervisor.lock");

        // Single-supervisor lock. Refuses to start if another live process
        // already holds the lock for this config. fs2's advisory `flock` is
        // automatically released by the OS on process exit — no stale PID
        // file to clean up, no risk of two GUIs talking to the same config.
        let lock_file = acquire_singleton_lock(&lock_path)?;

        let file = SessionsFile::load(&config_path)?;
        let mut runtime = RuntimeState::load_or_default(&state_path);

        // After a clean shutdown / reboot, treat all running/idle/etc as
        // "offline" so the reconciler restarts any auto_restart session. The
        // user's persisted Stopped marker stays Stopped, but `intentionally_stopped`
        // is empty after open so the next reconcile won't suppress those
        // sessions either — §6 "Fleet at login".
        for r in runtime.sessions.values_mut() {
            if matches!(
                r.status,
                SessionStatus::Starting
                    | SessionStatus::Working
                    | SessionStatus::NeedsPermission
                    | SessionStatus::Idle
                    | SessionStatus::RateLimited
            ) {
                r.status = SessionStatus::Offline;
                r.pid = None;
            }
        }
        runtime.keep_awake_active = false;
        let _ = runtime.save(&state_path);

        let (events_tx, _) = broadcast::channel(EVENT_BUFFER);
        let (logs_tx, _) = broadcast::channel(LOG_BUFFER);

        Ok(Self {
            inner: Arc::new(SupervisorInner {
                config_path,
                state_path,
                lock_path,
                _lock_file: Mutex::new(Some(lock_file)),
                os: default_layer(),
                events_tx,
                logs_tx,
                shared: tokio::sync::Mutex::new(SupervisorState {
                    file,
                    runtime,
                    workers: HashMap::new(),
                    intentionally_stopped: HashSet::new(),
                    binary_paths: HashMap::new(),
                }),
                keep_awake_token: Mutex::new(None),
                notify_reconcile: Notify::new(),
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.inner.events_tx.subscribe()
    }

    /// Subscribe to the higher-volume log-line channel. Separate from the
    /// critical event channel so verbose agent output can't starve
    /// `NeedsPermission` / `StatusChanged` updates.
    pub fn subscribe_logs(&self) -> broadcast::Receiver<CoreEvent> {
        self.inner.logs_tx.subscribe()
    }

    pub async fn snapshot(&self) -> (SessionsFile, RuntimeState) {
        let g = self.inner.shared.lock().await;
        (g.file.clone(), g.runtime.clone())
    }

    /// Collect agent processes the user has launched outside Session Manager
    /// (i.e. directly from a terminal). Annotates each with the matching
    /// `id` from `sessions.toml` when the cwd is the same.
    ///
    /// The scan shells out to `ps` and `lsof` synchronously, so we run it on
    /// the blocking pool — otherwise we'd pin a tokio worker thread for
    /// hundreds of milliseconds every time the UI ticks.
    pub async fn external_sessions(&self) -> Vec<crate::backend::DiscoveredSession> {
        let (file, runtime) = self.snapshot().await;
        let managed_pids: std::collections::HashSet<u32> =
            runtime.sessions.values().filter_map(|r| r.pid).collect();
        let our_pid = std::process::id();

        let raw = tokio::task::spawn_blocking(|| {
            let mut all = Vec::new();
            for b in registry() {
                if let Ok(backend) = make_backend(&b.id) {
                    all.extend(backend.discover_external());
                }
            }
            all
        })
        .await
        .unwrap_or_default();

        let mut all = raw;
        // Filter ourselves (and anything from our process tree) — this
        // catches the release-bundle case too, where the binary path
        // doesn't contain "/target/".
        all.retain(|d| d.pid != our_pid && !managed_pids.contains(&d.pid));

        // Dedup script-wrapper + child by (backend, name, cwd), keeping
        // the lowest PID. The wrapper is what the user typed, so stopping
        // it also kills the child.
        all.sort_by_key(|a| a.pid);
        let mut seen: std::collections::HashSet<(String, String, std::path::PathBuf)> =
            std::collections::HashSet::new();
        all.retain(|d| {
            let key = (d.backend_id.clone(), d.display_name.clone(), d.cwd.clone());
            seen.insert(key)
        });

        // Annotate with any matching managed-session id (by cwd canonicalisation).
        for d in &mut all {
            if let Ok(canon) = d.cwd.canonicalize() {
                for s in &file.sessions {
                    if let Ok(rp) = s.resolved_path() {
                        if rp.canonicalize().ok().as_ref() == Some(&canon) {
                            d.matches_session_id = Some(s.id.clone());
                            break;
                        }
                    }
                }
            }
        }

        // Live activity probe per external row — JSONL probe works the
        // same regardless of who started the process. Run concurrently
        // because each probe shells into `~/.claude/projects/...` and
        // reads the tail of a (potentially several-MB) JSONL.
        let probes = all.iter().map(|d| {
            let backend_id = d.backend_id.clone();
            let cwd = d.cwd.clone();
            async move {
                let backend = make_backend(&backend_id).ok()?;
                let stub = SessionConfig {
                    id: String::new(),
                    name: String::new(),
                    agent: backend_id,
                    path: cwd.to_string_lossy().into_owned(),
                    remote: false,
                    permission: Default::default(),
                    resume: Default::default(),
                    resume_id: String::new(),
                    model: String::new(),
                    keep_awake: false,
                    auto_restart: false,
                    restart_max: 0,
                    env: Default::default(),
                    log_path: None,
                    group: None,
                    record_stdout: false,
                    extra_args: Vec::new(),
                };
                backend.probe_activity(&stub).await.ok()
            }
        });
        let probe_results: Vec<Option<crate::backend::ActivityProbe>> =
            futures_join_all(probes).await;
        for (d, probe) in all.iter_mut().zip(probe_results) {
            if let Some(probe) = probe {
                d.status = probe.status;
                d.last_activity = probe.last_activity;
            }
        }
        all
    }

    /// Adopt an external session: SIGTERM the running process, wait, then
    /// create the managed config and start a fresh worker. The user gets
    /// a managed session pointing at the same project dir; Claude's
    /// `--continue` picks up where the original left off.
    pub async fn adopt_external(&self, pid: u32, config: SessionConfig) -> Result<()> {
        // 1. Stop the external first so we don't end up with two `claude`
        //    processes fighting over the same project dir.
        self.stop_external(pid)?;
        // 2. Give the process a moment to exit so its JSONL is unlocked.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        // 3. Create the managed config. If a session with the same id
        //    already exists, surface the error rather than silently
        //    clobbering.
        self.create_session(config.clone()).await?;
        // 4. Start it.
        self.start_session(&config.id).await?;
        Ok(())
    }

    /// Claim an external session **without killing it**. The running
    /// process keeps its current state; we just persist the config, mark
    /// the external PID as the runtime's current PID, and start a
    /// probe-only worker that tracks the JSONL + watches the PID for
    /// death. If the external dies (user closes shell, machine
    /// reboots, OOM) and `auto_restart` is on, the worker transitions
    /// into a normal spawn cycle — the supervisor "takes over" at that
    /// point. This is the gentler counterpart to `adopt_external`.
    pub async fn claim_external(&self, pid: u32, config: SessionConfig) -> Result<()> {
        #[cfg(unix)]
        {
            // Verify the PID still looks like the same agent — same guard
            // as stop_external. PIDs are reused and a stale row from 8s
            // ago might already point at an unrelated process.
            if !pid_looks_like_agent(pid) {
                return Err(Error::Other(format!(
                    "PID {pid} no longer looks like an agent; re-scan the External list."
                )));
            }
        }
        // Reject duplicate id BEFORE we touch any state.
        {
            let g = self.inner.shared.lock().await;
            if g.file.find(&config.id).is_some() {
                return Err(Error::SessionExists(config.id.clone()));
            }
        }
        // 1. **Insert the claimed worker FIRST** so the workers map is
        //    populated before the reconciler can race in. If we called
        //    create_session first, ConfigReloaded → notify_reconcile fires
        //    and the reconciler sees auto_restart=true + no worker entry
        //    → spawns a real worker_loop and exec's a duplicate `claude`
        //    in the same cwd, defeating the "claim without killing"
        //    contract entirely.
        //
        //    Set runtime PID synchronously in the same critical section
        //    so the next UI refreshExternal tick already filters the
        //    external row by managed_pids.
        {
            let mut g = self.inner.shared.lock().await;
            let r = g.runtime.entry_mut(&config.id);
            r.pid = Some(pid);
            // External processes are already past Starting — they're live
            // and serving the user. The probe will refine to Working/Idle
            // when JSONL data lands, but until then Idle is the correct
            // "alive and waiting" state (not Starting, which would stick
            // for the no-JSONL case).
            r.status = SessionStatus::Idle;
            r.reason = Some("claimed".into());
            r.started_at = Some(Utc::now());
            let _ = g.runtime.save(&self.inner.state_path);
        }
        emit(
            &self.inner,
            CoreEvent::StatusChanged {
                session_id: config.id.clone(),
                status: SessionStatus::Idle,
                reason: Some("claimed".into()),
            },
        );
        spawn_claimed_worker(Arc::clone(&self.inner), config.clone(), pid).await?;
        // 2. NOW persist the config. Reconciler may wake from the
        //    notify_one but sees workers.contains_key(id)=true → skips.
        self.create_session_no_emit(config).await?;
        Ok(())
    }

    /// `create_session` minus the ConfigReloaded emit + reconcile notify.
    /// Used by the claim path where we *don't* want to wake the reconciler
    /// (we've already set up the right worker ourselves). Callers that DO
    /// want reconcile to consider the new session should use
    /// [`create_session`] instead.
    async fn create_session_no_emit(&self, session: SessionConfig) -> Result<()> {
        let mut g = self.inner.shared.lock().await;
        if g.file.find(&session.id).is_some() {
            return Err(Error::SessionExists(session.id));
        }
        g.intentionally_stopped.remove(&session.id);
        g.file.upsert(session);
        g.file.save(&self.inner.config_path)?;
        // No emit, no notify_reconcile — the caller has already wired up
        // the worker for this session and a reconcile pass would be a
        // no-op race waiting to happen.
        Ok(())
    }

    /// Send SIGTERM to an external session by PID. We deliberately do not
    /// adopt these — they were started by another shell and don't have a
    /// SessionWorker — so the user stays the owner of the process.
    ///
    /// Re-verifies before killing: PIDs are reused, and a row that was
    /// fresh 8 s ago might now be an unrelated process. We only signal
    /// when the live command line still looks like an agent.
    pub fn stop_external(&self, pid: u32) -> Result<()> {
        #[cfg(unix)]
        {
            if !pid_looks_like_agent(pid) {
                return Err(Error::Other(format!(
                    "refusing to kill PID {pid}: command no longer looks like an agent \
                     (process may have been recycled). Re-scan first."
                )));
            }
            let r = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            if r != 0 {
                return Err(Error::Other(format!(
                    "kill({pid}) failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            return Err(Error::Other(
                "stop_external is unix-only in this build".into(),
            ));
        }
        Ok(())
    }

    pub async fn auth_states(&self) -> HashMap<String, AuthState> {
        let mut out = HashMap::new();
        for b in registry() {
            if let Ok(backend) = make_backend(&b.id) {
                let state = backend.auth_state().await.unwrap_or(AuthState::Unknown);
                out.insert(b.id, state);
            }
        }
        out
    }

    pub fn registry(&self) -> Vec<BackendInfo> {
        registry()
    }

    /// Begin supervising. Takes `Arc<Self>` so we can spawn the IPC server.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        // Sleep / wake watcher.
        let (sleep_tx, mut sleep_rx) = mpsc::unbounded_channel::<SleepEvent>();
        if let Err(e) = self.inner.os.watch_sleep_events(sleep_tx) {
            warn!("sleep watcher: {e}");
        }
        let inner_for_sleep = Arc::clone(&self.inner);
        tokio::spawn(async move {
            while let Some(ev) = sleep_rx.recv().await {
                match ev {
                    SleepEvent::WillSleep => {
                        // Don't fan out to "every session is offline" — sleep
                        // is system-wide; we'll re-probe on wake.
                        info!("system entering sleep");
                    }
                    SleepEvent::DidWake => {
                        inner_for_sleep.notify_reconcile.notify_one();
                    }
                }
            }
        });

        spawn_config_watcher(Arc::clone(&self.inner));

        // Reconciler loop.
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            loop {
                if let Err(e) = reconcile(&inner).await {
                    error!("reconcile: {e}");
                }
                tokio::select! {
                    _ = inner.notify_reconcile.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {},
                }
            }
        });

        self.inner.notify_reconcile.notify_one();

        // Binary-upgrade watcher: re-resolves each backend's binary every
        // 60s. Homebrew's Caskroom embeds the version in the path
        // (`/opt/homebrew/Caskroom/claude-code/<version>/claude`), so any
        // upgrade flips the path. We emit `BinaryUpgraded` so the UI can
        // offer "Restart all".
        let inner_upgrades = Arc::clone(&self.inner);
        tokio::spawn(async move {
            // Prime the map before any comparisons so we don't false-fire
            // on first tick.
            check_binary_versions(&inner_upgrades, false).await;
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                check_binary_versions(&inner_upgrades, true).await;
            }
        });

        // IPC server (Unix only). Lets the CLI talk to the running
        // supervisor instead of fighting the singleton lock.
        #[cfg(unix)]
        {
            match crate::ipc::socket_path() {
                Ok(p) => {
                    if let Err(e) = crate::ipc::serve(Arc::clone(self), p).await {
                        warn!("ipc server: {e}");
                    }
                }
                Err(e) => warn!("ipc socket path: {e}"),
            }
        }

        Ok(())
    }

    /// Stop a session and wait until the worker has acknowledged the kill
    /// (or the deadline elapses). Never `abort()`s — that would orphan the
    /// child (§8.1 "Never orphan a child").
    pub async fn stop_session(&self, id: &str) -> Result<()> {
        let worker = {
            let mut g = self.inner.shared.lock().await;
            g.intentionally_stopped.insert(id.to_string());
            g.workers.remove(id)
        };
        let Some(worker) = worker else {
            // Already stopped — just update the runtime status if needed.
            let mut g = self.inner.shared.lock().await;
            let entry = g.runtime.entry_mut(id);
            entry.status = SessionStatus::Stopped;
            entry.pid = None;
            let _ = g.runtime.save(&self.inner.state_path);
            emit(
                &self.inner,
                CoreEvent::StatusChanged {
                    session_id: id.to_string(),
                    status: SessionStatus::Stopped,
                    reason: Some("user".into()),
                },
            );
            return Ok(());
        };

        let (ack_tx, ack_rx) = oneshot::channel();
        let _ = worker.control.send(WorkerCmd::Stop {
            user_initiated: true,
            ack: ack_tx,
        });
        let _ = tokio::time::timeout(STOP_ACK_DEADLINE, async {
            let _ = ack_rx.await;
            let _ = worker.handle.await;
        })
        .await;
        Ok(())
    }

    pub async fn stop_all(&self) -> Result<()> {
        let ids: Vec<String> = {
            let g = self.inner.shared.lock().await;
            g.workers.keys().cloned().collect()
        };
        for id in ids {
            let _ = self.stop_session(&id).await;
        }
        Ok(())
    }

    /// Stop all sessions, await their workers, and release the singleton lock.
    /// Call from your binary's shutdown handler (tray "Quit", Ctrl-C).
    pub async fn shutdown(&self) {
        let _ = self.stop_all().await;
        // Release keep-awake if any.
        if let Some(t) = self.inner.keep_awake_token.lock().take() {
            t.release();
        }
        // Remove the IPC socket so a subsequent supervisor doesn't have to
        // delete a stale one. (Stale-socket removal is also done at `serve`
        // bind time as a belt-and-braces.)
        #[cfg(unix)]
        if let Ok(p) = crate::ipc::socket_path() {
            let _ = std::fs::remove_file(p);
        }
        // Drop the lock file: this releases the advisory flock too.
        if let Some(f) = self.inner._lock_file.lock().take() {
            drop(f);
        }
    }

    pub async fn start_session(&self, id: &str) -> Result<()> {
        let session = {
            let mut g = self.inner.shared.lock().await;
            g.intentionally_stopped.remove(id);
            g.file
                .find(id)
                .cloned()
                .ok_or_else(|| Error::SessionNotFound(id.into()))?
        };
        spawn_worker(Arc::clone(&self.inner), session).await
    }

    pub async fn restart_session(&self, id: &str) -> Result<()> {
        // stop_session awaits child exit (no abort), so this is naturally
        // serialized and we won't double-spawn.
        self.stop_session(id).await.ok();
        let mut g = self.inner.shared.lock().await;
        g.intentionally_stopped.remove(id);
        let session = g
            .file
            .find(id)
            .cloned()
            .ok_or_else(|| Error::SessionNotFound(id.into()))?;
        drop(g);
        spawn_worker(Arc::clone(&self.inner), session).await
    }

    /// Reset the restart counter on a session that was parked after exhausting
    /// `restart_max`, then start it again. Crucially: if a worker is still
    /// in its backoff window for this session (i.e. the user hit "Reset"
    /// before giveup completed), `stop_session` first so we don't race the
    /// existing worker's spawn-loop.
    pub async fn reset_and_retry(&self, id: &str) -> Result<()> {
        // Stop any in-flight worker for this id. stop_session marks the
        // session as `intentionally_stopped` — we clear that below.
        self.stop_session(id).await.ok();
        let mut g = self.inner.shared.lock().await;
        if let Some(r) = g.runtime.sessions.get_mut(id) {
            r.restart_count = 0;
            r.reason = None;
        }
        g.intentionally_stopped.remove(id);
        let session = g
            .file
            .find(id)
            .cloned()
            .ok_or_else(|| Error::SessionNotFound(id.into()))?;
        drop(g);
        spawn_worker(Arc::clone(&self.inner), session).await
    }

    pub async fn create_session(&self, session: SessionConfig) -> Result<()> {
        let mut g = self.inner.shared.lock().await;
        if g.file.find(&session.id).is_some() {
            return Err(Error::SessionExists(session.id));
        }
        g.intentionally_stopped.remove(&session.id);
        g.file.upsert(session);
        g.file.save(&self.inner.config_path)?;
        drop(g);
        emit(&self.inner, CoreEvent::ConfigReloaded);
        self.inner.notify_reconcile.notify_one();
        Ok(())
    }

    pub async fn update_session(&self, session: SessionConfig) -> Result<()> {
        let session_id = session.id.clone();
        let running = {
            let mut g = self.inner.shared.lock().await;
            if g.file.find(&session.id).is_none() {
                return Err(Error::SessionNotFound(session.id));
            }
            g.file.upsert(session);
            g.file.save(&self.inner.config_path)?;
            g.workers.contains_key(&session_id)
        };
        emit(&self.inner, CoreEvent::ConfigReloaded);
        if running {
            // Restart with the new config.
            self.restart_session(&session_id).await?;
        }
        self.inner.notify_reconcile.notify_one();
        Ok(())
    }

    pub async fn delete_session(&self, id: &str) -> Result<()> {
        self.stop_session(id).await.ok();
        let mut g = self.inner.shared.lock().await;
        g.file
            .remove(id)
            .ok_or_else(|| Error::SessionNotFound(id.into()))?;
        g.runtime.sessions.remove(id);
        g.intentionally_stopped.remove(id);
        g.file.save(&self.inner.config_path)?;
        let _ = g.runtime.save(&self.inner.state_path);
        drop(g);
        emit(&self.inner, CoreEvent::ConfigReloaded);
        Ok(())
    }

    pub async fn update_preferences(
        &self,
        mutator: impl FnOnce(&mut crate::config::AppPreferences),
    ) -> Result<()> {
        let mut g = self.inner.shared.lock().await;
        mutator(&mut g.file.preferences);
        g.file.save(&self.inner.config_path)?;
        drop(g);
        // Tell the UI to refresh — any path (Tauri command, CLI/IPC, etc.)
        // that mutates prefs must keep subscribed clients in sync.
        emit(&self.inner, CoreEvent::ConfigReloaded);
        Ok(())
    }

    pub fn os(&self) -> &dyn OsLayer {
        self.inner.os.as_ref()
    }

    pub fn config_path(&self) -> &Path {
        &self.inner.config_path
    }

    pub async fn notify(&self, title: &str, body: &str, urgent: bool) {
        notify_gated(&self.inner, title, body, urgent).await
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // Best-effort cleanup if shutdown() wasn't called. The OS will
        // release the flock automatically on process exit, but if we are
        // dropped while the process keeps running (e.g. tests), explicitly
        // dropping the file handle releases the lock now.
        if let Some(f) = self.inner._lock_file.lock().take() {
            drop(f);
        }
    }
}

// --------------------------------------------------------------------------
// Internals
// --------------------------------------------------------------------------

fn emit(inner: &Arc<SupervisorInner>, event: CoreEvent) {
    let _ = inner.events_tx.send(event);
}

async fn notify_gated(inner: &Arc<SupervisorInner>, title: &str, body: &str, urgent: bool) {
    let enabled = {
        let g = inner.shared.lock().await;
        g.file.preferences.notifications_enabled
    };
    if !enabled {
        return;
    }
    // Emit an event so Tauri fires a native notification AS
    // "Session Manager" via plugin-notification (proper identity, proper
    // permission flow). We deliberately do NOT also call os.notify here
    // — that path delivers via `osascript`, which appears as "Script
    // Editor" and causes a duplicate banner when both fire.
    emit(
        inner,
        CoreEvent::NotifyRequested {
            title: title.to_string(),
            body: body.to_string(),
            urgent,
        },
    );
}

async fn reconcile(inner: &Arc<SupervisorInner>) -> Result<()> {
    let desired: Vec<SessionConfig> = {
        let g = inner.shared.lock().await;
        g.file
            .sessions
            .iter()
            .filter(|s| s.auto_restart)
            .cloned()
            .collect()
    };
    for s in desired {
        let (running, intentionally_stopped) = {
            let g = inner.shared.lock().await;
            (
                g.workers.contains_key(&s.id),
                g.intentionally_stopped.contains(&s.id),
            )
        };
        if !running && !intentionally_stopped {
            if let Err(e) = spawn_worker(Arc::clone(inner), s.clone()).await {
                warn!("reconcile spawn {}: {e}", s.id);
            }
        }
    }
    refresh_keep_awake(inner).await;
    Ok(())
}

async fn refresh_keep_awake(inner: &Arc<SupervisorInner>) {
    let (any_active, master_enabled) = {
        let g = inner.shared.lock().await;
        // Master is the single switch. When on, ANY actively-running
        // session keeps the machine awake. Per-session keep_awake used
        // to be a scoping knob — removed because the machine is awake or
        // not; per-session doesn't make sense.
        //
        // `Offline` is treated as "process exists but not responding" by
        // `is_running()` so that the UI can still tail logs etc.; for
        // *keep-awake* purposes Offline means "we have no signal that
        // anything live is happening" and would otherwise fire caffeinate
        // immediately after `open()` resets every status to Offline (the
        // "boot transient" finding). Exclude it explicitly here.
        let any_active = g.file.sessions.iter().any(|s| {
            g.runtime
                .sessions
                .get(&s.id)
                .map(|r| r.status.is_running() && r.status != SessionStatus::Offline)
                .unwrap_or(false)
        });
        (any_active, g.file.preferences.keep_awake_master)
    };

    // Hold the parking_lot guard for the smallest possible window — never
    // across an await.
    let want = any_active && master_enabled;
    enum Action {
        Acquire,
        Release,
        NoOp,
    }
    let action = {
        let token = inner.keep_awake_token.lock();
        match (want, token.is_some()) {
            (true, false) => Action::Acquire,
            (false, true) => Action::Release,
            _ => Action::NoOp,
        }
    };
    match action {
        Action::Acquire => {
            match inner
                .os
                .acquire_keep_awake("session-manager: active session")
            {
                Ok(t) => {
                    *inner.keep_awake_token.lock() = Some(t);
                    {
                        let mut g = inner.shared.lock().await;
                        g.runtime.keep_awake_active = true;
                        let _ = g.runtime.save(&inner.state_path);
                    }
                    emit(
                        inner,
                        CoreEvent::KeepAwakeChanged {
                            active: true,
                            reason: "active session".into(),
                        },
                    );
                }
                Err(e) => warn!("keep-awake acquire failed: {e}"),
            }
        }
        Action::Release => {
            if let Some(t) = inner.keep_awake_token.lock().take() {
                t.release();
            }
            {
                let mut g = inner.shared.lock().await;
                g.runtime.keep_awake_active = false;
                let _ = g.runtime.save(&inner.state_path);
            }
            emit(
                inner,
                CoreEvent::KeepAwakeChanged {
                    active: false,
                    reason: "no working sessions".into(),
                },
            );
        }
        Action::NoOp => {}
    }
}

/// Spawn a *claim* worker for an externally-started session.
///
/// Unlike `spawn_worker`, this does NOT exec the agent binary — the
/// process is already running somewhere we don't own. The worker
/// instead probes its JSONL (status detection), watches the PID for
/// death, and forwards Stop/Restart control to a SIGTERM on the
/// external PID. If the external dies while `auto_restart` is on, the
/// worker transitions into the normal spawn loop and the supervisor
/// "takes over" cleanly.
async fn spawn_claimed_worker(
    inner: Arc<SupervisorInner>,
    session: SessionConfig,
    pid: u32,
) -> Result<()> {
    let mut g = inner.shared.lock().await;
    if g.workers.contains_key(&session.id) {
        tracing::debug!("spawn_claimed_worker({}): already running", session.id);
        return Ok(());
    }
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel::<WorkerCmd>();
    let inner_for_worker = Arc::clone(&inner);
    let session_id = session.id.clone();
    let handle = tokio::spawn(async move {
        claimed_worker_loop(inner_for_worker, session, ctl_rx, pid).await;
    });
    g.workers.insert(
        session_id,
        SessionWorker {
            handle,
            control: ctl_tx,
        },
    );
    Ok(())
}

async fn spawn_worker(inner: Arc<SupervisorInner>, session: SessionConfig) -> Result<()> {
    // Single critical section: check membership, spawn, and insert under
    // the same lock. Two concurrent callers (reconciler + IPC start) racing
    // here used to both pass the contains_key check and both spawn
    // workers; one would orphan. tokio::spawn is non-blocking, and the
    // task it returns can't make progress until we drop the lock, so the
    // child worker_loop is guaranteed to see the workers map already
    // contains its own entry.
    let mut g = inner.shared.lock().await;
    if g.workers.contains_key(&session.id) {
        tracing::debug!("spawn_worker({}): already running", session.id);
        return Ok(());
    }
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel::<WorkerCmd>();
    let inner_for_worker = Arc::clone(&inner);
    let session_id = session.id.clone();
    let handle = tokio::spawn(async move {
        worker_loop(inner_for_worker, session, ctl_rx).await;
    });
    g.workers.insert(
        session_id,
        SessionWorker {
            handle,
            control: ctl_tx,
        },
    );
    Ok(())
}

async fn worker_loop(
    inner: Arc<SupervisorInner>,
    session: SessionConfig,
    mut ctl_rx: mpsc::UnboundedReceiver<WorkerCmd>,
) {
    let backend = match make_backend(&session.agent) {
        Ok(b) => b,
        Err(e) => {
            mark_status(
                &inner,
                &session.id,
                SessionStatus::Crashed,
                Some(e.to_string()),
            )
            .await;
            cleanup_worker_entry(&inner, &session.id).await;
            return;
        }
    };

    let mut backoff = Duration::from_millis(500);
    let mut restart_count: u32 = 0;
    let mut pending_ack: Option<oneshot::Sender<()>> = None;

    loop {
        // Pull the JSONL hint a prior run captured (if any). Passing
        // `--resume <path>` makes restarts deterministic.
        let resume_hint = {
            let g = inner.shared.lock().await;
            g.runtime
                .sessions
                .get(&session.id)
                .and_then(|r| r.claude_jsonl_path.clone())
        };
        let launch = match backend.build_launch(&session, resume_hint.as_deref()) {
            Ok(l) => l,
            Err(e) => {
                mark_status(
                    &inner,
                    &session.id,
                    SessionStatus::Crashed,
                    Some(e.to_string()),
                )
                .await;
                notify_gated(
                    &inner,
                    &format!("{} failed to launch", session.name),
                    &e.to_string(),
                    true,
                )
                .await;
                cleanup_worker_entry(&inner, &session.id).await;
                return;
            }
        };

        mark_status(&inner, &session.id, SessionStatus::Starting, None).await;
        let log_path = session.resolved_log_path().unwrap_or_else(|_| {
            paths::log_dir()
                .unwrap_or_else(|_| PathBuf::from("/tmp"))
                .join(format!("{}.log", session.id))
        });
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Snapshot the project dir's existing JSONLs IMMEDIATELY before
        // exec. discover_session_file later compares against this to
        // disambiguate "our spawn's jsonl" from "another concurrent
        // claude in the same cwd that happened to touch its jsonl in
        // the same second."
        let pre_snapshot = backend.snapshot_session_files(&session);

        let mut child = match spawn_child(&launch) {
            Ok(c) => c,
            Err(e) => {
                mark_status(
                    &inner,
                    &session.id,
                    SessionStatus::Crashed,
                    Some(e.to_string()),
                )
                .await;
                notify_gated(
                    &inner,
                    &format!("{} crashed", session.name),
                    &format!("Could not spawn process: {e}"),
                    true,
                )
                .await;
                if !session.auto_restart || restart_count >= session.restart_max {
                    // Same giveup → park semantics as the post-run crash path.
                    cleanup_worker_entry(&inner, &session.id).await;
                    mark_intentionally_stopped(&inner, &session.id).await;
                    refresh_keep_awake(&inner).await;
                    return;
                }
                restart_count += 1;
                // Persist for the UI's "Restarts" field so the user sees the
                // attempt count grow even when spawn keeps failing.
                {
                    let mut g = inner.shared.lock().await;
                    g.runtime.entry_mut(&session.id).restart_count = restart_count;
                    let _ = g.runtime.save(&inner.state_path);
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let pid = child.id().unwrap_or(0);
        let spawn_at = Utc::now();
        {
            let mut g = inner.shared.lock().await;
            let r = g.runtime.entry_mut(&session.id);
            r.pid = Some(pid);
            r.started_at = Some(spawn_at);
            r.restart_count = restart_count;
            let _ = g.runtime.save(&inner.state_path);
        }

        // Deterministic-restart support: identify the JSONL file that this
        // claude process is writing to and persist its path. On the next
        // start we resume that EXACT file (`claude --resume <path>`)
        // instead of `--continue`, which picks "the latest in cwd" — wrong
        // when multiple sessions share a project directory.
        //
        // Strategy: poll the project dir for ~10s for a `.jsonl` whose
        // mtime is at-or-after `spawn_at`. Claude usually writes a
        // bridge-session line within a few seconds of startup.
        if let Some(backend_for_jsonl) = make_backend(&session.agent).ok() {
            let inner_for_jsonl = Arc::clone(&inner);
            let session_for_jsonl = session.clone();
            let pre_snapshot_for_jsonl = pre_snapshot.clone();
            tokio::spawn(async move {
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_secs(20);
                while std::time::Instant::now() < deadline {
                    if let Some(path) = backend_for_jsonl.discover_session_file(
                        &session_for_jsonl,
                        &pre_snapshot_for_jsonl,
                        spawn_at,
                    ) {
                        // Guard against the deleted-session race: this task
                        // can outlive a delete_session, and a blind
                        // `entry_mut` would resurrect a SessionRuntime row
                        // for an id that no longer exists in the config —
                        // surfacing as a phantom session on disk + in the UI.
                        // Only persist when the session is still defined.
                        let mut g = inner_for_jsonl.shared.lock().await;
                        if g.file.find(&session_for_jsonl.id).is_none() {
                            return;
                        }
                        let r = g.runtime.entry_mut(&session_for_jsonl.id);
                        if r.claude_jsonl_path.as_deref() != Some(&path) {
                            r.claude_jsonl_path = Some(path);
                            let _ = g.runtime.save(&inner_for_jsonl.state_path);
                        }
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                }
            });
        }

        // Modal-prompt answer pump. Claude shows a "Resume from summary /
        // full / don't ask" prompt on `--continue` when the prior session
        // is large enough; the worker has no human at the keyboard, so we
        // detect the prompt in stdout (capture_stream) and answer one `\n`
        // here — picks the default (Resume from summary).
        //
        // The pump *holds* its end of the stdin pipe open after answering:
        // closing it would EOF the `script(1)` wrapper's stdin, which on
        // some configurations propagates to the PTY and tears the agent
        // down. Held until the worker aborts the task post-exit.
        let stdin_handle = child.stdin.take();
        let prompt_seen = Arc::new(Notify::new());
        let stdin_pump = {
            let prompt_seen = Arc::clone(&prompt_seen);
            tokio::spawn(async move {
                let Some(mut stdin) = stdin_handle else { return };
                prompt_seen.notified().await;
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(b"\n").await;
                let _ = stdin.flush().await;
                std::future::pending::<()>().await;
            })
        };

        let log_path_for_capture = log_path.clone();
        let inner_for_capture = Arc::clone(&inner);
        let session_for_capture = session.id.clone();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let prompt_seen_stdout = Arc::clone(&prompt_seen);
        let stdout_task = tokio::spawn(async move {
            if let Some(s) = stdout {
                capture_stream(
                    s,
                    false,
                    &session_for_capture,
                    &log_path_for_capture,
                    &inner_for_capture,
                    Some(prompt_seen_stdout),
                )
                .await;
            }
        });
        let log_path_for_capture2 = log_path.clone();
        let inner_for_capture2 = Arc::clone(&inner);
        let session_for_capture2 = session.id.clone();
        let stderr_task = tokio::spawn(async move {
            if let Some(s) = stderr {
                capture_stream(
                    s,
                    true,
                    &session_for_capture2,
                    &log_path_for_capture2,
                    &inner_for_capture2,
                    None,
                )
                .await;
            }
        });

        let inner_for_probe = Arc::clone(&inner);
        let session_for_probe = session.clone();
        let (probe_stop_tx, mut probe_stop_rx) = oneshot::channel::<()>();
        let _probe_task = tokio::spawn(async move {
            let backend = match make_backend(&session_for_probe.agent) {
                Ok(b) => b,
                Err(_) => return,
            };
            loop {
                tokio::select! {
                    _ = &mut probe_stop_rx => return,
                    _ = tokio::time::sleep(PROBE_PERIOD) => {}
                }
                if let Ok(probe) = backend.probe_activity(&session_for_probe).await {
                    apply_probe(&inner_for_probe, &session_for_probe.id, probe).await;
                }
            }
        });

        let mut user_initiated_stop = false;
        let exit_status: Option<std::process::ExitStatus> = tokio::select! {
            cmd = ctl_rx.recv() => {
                match cmd {
                    Some(WorkerCmd::Stop { user_initiated, ack }) => {
                        user_initiated_stop = user_initiated;
                        pending_ack = Some(ack);
                    }
                    None => {
                        // Control channel dropped — supervisor going away.
                        user_initiated_stop = true;
                    }
                }
                let _ = child.start_kill();
                match tokio::time::timeout(GRACEFUL_STOP_DEADLINE, child.wait()).await {
                    Ok(Ok(s)) => Some(s),
                    _ => {
                        let _ = child.kill().await;
                        child.try_wait().ok().flatten()
                    }
                }
            }
            w = child.wait() => w.ok(),
        };
        let _ = probe_stop_tx.send(());
        stdout_task.abort();
        stderr_task.abort();
        stdin_pump.abort();

        if user_initiated_stop {
            // Stop is a "session ended cleanly from the user's POV": clear
            // the stale jsonl hint so the *next* start with resume=Continue
            // discovers the latest JSONL fresh, instead of pinning to the
            // file we just finished writing to.
            clear_jsonl_hint(&inner, &session.id).await;
            mark_status(
                &inner,
                &session.id,
                SessionStatus::Stopped,
                Some("user".into()),
            )
            .await;
            cleanup_worker_entry(&inner, &session.id).await;
            // Keep-awake releases promptly even though reconciler is on a
            // 30s tick.
            refresh_keep_awake(&inner).await;
            if let Some(ack) = pending_ack.take() {
                let _ = ack.send(());
            }
            return;
        }
        let clean = exit_status.as_ref().map(|s| s.success()).unwrap_or(false);
        if clean {
            // Clean Done — same logic as user-stop: clear the jsonl hint
            // so a fresh Continue picks the actual latest, not this just-
            // -completed transcript.
            clear_jsonl_hint(&inner, &session.id).await;
            mark_status(&inner, &session.id, SessionStatus::Done, None).await;
            notify_gated(
                &inner,
                &format!("{} finished", session.name),
                "Session completed.",
                false,
            )
            .await;
            cleanup_worker_entry(&inner, &session.id).await;
            // A clean `Done` is terminal: don't let the reconciler resurrect
            // an `auto_restart` session that completed successfully (e.g.
            // the user is using `claude` for a one-shot script). The next
            // user-initiated start removes this entry.
            mark_intentionally_stopped(&inner, &session.id).await;
            refresh_keep_awake(&inner).await;
            return;
        }

        let code = exit_status.as_ref().and_then(|s| s.code());
        let reason = code
            .map(|c| format!("exit {c}"))
            .unwrap_or_else(|| "killed".into());
        mark_status(
            &inner,
            &session.id,
            SessionStatus::Crashed,
            Some(reason.clone()),
        )
        .await;
        notify_gated(&inner, &format!("{} crashed", session.name), &reason, true).await;
        if !session.auto_restart || restart_count >= session.restart_max {
            // Final terminal state — be loud so the user knows nothing else
            // will happen automatically.
            let final_reason = if session.auto_restart {
                format!(
                    "{reason} (gave up after {} restart attempts — open log or hit Reset to retry)",
                    session.restart_max
                )
            } else {
                reason.clone()
            };
            mark_status(
                &inner,
                &session.id,
                SessionStatus::Crashed,
                Some(final_reason.clone()),
            )
            .await;
            if session.auto_restart {
                notify_gated(
                    &inner,
                    &format!("{} stopped retrying", session.name),
                    &format!(
                        "Failed {} times. Open Session Manager to inspect the log.",
                        session.restart_max
                    ),
                    true,
                )
                .await;
            }
            cleanup_worker_entry(&inner, &session.id).await;
            // Park the session: the reconciler MUST NOT respawn a session
            // that just exhausted its backoff budget. The user clears this
            // by hitting Start, Restart, or "Reset & retry".
            mark_intentionally_stopped(&inner, &session.id).await;
            refresh_keep_awake(&inner).await;
            return;
        }
        restart_count += 1;
        info!(
            "restarting {} (#{}) after backoff {:?}",
            session.id, restart_count, backoff
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
        // No more pending_ack on retry — restart is "still working" from the
        // caller's perspective.
        continue;
    }
}

async fn mark_intentionally_stopped(inner: &Arc<SupervisorInner>, id: &str) {
    let mut g = inner.shared.lock().await;
    g.intentionally_stopped.insert(id.to_string());
}

/// Concurrent collector for a Vec of futures without pulling in the
/// `futures` crate. Spawns each onto the current runtime, awaits all.
///
/// **Panic-safe**: if an individual future panics, its slot in the result
/// is filled with `T::default()` so the rest of the batch keeps going.
/// Callers (currently only `external_sessions`) treat results as
/// best-effort anyway; a single backend hiccup must not collapse the
/// whole probe sweep (which used to make the External list silently
/// disappear in the UI).
async fn futures_join_all<F, T>(futs: impl IntoIterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static + Default,
{
    let handles: Vec<_> = futs.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(v) => out.push(v),
            Err(e) => {
                warn!("probe future failed: {e}");
                out.push(T::default());
            }
        }
    }
    out
}

/// Resolve every registered backend's binary path, compare against what we
/// recorded last cycle, emit `BinaryUpgraded` on changes. `emit_changes`
/// is false on the priming call.
async fn check_binary_versions(inner: &Arc<SupervisorInner>, emit_changes: bool) {
    use crate::backend::{make_backend, registry};
    let resolved: Vec<(String, PathBuf)> = tokio::task::spawn_blocking(|| {
        let mut out = Vec::new();
        for b in registry() {
            if let Ok(backend) = make_backend(&b.id) {
                if let Ok(p) = backend.resolve_binary() {
                    out.push((b.id, p));
                }
            }
        }
        out
    })
    .await
    .unwrap_or_default();

    let mut events: Vec<CoreEvent> = Vec::new();
    {
        let mut g = inner.shared.lock().await;
        for (id, new_path) in resolved {
            if let Some(prev) = g.binary_paths.get(&id) {
                if *prev != new_path && emit_changes {
                    events.push(CoreEvent::BinaryUpgraded {
                        backend_id: id.clone(),
                        old_path: prev.to_string_lossy().into_owned(),
                        new_path: new_path.to_string_lossy().into_owned(),
                    });
                }
            }
            g.binary_paths.insert(id, new_path);
        }
    }
    for ev in events {
        emit(inner, ev);
    }
}

async fn cleanup_worker_entry(inner: &Arc<SupervisorInner>, id: &str) {
    let mut g = inner.shared.lock().await;
    g.workers.remove(id);
}

/// Drop the cached JSONL hint for a session that finished cleanly (clean
/// Done or user-initiated Stop). The next Start with resume=Continue
/// then re-discovers via the post-spawn task instead of pinning the user
/// to the transcript they just closed.
async fn clear_jsonl_hint(inner: &Arc<SupervisorInner>, id: &str) {
    let mut g = inner.shared.lock().await;
    if let Some(r) = g.runtime.sessions.get_mut(id) {
        if r.claude_jsonl_path.is_some() {
            r.claude_jsonl_path = None;
            let _ = g.runtime.save(&inner.state_path);
        }
    }
}

/// Probe-only worker for sessions where the process is externally owned.
///
/// Flow:
///   1. Record the external PID + initial status into runtime so the UI
///      shows the session as live immediately.
///   2. Spin up the same JSONL probe loop the spawn-mode worker uses.
///   3. Spin a PID watch that resolves when the external dies.
///   4. `select!` on (control channel, PID watch). On Stop → SIGTERM the
///      external and ack. On PID death → if `auto_restart` is on, hand
///      off to `spawn_worker` for a normal supervised respawn.
///
/// Crucially: we never exec our own claude here. The user's existing
/// process keeps its context untouched until either they Stop it or it
/// dies on its own.
async fn claimed_worker_loop(
    inner: Arc<SupervisorInner>,
    session: SessionConfig,
    mut ctl_rx: mpsc::UnboundedReceiver<WorkerCmd>,
    pid: u32,
) {
    let backend = match make_backend(&session.agent) {
        Ok(b) => b,
        Err(e) => {
            mark_status(
                &inner,
                &session.id,
                SessionStatus::Crashed,
                Some(e.to_string()),
            )
            .await;
            cleanup_worker_entry(&inner, &session.id).await;
            return;
        }
    };

    // Runtime snapshot for the claimed session was already set
    // synchronously by `claim_external` before this task ran — that's
    // what avoids the race with the UI's `refreshExternal` and lets the
    // external row disappear immediately.

    // Probe task — identical to the one in worker_loop.
    let inner_for_probe = Arc::clone(&inner);
    let session_for_probe = session.clone();
    let (probe_stop_tx, mut probe_stop_rx) = oneshot::channel::<()>();
    let _probe_task = tokio::spawn(async move {
        let backend = match make_backend(&session_for_probe.agent) {
            Ok(b) => b,
            Err(_) => return,
        };
        loop {
            tokio::select! {
                _ = &mut probe_stop_rx => return,
                _ = tokio::time::sleep(PROBE_PERIOD) => {}
            }
            if let Ok(probe) = backend.probe_activity(&session_for_probe).await {
                apply_probe(&inner_for_probe, &session_for_probe.id, probe).await;
            }
        }
    });

    // PID watcher — POSIX kill(pid, 0) probes existence cheaply. We poll
    // at 2s because external death is not time-critical (the user will
    // see it via the UI's status flip anyway).
    //
    // Errno handling:
    //   - ESRCH: process is gone → return (death).
    //   - EPERM: process exists, we lack signal permission → treat as
    //     alive (the original audit noted EPERM would otherwise spin
    //     forever in sandboxed contexts).
    //   - Anything else: alive-by-assumption + log so we notice if the
    //     pattern ever changes on a new macOS release.
    let pid_watch = async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            #[cfg(unix)]
            {
                let r = unsafe { libc::kill(pid as i32, 0) };
                if r != 0 {
                    let err = std::io::Error::last_os_error();
                    match err.raw_os_error() {
                        Some(libc::ESRCH) => return,
                        Some(libc::EPERM) => {} // process exists, no perm — alive
                        other => {
                            warn!("pid_watch kill({pid}, 0) errno={other:?}: {err}");
                        }
                    }
                }
            }
            #[cfg(not(unix))]
            {
                return;
            }
        }
    };

    // No JSONL hint capture here — the external has been running long
    // before we claimed it, so the strict `mtime >= spawn_at` check that
    // works for the spawn path would never match. If/when the external
    // dies and `auto_restart` kicks in, the supervised spawn below picks
    // up the JSONL via its own post-spawn discovery.
    let _ = backend; // unused after the make_backend check above

    let exit = tokio::select! {
        cmd = ctl_rx.recv() => {
            match cmd {
                Some(WorkerCmd::Stop { user_initiated: _, ack }) => ClaimExit::UserStop { ack: Some(ack) },
                None => ClaimExit::UserStop { ack: None },
            }
        }
        _ = pid_watch => ClaimExit::PidDied,
    };

    let _ = probe_stop_tx.send(());

    match exit {
        ClaimExit::UserStop { ack } => {
            // SIGTERM the external — same guard as stop_external uses to
            // avoid signalling a recycled PID.
            #[cfg(unix)]
            {
                if pid_looks_like_agent(pid) {
                    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                    // Brief wait so the JSONL settles before any potential
                    // restart finds the latest one.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
            mark_status(
                &inner,
                &session.id,
                SessionStatus::Stopped,
                Some("user".into()),
            )
            .await;
            mark_intentionally_stopped(&inner, &session.id).await;
            cleanup_worker_entry(&inner, &session.id).await;
            refresh_keep_awake(&inner).await;
            if let Some(ack) = ack {
                let _ = ack.send(());
            }
        }
        ClaimExit::PidDied => {
            // External died on its own. Honour any racing stop_session —
            // it may have inserted intentionally_stopped between our
            // pid_watch firing and us reaching this arm. Without this
            // guard the takeover would respawn against an explicit Stop.
            cleanup_worker_entry(&inner, &session.id).await;
            let intentionally_stopped = {
                let g = inner.shared.lock().await;
                g.intentionally_stopped.contains(&session.id)
            };
            if session.auto_restart && !intentionally_stopped {
                mark_status(
                    &inner,
                    &session.id,
                    SessionStatus::Starting,
                    Some("external exited — taking over".into()),
                )
                .await;
                let _ = spawn_worker(Arc::clone(&inner), session).await;
            } else {
                mark_status(
                    &inner,
                    &session.id,
                    SessionStatus::Stopped,
                    Some(if intentionally_stopped {
                        "stopped".into()
                    } else {
                        "external exited".into()
                    }),
                )
                .await;
                refresh_keep_awake(&inner).await;
            }
        }
    }
}

enum ClaimExit {
    /// `ack=None` happens when the control channel was dropped (supervisor
    /// shutting down) instead of an explicit Stop command.
    UserStop { ack: Option<oneshot::Sender<()>> },
    PidDied,
}

fn spawn_child(spec: &LaunchSpec) -> std::io::Result<Child> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args).current_dir(&spec.cwd).env_clear();
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    // Pipe stdin (rather than null) so the worker can answer interactive
    // modal prompts the agent shows on startup — Claude's "Resume from
    // summary / full / don't ask" prompt for large sessions is the
    // motivating case. The worker writes a single `\n` when it detects
    // such a prompt in stdout (capture_stream); for sessions without a
    // prompt, the pipe just stays idle, which is the same shape `script(1)`
    // sees in a normal terminal.
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .kill_on_drop(false);

    #[cfg(unix)]
    {
        unsafe {
            cmd.pre_exec(|| {
                // setsid: new session, becomes process-group leader so the
                // agent survives our exit and reattaches to anyone subscribed
                // to its native remote control (§7.2 / §8.1).
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000200); // CREATE_NEW_PROCESS_GROUP
    }
    cmd.spawn()
}

/// Per-session log rotation: at this size, rotate `<id>.log` → `<id>.log.1`
/// (replacing any previous `.1`) and start fresh. We only keep one old file
/// — that's enough for "what just crashed" and prevents unbounded disk use
/// when a verbose agent runs for weeks.
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

async fn capture_stream<R>(
    stream: R,
    is_stderr: bool,
    session_id: &str,
    log_path: &Path,
    inner: &Arc<SupervisorInner>,
    // One-shot signal for the "I just saw a modal prompt that needs an
    // Enter to dismiss" condition. Only stdout passes a Notify; stderr
    // doesn't carry the prompt and passes None. After we fire once, we
    // never fire again — `notify_one()` on an already-notified Notify is
    // a no-op anyway, but `prompt_signaled` keeps us from scanning every
    // subsequent line.
    prompt_seen: Option<Arc<Notify>>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();
    let mut file = open_log(log_path).await;
    let mut prompt_signaled = false;
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(f) = file.as_mut() {
            let _ = f.write_all(line.as_bytes()).await;
            let _ = f.write_all(b"\n").await;
            if let Ok(meta) = f.metadata().await {
                if meta.len() >= LOG_ROTATE_BYTES {
                    drop(file.take());
                    rotate_log(log_path);
                    file = open_log(log_path).await;
                }
            }
        }
        // Surface a remote-control URL when the agent prints one (Claude
        // Code's connect line — the supervisor only surfaces the URL; it
        // never proxies the channel.
        if let Some(url) = extract_remote_url(&line) {
            let qr_svg = render_qr_svg(&url);
            let _ = inner.events_tx.send(CoreEvent::RemoteAffordance {
                session_id: session_id.to_string(),
                url: Some(url.clone()),
                qr: qr_svg.clone(),
            });
            // Persist into runtime so the UI on first open already shows it.
            let mut g = inner.shared.lock().await;
            let r = g.runtime.entry_mut(session_id);
            r.remote_url = Some(url);
            r.remote_qr = qr_svg;
            r.remote_online = true;
            let _ = g.runtime.save(&inner.state_path);
        }
        // Modal prompt detection. Claude prints this when `--continue`
        // would resume a session large enough to be expensive. The
        // worker can't show a TUI so we accept the default by sending
        // Enter (the highlighted choice is "Resume from summary").
        // We strip ANSI escapes because the prompt arrives via the
        // `script(1)` PTY recording and includes colour codes.
        if !prompt_signaled {
            if let Some(notify) = prompt_seen.as_ref() {
                let plain = strip_ansi(&line);
                if plain.contains("Resuming the full session will consume")
                    || plain.contains("Resume from summary")
                {
                    notify.notify_one();
                    prompt_signaled = true;
                }
            }
        }
        // Log lines on a separate channel — keeps a verbose agent's
        // stdout from pushing a `NeedsPermission` out of the critical
        // event ring buffer.
        let _ = inner.logs_tx.send(CoreEvent::LogLine {
            session_id: session_id.to_string(),
            line: truncate_for_event(&line),
            is_stderr,
            at_ms: Utc::now().timestamp_millis(),
        });
    }
}

fn strip_ansi(s: &str) -> String {
    // Tiny inline ANSI/CSI stripper — we don't need full coverage, just
    // enough to make `contains()` work on PTY recording lines that include
    // colour escapes around the prompt text.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC — skip until we hit a terminating byte (letter or '~').
            i += 1;
            // Optional bracket or other intro byte
            if i < bytes.len() && (bytes[i] == b'[' || bytes[i] == b']' || bytes[i] == b'(') {
                i += 1;
            }
            while i < bytes.len() {
                let b = bytes[i];
                i += 1;
                if b.is_ascii_alphabetic() || b == b'~' {
                    break;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

async fn open_log(log_path: &Path) -> Option<tokio::fs::File> {
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await
    {
        Ok(f) => Some(f),
        Err(e) => {
            warn!("open log {log_path:?}: {e}");
            None
        }
    }
}

fn rotate_log(log_path: &Path) {
    let prev = log_path.with_extension(
        log_path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| format!("{s}.1"))
            .unwrap_or_else(|| "1".into()),
    );
    let _ = std::fs::remove_file(&prev);
    let _ = std::fs::rename(log_path, &prev);
}

fn url_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // Match any https URL on agent-provider domains (claude.ai, anthropic,
        // openai). Conservative — we don't want to surface unrelated URLs.
        Regex::new(
            r#"(?i)https://(?:[a-z0-9-]+\.)*(?:claude\.ai|anthropic\.com|openai\.com)/[^\s'"]+"#,
        )
        .expect("static regex")
    })
}

fn extract_remote_url(line: &str) -> Option<String> {
    let m = url_regex().find(line)?;
    Some(m.as_str().to_string())
}

/// Render an SVG QR code for a remote-control URL. Generated once on
/// capture and persisted into runtime state — the UI just inlines the
/// SVG. Medium ECC is the sweet spot for short URLs (small payload, more
/// robust against camera glare).
fn render_qr_svg(url: &str) -> Option<String> {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};
    let code = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M).ok()?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(192, 192)
        .light_color(svg::Color("#ffffff"))
        .dark_color(svg::Color("#0b0d12"))
        .build();
    Some(svg)
}

fn truncate_for_event(line: &str) -> String {
    const MAX: usize = 4096;
    if line.len() <= MAX {
        line.to_string()
    } else {
        let mut s = line[..MAX].to_string();
        s.push('…');
        s
    }
}

async fn apply_probe(inner: &Arc<SupervisorInner>, id: &str, probe: ActivityProbe) {
    let mut needs_permission_notify = false;
    let mut display_name = id.to_string();
    // Hold the lock only as long as we're mutating in-memory state. The
    // fs save (50-200 µs typically, more on slow disks) happens *after*
    // the lock is released so probes don't stall the IPC server, the tray,
    // and the CLI for every status change.
    let runtime_snapshot;
    {
        let mut g = inner.shared.lock().await;
        if let Some(cfg) = g.file.find(id) {
            display_name = cfg.name.clone();
        }
        let r = g.runtime.entry_mut(id);
        if let Some(s) = probe.status {
            if r.status != s {
                r.status = s;
                r.last_seen = Some(Utc::now());
                emit(
                    inner,
                    CoreEvent::StatusChanged {
                        session_id: id.to_string(),
                        status: s,
                        reason: None,
                    },
                );
                if matches!(s, SessionStatus::NeedsPermission) {
                    emit(
                        inner,
                        CoreEvent::NeedsPermission {
                            session_id: id.to_string(),
                            prompt: None,
                        },
                    );
                    needs_permission_notify = true;
                }
            }
        }
        if let Some(url) = probe.remote_url {
            if r.remote_url.as_deref() != Some(url.as_str()) {
                r.remote_url = Some(url.clone());
                emit(
                    inner,
                    CoreEvent::RemoteAffordance {
                        session_id: id.to_string(),
                        url: Some(url),
                        qr: None,
                    },
                );
            }
        }
        if let Some(online) = probe.remote_online {
            r.remote_online = online;
        }
        if let Some(act) = probe.last_activity.clone() {
            r.last_activity = Some(act);
        }
        if !probe.recent_lines.is_empty() {
            emit(
                inner,
                CoreEvent::TranscriptTail {
                    session_id: id.to_string(),
                    lines: probe.recent_lines.clone(),
                },
            );
        }
        runtime_snapshot = g.runtime.clone();
    }
    let _ = runtime_snapshot.save(&inner.state_path);
    if needs_permission_notify {
        notify_gated(
            inner,
            &format!("{display_name} needs permission"),
            "Open the session to answer.",
            true,
        )
        .await;
    }
}

async fn mark_status(
    inner: &Arc<SupervisorInner>,
    id: &str,
    status: SessionStatus,
    reason: Option<String>,
) {
    let mut g = inner.shared.lock().await;
    let r = g.runtime.entry_mut(id);
    let changed = r.status != status;
    r.status = status;
    r.last_seen = Some(Utc::now());
    if !status.is_running() {
        r.pid = None;
    }
    if changed {
        if let Some(reason) = &reason {
            r.reason = Some(reason.clone());
        }
    }
    let _ = g.runtime.save(&inner.state_path);
    emit(
        inner,
        CoreEvent::StatusChanged {
            session_id: id.to_string(),
            status,
            reason,
        },
    );
}

fn spawn_config_watcher(inner: Arc<SupervisorInner>) {
    let handle = match tokio::runtime::Handle::try_current() {
        Ok(h) => h,
        Err(_) => {
            warn!("config watcher: no tokio runtime — file changes won't reload");
            return;
        }
    };
    let path = inner.config_path.clone();
    std::thread::Builder::new()
        .name("config-watcher".into())
        .spawn(move || {
            use notify::Watcher;
            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(w) => w,
                Err(_) => return,
            };
            if let Some(parent) = path.parent() {
                let _ = watcher.watch(parent, notify::RecursiveMode::NonRecursive);
            }
            let mut last_emit = std::time::Instant::now() - Duration::from_secs(10);
            while let Ok(event) = rx.recv() {
                if event.is_err() {
                    continue;
                }
                let _ = rx.recv_timeout(Duration::from_millis(200));
                if last_emit.elapsed() < Duration::from_millis(150) {
                    continue;
                }
                last_emit = std::time::Instant::now();
                // Read+parse. Anything other than "valid TOML" is surfaced
                // back to the UI as a ConfigError event so the user sees
                // what's wrong instead of "I edited the file and nothing
                // happened".
                let raw = match std::fs::read_to_string(&path) {
                    Ok(r) => r,
                    Err(e) => {
                        let inner = Arc::clone(&inner);
                        let msg = format!("could not read config: {e}");
                        handle.spawn(async move {
                            emit(&inner, CoreEvent::ConfigError { message: msg });
                        });
                        continue;
                    }
                };
                let new_file: SessionsFile = match toml::from_str(&raw) {
                    Ok(f) => f,
                    Err(e) => {
                        let inner = Arc::clone(&inner);
                        let msg = format!("sessions.toml has a syntax error: {e}");
                        handle.spawn(async move {
                            emit(&inner, CoreEvent::ConfigError { message: msg });
                        });
                        continue;
                    }
                };
                let inner = Arc::clone(&inner);
                handle.spawn(async move {
                    // Soft-data-loss safeguard: if we used to have sessions
                    // and the file now has none, treat this as a likely
                    // accidental edit (the user ran `> sessions.toml`,
                    // someone ran `rm`, an editor saved an empty buffer).
                    // Don't apply — but tell the user.
                    let prev_count = {
                        let g = inner.shared.lock().await;
                        g.file.sessions.len()
                    };
                    if prev_count > 0 && new_file.sessions.is_empty() {
                        emit(&inner, CoreEvent::ConfigError {
                            message: format!(
                                "sessions.toml is empty but used to define {prev_count} sessions. \
                                 Ignored to avoid losing your fleet — add at least one [[session]] back, \
                                 or delete sessions from the UI."
                            ),
                        });
                        return;
                    }
                    {
                        let mut g = inner.shared.lock().await;
                        g.file = new_file;
                    }
                    emit(&inner, CoreEvent::ConfigReloaded);
                    inner.notify_reconcile.notify_one();
                });
            }
        })
        .ok();
}

#[cfg(unix)]
fn pid_looks_like_agent(pid: u32) -> bool {
    // `ps -o command= -p <pid>` returns just the command line for one PID,
    // exiting non-zero if the PID is gone. We accept any agent token.
    let out = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output();
    let Ok(out) = out else { return false };
    if !out.status.success() {
        return false;
    }
    let cmd = String::from_utf8_lossy(&out.stdout).to_string();
    cmd.contains("claude") || cmd.contains("codex")
}

/// Acquire an advisory exclusive lock on `lock_path` for the lifetime of the
/// returned [`File`]. Cross-platform via `fs2`. The lock is released
/// automatically when the file handle is dropped *or* when the process exits
/// for any reason — including SIGKILL — so there's no stale-lock cleanup.
fn acquire_singleton_lock(lock_path: &Path) -> Result<File> {
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    file.try_lock_exclusive().map_err(|_| {
        Error::Other(format!(
            "another supervisor is already running (file lock on {}). \
             Quit the GUI app, or stop the `session-manager daemon` first.",
            lock_path.display()
        ))
    })?;
    // Write our PID for human debuggability — *not* used for liveness.
    use std::io::{Seek, SeekFrom, Write};
    let mut f = &file;
    let _ = (&mut f).seek(SeekFrom::Start(0));
    let _ = f.set_len(0);
    let _ = writeln!(&mut f, "{}", std::process::id());
    Ok(file)
}

pub fn backend_registry() -> Vec<BackendInfo> {
    registry()
}

pub fn snapshot_runtime(state: &RuntimeState, id: &str) -> Option<SessionRuntime> {
    state.sessions.get(id).cloned()
}

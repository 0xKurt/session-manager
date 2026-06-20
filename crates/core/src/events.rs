//! Status enum + event stream pushed from core to UI (§9.4 + §7.3).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SessionStatus {
    /// Spawned, not yet observed to be ready.
    Starting,
    /// Agent actively running/executing.
    Working,
    /// Blocked, waiting on the user. Highest UI priority (§9.4).
    NeedsPermission,
    /// Alive, waiting for input.
    Idle,
    /// Finished cleanly.
    Done,
    /// Stalled due to limits (§5 exception).
    RateLimited,
    /// Died unexpectedly.
    Crashed,
    /// Stopped by user.
    #[default]
    Stopped,
    /// Remote channel down (local process still alive).
    Offline,
}

impl SessionStatus {
    /// True for states the UI should sort to the top.
    pub fn is_priority(self) -> bool {
        matches!(
            self,
            SessionStatus::NeedsPermission | SessionStatus::Crashed
        )
    }

    pub fn is_running(self) -> bool {
        matches!(
            self,
            SessionStatus::Starting
                | SessionStatus::Working
                | SessionStatus::NeedsPermission
                | SessionStatus::Idle
                | SessionStatus::RateLimited
                | SessionStatus::Offline
        )
    }

    pub fn slug(self) -> &'static str {
        match self {
            SessionStatus::Starting => "starting",
            SessionStatus::Working => "working",
            SessionStatus::NeedsPermission => "needs-permission",
            SessionStatus::Idle => "idle",
            SessionStatus::Done => "done",
            SessionStatus::RateLimited => "rate-limited",
            SessionStatus::Crashed => "crashed",
            SessionStatus::Stopped => "stopped",
            SessionStatus::Offline => "offline",
        }
    }
}

/// Events emitted by the core. The UI subscribes via Tauri's event bus
/// (`event = "core-event"`). The CLI consumes the same enum off a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CoreEvent {
    /// Status changed for a session.
    StatusChanged {
        session_id: String,
        status: SessionStatus,
        reason: Option<String>,
    },
    /// One new stdout/stderr line captured from a session.
    LogLine {
        session_id: String,
        line: String,
        is_stderr: bool,
        at_ms: i64,
    },
    /// Recent conversation tail snapshot — sent on session-detail open and
    /// whenever the agent's local JSONL file grows substantially.
    TranscriptTail {
        session_id: String,
        lines: Vec<String>,
    },
    /// Remote-connect affordance from the agent (URL or QR token).
    RemoteAffordance {
        session_id: String,
        url: Option<String>,
        qr: Option<String>,
    },
    /// Agent emitted a "needs permission" event. Surfaces a native
    /// notification in the OS layer.
    NeedsPermission {
        session_id: String,
        prompt: Option<String>,
    },
    /// Whole config changed (debounced after a `sessions.toml` external edit).
    ConfigReloaded,
    /// External `sessions.toml` edit was malformed or rejected by the
    /// safety guards. Surfaces in the UI as a toast.
    ConfigError { message: String },
    /// Sleep-inhibitor state changed (drives the tray "Awake" badge).
    KeepAwakeChanged { active: bool, reason: String },
    /// The resolved binary path for an agent backend changed since last
    /// check — usually because the user upgraded (`brew upgrade
    /// claude-code`, npm reinstall, …). Running sessions are still on the
    /// old binary; the UI offers a "Restart all" action.
    BinaryUpgraded {
        backend_id: String,
        old_path: String,
        new_path: String,
    },
    /// Native-notification request from the supervisor. The Tauri runtime
    /// routes this through `tauri-plugin-notification` so the toast
    /// appears as "Session Manager" (the right app identity for the OS
    /// permission flow). Falls back to the OS layer's own notify() if
    /// the Tauri side isn't listening (CLI/daemon mode).
    NotifyRequested {
        title: String,
        body: String,
        urgent: bool,
    },
}

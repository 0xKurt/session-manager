//! Runtime state cache. Mirrors what the supervisor knows about each session
//! at this instant — never hand-edited (§7.7).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Result, SessionStatus};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionRuntime {
    pub id: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub status: SessionStatus,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_seen: Option<DateTime<Utc>>,
    #[serde(default)]
    pub restart_count: u32,
    #[serde(default)]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub remote_online: bool,
    #[serde(default)]
    pub last_activity: Option<String>,
    /// Pre-rendered QR SVG of `remote_url`. The supervisor generates it once
    /// when the URL is captured so the UI doesn't have to ship a QR
    /// generator in the renderer bundle.
    #[serde(default)]
    pub remote_qr: Option<String>,
    /// Path to the JSONL file claude is currently writing to. Captured
    /// after spawn (newest `.jsonl` in the project's claude dir whose
    /// mtime is >= our spawn time). Used on **restart** so we resume the
    /// EXACT same conversation via `claude --resume <path>` instead of
    /// `--continue`, which picks "the most recent" — non-deterministic
    /// when multiple sessions live in the same cwd.
    #[serde(default)]
    pub claude_jsonl_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeState {
    pub sessions: HashMap<String, SessionRuntime>,
    #[serde(default)]
    pub keep_awake_active: bool,
    /// Sessions the user explicitly stopped. **Persisted across app
    /// restarts** (used to be in-memory only, which meant relaunching
    /// the app respawned every auto_restart session even if the user
    /// had just stopped them on purpose). Cleared when the user starts
    /// the session again. Machine reboots no longer reset this either;
    /// if you want a session to come back at login, leave it running.
    #[serde(default)]
    pub intentionally_stopped: HashSet<String>,
}

impl RuntimeState {
    pub fn load_or_default(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn entry_mut(&mut self, id: &str) -> &mut SessionRuntime {
        self.sessions
            .entry(id.to_string())
            .or_insert_with(|| SessionRuntime {
                id: id.to_string(),
                ..Default::default()
            })
    }
}

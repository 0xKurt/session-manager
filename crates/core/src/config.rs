//! Declarative session config (§7.7) + read/write of `sessions.toml`.
//!
//! The TOML file is human-editable and is the source of truth. Runtime state
//! (PID, current status, remote URL, …) lives in [`crate::state::RuntimeState`]
//! — never written back here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{paths, Error, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    /// Agent must ask for everything, including reads (when supported).
    Safe,
    /// Agent asks before tool execution. §13.2 recommended default —
    /// `danger` should be a deliberate per-session choice.
    #[default]
    Ask,
    /// Skip-permissions — full danger. Surfaced unmistakably in the UI (§9.5).
    Danger,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum ResumeMode {
    /// `--continue` (or backend equivalent).
    #[default]
    Continue,
    /// `--resume <id>` — requires `resume_id`.
    Resume,
    /// No flag — start a fresh conversation.
    Fresh,
}

/// One declared session. Matches §7.7 schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub path: String,
    #[serde(default = "default_true")]
    pub remote: bool,
    #[serde(default)]
    pub permission: PermissionMode,
    #[serde(default)]
    pub resume: ResumeMode,
    #[serde(default)]
    pub resume_id: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub keep_awake: bool,
    #[serde(default = "default_true")]
    pub auto_restart: bool,
    #[serde(default = "default_restart_max")]
    pub restart_max: u32,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub log_path: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    /// If true, wrap the agent launch in `script -q <recording_path> <agent>
    /// <args>` so the full stdout transcript is captured to disk under a
    /// pseudo-tty. Mirrors the `script /tmp/rc_<name>.txt …` shell habit
    /// some users have today, without the user needing to remember to
    /// type it. Default off — `script` reserves a PTY and slightly changes
    /// the agent's I/O behaviour.
    #[serde(default)]
    pub record_stdout: bool,
    /// Additional command-line arguments appended after the ones the
    /// backend builds — lets the user pass any flag `claude` accepts
    /// (e.g. `--allowed-tools "Bash(git *)"`, `--add-dir /path`,
    /// `--append-system-prompt "…"`) without us having to expose every
    /// flag in the UI.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

fn default_true() -> bool {
    true
}
fn default_model() -> String {
    "default".to_string()
}
fn default_restart_max() -> u32 {
    5
}

impl SessionConfig {
    /// Resolve `path` to an absolute `PathBuf`, expanding `~` and env vars.
    pub fn resolved_path(&self) -> Result<PathBuf> {
        let expanded =
            shellexpand::full(&self.path).map_err(|e| Error::PathExpand(e.to_string()))?;
        Ok(PathBuf::from(expanded.into_owned()))
    }

    /// Resolve the log path (defaults to `<state>/logs/<id>.log`).
    pub fn resolved_log_path(&self) -> Result<PathBuf> {
        if let Some(p) = &self.log_path {
            let expanded = shellexpand::full(p).map_err(|e| Error::PathExpand(e.to_string()))?;
            Ok(PathBuf::from(expanded.into_owned()))
        } else {
            Ok(paths::log_dir()?.join(format!("{}.log", self.id)))
        }
    }
}

/// Defaults applied when the user creates a new session through the UI.
/// Editable in Settings (§9.3.D).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDefaults {
    #[serde(default = "default_agent_id")]
    pub agent: String,
    #[serde(default)]
    pub permission: PermissionMode,
    #[serde(default = "default_true")]
    pub remote: bool,
    #[serde(default)]
    pub resume: ResumeMode,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub keep_awake: bool,
    #[serde(default = "default_true")]
    pub auto_restart: bool,
}

fn default_agent_id() -> String {
    "claude-code".to_string()
}

impl Default for SessionDefaults {
    fn default() -> Self {
        Self {
            agent: default_agent_id(),
            permission: PermissionMode::default(),
            remote: true,
            resume: ResumeMode::default(),
            model: default_model(),
            keep_awake: false,
            auto_restart: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppPreferences {
    #[serde(default)]
    pub launch_at_login: bool,
    #[serde(default = "default_true")]
    pub notifications_enabled: bool,
    /// Hold an OS sleep-inhibitor whenever ANY managed session is in a
    /// running state. Single global switch — the previous per-session
    /// `keep_awake` flag was dropped because "either the machine is awake
    /// or it isn't" makes a per-session scoping knob a footgun. §8.6.
    #[serde(default = "default_true")]
    pub keep_awake_master: bool,
    #[serde(default)]
    pub defaults: SessionDefaults,
    /// Once the user has either applied or dismissed the "power-user
    /// defaults" suggestion, don't ask again.
    #[serde(default)]
    pub power_user_prompt_dismissed: bool,
    /// Once the user has either enabled or dismissed the "survive reboot"
    /// suggestion, don't ask again.
    #[serde(default)]
    pub launch_at_login_prompt_dismissed: bool,
}

/// The on-disk shape of `sessions.toml`.
///
/// Field name is `sessions` (plural) — keeps JSON/IPC and frontend types
/// aligned. The `alias = "session"` lets us still read existing files
/// that used the older singular `[[session]]` convention.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionsFile {
    #[serde(default)]
    pub preferences: AppPreferences,
    #[serde(default, alias = "session")]
    pub sessions: Vec<SessionConfig>,
}

impl SessionsFile {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        let file: SessionsFile = toml::from_str(&raw)?;
        Ok(file)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::BadConfigPath(path.to_path_buf()))?;
        std::fs::create_dir_all(parent)?;
        let body = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn find(&self, id: &str) -> Option<&SessionConfig> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut SessionConfig> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn upsert(&mut self, session: SessionConfig) {
        match self.sessions.iter_mut().find(|s| s.id == session.id) {
            Some(existing) => *existing = session,
            None => self.sessions.push(session),
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<SessionConfig> {
        let idx = self.sessions.iter().position(|s| s.id == id)?;
        Some(self.sessions.remove(idx))
    }
}

//! Agent backend abstraction (§7.4). Adding an agent = implementing this
//! trait — nothing else touches it.

pub mod claude_code;
pub mod codex;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Error, Result, SessionConfig, SessionStatus};

/// One process Session Manager discovered running on the user's machine
/// that *wasn't* started through Session Manager. Surfaced in the UI as
/// "External" so the user can see what's already there and (optionally)
/// adopt it into a managed session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredSession {
    pub pid: u32,
    pub backend_id: String,
    pub display_name: String,
    pub cwd: PathBuf,
    pub args: Vec<String>,
    /// Hint for the UI: matches an `id` already in the user's sessions.toml
    /// at the same cwd — meaning this process is "the unmanaged copy" of an
    /// existing config entry.
    #[serde(default)]
    pub matches_session_id: Option<String>,
    /// Live activity probe derived from the agent's local JSONL/transcript
    /// for this `cwd`. Populated by `Supervisor::external_sessions` so the
    /// UI can show working / needs-permission / idle on external rows the
    /// same way as managed ones.
    #[serde(default)]
    pub status: Option<crate::SessionStatus>,
    /// `last_activity` timestamp from the agent's transcript, if known.
    #[serde(default)]
    pub last_activity: Option<String>,
}

/// Environment variables that are safe to inherit from the supervisor's own
/// environment into spawned agent processes (§10 — we don't want to leak the
/// full host env). Anything else must come from `SessionConfig::env`.
const SAFE_INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "TZ",
    // Mac-specific tooling sometimes needs these:
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
];

/// Build the minimum-viable environment for a spawned agent.
pub fn minimal_env(session: &SessionConfig) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars()
        .filter(|(k, _)| SAFE_INHERITED_ENV.contains(&k.as_str()))
        .collect();
    for (k, v) in &session.env {
        env.insert(k.clone(), v.clone());
    }
    env
}

/// Wrap an already-built `LaunchSpec` in `script -q <path> <program> <args>`
/// when the session has `record_stdout = true`. The wrapper gives the
/// agent a real PTY and writes a full transcript to disk — same effect
/// as the user typing `script /tmp/rc_foo.txt claude ...` by hand.
///
/// The recording file lives under our state dir per session, so it
/// survives restart and doesn't pile up in `/tmp`.
pub fn maybe_wrap_with_script(spec: LaunchSpec, session: &SessionConfig) -> Result<LaunchSpec> {
    if !session.record_stdout {
        return Ok(spec);
    }
    #[cfg(unix)]
    {
        let recordings = crate::paths::state_dir()?.join("recordings");
        std::fs::create_dir_all(&recordings).map_err(Error::Io)?;
        let recording_path = recordings.join(format!("{}.log", session.id));
        // macOS `script` syntax: `script -q file command [args ...]`.
        // The original program becomes the wrapper's command argument.
        let mut wrapped_args = vec![
            "-q".to_string(),
            recording_path.to_string_lossy().into_owned(),
            spec.program.to_string_lossy().into_owned(),
        ];
        wrapped_args.extend(spec.args);
        Ok(LaunchSpec {
            program: std::path::PathBuf::from("/usr/bin/script"),
            args: wrapped_args,
            cwd: spec.cwd,
            env: spec.env,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = session;
        Ok(spec)
    }
}

/// What a backend resolves a [`SessionConfig`] into when launched.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
}

/// Snapshot of agent activity, derived from the agent's local artifacts
/// (e.g. Claude Code JSONL). The supervisor merges this with process state.
#[derive(Debug, Clone, Default)]
pub struct ActivityProbe {
    pub status: Option<SessionStatus>,
    pub last_activity: Option<String>,
    pub recent_lines: Vec<String>,
    pub remote_url: Option<String>,
    pub remote_online: Option<bool>,
}

/// The result of an auth check.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthState {
    /// Binary resolves AND credentials are present.
    LoggedIn,
    /// Binary resolves but no credentials.
    LoggedOut,
    /// Binary is not on PATH (or shell login PATH).
    BinaryMissing,
    /// Couldn't determine — backend declined to answer.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendInfo {
    pub id: String,
    pub display_name: String,
}

#[async_trait]
pub trait Backend: Send + Sync {
    fn info(&self) -> BackendInfo;

    /// Resolve the agent binary (must handle version-manager shims; see §7.4).
    fn resolve_binary(&self) -> Result<PathBuf>;

    /// Build argv + env + cwd for a given session config.
    ///
    /// `resume_hint` (when present) is the agent-native artifact captured
    /// from a prior run via `discover_session_file` — e.g. for claude, a
    /// path to `<uuid>.jsonl`. The backend should prefer `--resume <hint>`
    /// over `--continue` so restarts are deterministic. `None` means
    /// "first start / no prior artifact known" — fall back to the
    /// session's configured resume mode.
    fn build_launch(
        &self,
        session: &SessionConfig,
        resume_hint: Option<&str>,
    ) -> Result<LaunchSpec>;

    /// Read the agent's local artifacts and report current activity state.
    async fn probe_activity(&self, session: &SessionConfig) -> Result<ActivityProbe>;

    /// Check whether the user is logged in to this agent.
    async fn auth_state(&self) -> Result<AuthState>;

    /// Find agent processes the user has launched outside Session Manager.
    /// Default: nothing. Backends opt in by implementing this.
    fn discover_external(&self) -> Vec<DiscoveredSession> {
        Vec::new()
    }

    /// After spawn, identify the on-disk artifact (e.g. claude's
    /// `<uuid>.jsonl` transcript) that this session is writing to.
    ///
    /// `pre_snapshot` is `(path, mtime)` for every transcript in the
    /// project dir at the moment immediately BEFORE we exec'd the agent.
    /// The implementation should pick a file that has either appeared
    /// since the snapshot or whose mtime jumped past its snapshotted
    /// value — that disambiguates "the jsonl OUR spawn is writing to"
    /// from "some other concurrent claude in the same cwd happened to
    /// touch its jsonl in the same second."
    ///
    /// `spawn_at` is a redundant fallback used only when the snapshot is
    /// empty (fresh project dir) — we accept any file modified at-or-
    /// after spawn_at.
    fn discover_session_file(
        &self,
        _session: &SessionConfig,
        _pre_snapshot: &std::collections::HashMap<std::path::PathBuf, std::time::SystemTime>,
        _spawn_at: chrono::DateTime<chrono::Utc>,
    ) -> Option<String> {
        None
    }

    /// Snapshot the transcripts that exist for `session` *now*, returned
    /// as `(path, mtime)`. Called once right before spawn so the
    /// post-spawn `discover_session_file` can compare deltas and pick the
    /// exact file the new process is appending to.
    fn snapshot_session_files(
        &self,
        _session: &SessionConfig,
    ) -> std::collections::HashMap<std::path::PathBuf, std::time::SystemTime> {
        Default::default()
    }
}

/// Best-effort binary resolution via the user's login shell, lifted from
/// Claude-God's fix for nvm/asdf-style installs (§7.4 / §7.9).
pub fn shell_which(bin: &str) -> Result<PathBuf> {
    if let Ok(p) = which_via_login_shell(bin) {
        return Ok(p);
    }
    // Fallback: a plain PATH lookup.
    if let Some(p) = direct_which(bin) {
        return Ok(p);
    }
    Err(Error::BinaryNotFound {
        backend: bin.into(),
        detail: "not found via login shell or PATH".into(),
    })
}

fn which_via_login_shell(bin: &str) -> Result<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(target_os = "windows") {
            "cmd.exe".into()
        } else {
            "/bin/sh".into()
        }
    });
    let cmd = if cfg!(target_os = "windows") {
        Command::new(&shell)
            .args(["-l", "-c", &format!("where {bin}")])
            .output()
    } else {
        Command::new(&shell)
            .args(["-l", "-c", &format!("which {bin}")])
            .output()
    };
    let out = cmd.map_err(|e| Error::BinaryNotFound {
        backend: bin.into(),
        detail: format!("spawn shell `{shell}`: {e}"),
    })?;
    if !out.status.success() {
        return Err(Error::BinaryNotFound {
            backend: bin.into(),
            detail: format!(
                "{} {bin} exited {}",
                if cfg!(windows) { "where" } else { "which" },
                out.status
            ),
        });
    }
    let path = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if path.is_empty() {
        return Err(Error::BinaryNotFound {
            backend: bin.into(),
            detail: "empty path from login shell".into(),
        });
    }
    Ok(PathBuf::from(path))
}

fn direct_which(bin: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = if cfg!(windows) {
            dir.join(format!("{bin}.exe"))
        } else {
            dir.join(bin)
        };
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Look up a backend by id. The set of backends is static for now (§8.8).
pub fn make_backend(agent_id: &str) -> Result<Box<dyn Backend>> {
    match agent_id {
        "claude-code" => Ok(Box::new(claude_code::ClaudeCodeBackend)),
        "codex" => Ok(Box::new(codex::CodexBackend)),
        other => Err(Error::UnknownBackend(other.to_string())),
    }
}

/// List of backends known to the build, for the UI's "agent" picker.
pub fn registry() -> Vec<BackendInfo> {
    vec![
        BackendInfo {
            id: "claude-code".into(),
            display_name: "Claude Code".into(),
        },
        BackendInfo {
            id: "codex".into(),
            display_name: "Codex".into(),
        },
    ]
}

/// Walk `ps -A` once and return (pid, command, cwd) tuples for every
/// process whose command line contains any of `needles`. cwd is resolved
/// via `lsof -a -d cwd`. Cheap enough to call every few seconds.
#[cfg(unix)]
pub fn scan_processes(needles: &[&str]) -> Vec<(u32, String, PathBuf)> {
    let Ok(out) = Command::new("ps")
        .args(["-A", "-o", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    let raw = String::from_utf8_lossy(&out.stdout);
    let mut hits = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let Some((pid_str, rest)) = trimmed.split_once(' ') else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let cmd = rest.trim().to_string();
        if !needles.iter().any(|n| cmd.contains(n)) {
            continue;
        }
        if is_self_noise(&cmd) {
            continue;
        }
        let cwd = lookup_cwd(pid).unwrap_or_else(|| PathBuf::from("/"));
        hits.push((pid, cmd, cwd));
    }
    hits
}

#[cfg(not(unix))]
pub fn scan_processes(_needles: &[&str]) -> Vec<(u32, String, PathBuf)> {
    Vec::new()
}

/// Filter out processes that aren't agents themselves — our own supervisor
/// binary, ps/lsof/grep noise, internal Claude bg-pty-host subprocesses.
fn is_self_noise(cmd: &str) -> bool {
    cmd.contains("session-manager") && cmd.contains("/target/")
        || cmd.contains("--bg-pty-host")
        || cmd.starts_with("ps ")
        || cmd.starts_with("lsof ")
}

#[cfg(unix)]
fn lookup_cwd(pid: u32) -> Option<PathBuf> {
    let out = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-F", "n", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let raw = String::from_utf8_lossy(&out.stdout);
    for line in raw.lines() {
        if let Some(stripped) = line.strip_prefix('n') {
            return Some(PathBuf::from(stripped));
        }
    }
    None
}

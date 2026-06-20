//! Claude Code backend.
//!
//! Launch (example from §7.4):
//!   claude --remote-control "<name>" --dangerously-skip-permissions --continue
//! Status detection: tail `~/.claude/projects/**/*.jsonl` and map last entry
//! type (§7.6).

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use super::{
    maybe_wrap_with_script, minimal_env, scan_processes, shell_which, ActivityProbe, AuthState,
    Backend, BackendInfo, DiscoveredSession, LaunchSpec,
};
use crate::{Error, PermissionMode, Result, ResumeMode, SessionConfig, SessionStatus};

pub struct ClaudeCodeBackend;

#[async_trait]
impl Backend for ClaudeCodeBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            id: "claude-code".into(),
            display_name: "Claude Code".into(),
        }
    }

    fn resolve_binary(&self) -> Result<PathBuf> {
        shell_which("claude")
    }

    fn build_launch(
        &self,
        session: &SessionConfig,
        resume_hint: Option<&str>,
    ) -> Result<LaunchSpec> {
        let program = self.resolve_binary()?;
        let mut args: Vec<String> = Vec::new();

        if session.remote {
            // Native remote-control mode (§7.5).
            args.push("--remote-control".into());
            args.push(session.name.clone());
        }

        match session.permission {
            PermissionMode::Safe => {
                args.push("--permission-mode".into());
                args.push("safe".into());
            }
            PermissionMode::Ask => {
                // default — no flag needed
            }
            PermissionMode::Danger => {
                args.push("--dangerously-skip-permissions".into());
            }
        }

        // If a prior run captured the exact JSONL claude was writing to,
        // resume *that* file — deterministic across restarts, immune to
        // "which session does --continue pick?" when several share a
        // project dir. Only takes effect for resume modes that imply
        // resuming; Fresh stays fresh.
        let used_hint = match (session.resume.clone(), resume_hint) {
            (ResumeMode::Continue | ResumeMode::Resume, Some(path)) if !path.is_empty() => {
                args.push("--resume".into());
                args.push(path.to_string());
                true
            }
            _ => false,
        };
        if !used_hint {
            match session.resume {
                ResumeMode::Continue => args.push("--continue".into()),
                ResumeMode::Resume => {
                    if session.resume_id.is_empty() {
                        return Err(Error::Backend(
                            "resume = \"resume\" requires resume_id".into(),
                        ));
                    }
                    args.push("--resume".into());
                    args.push(session.resume_id.clone());
                }
                ResumeMode::Fresh => {}
            }
        }

        if !session.model.is_empty() && session.model != "default" {
            args.push("--model".into());
            args.push(session.model.clone());
        }
        // Power-user escape hatch — anything the user typed in the
        // "Extra arguments" field is appended verbatim. Lets us cover
        // every claude flag without hard-coding each one in the UI.
        for extra in &session.extra_args {
            if !extra.trim().is_empty() {
                args.push(extra.clone());
            }
        }

        let cwd = session.resolved_path()?;
        let env = minimal_env(session);
        let spec = LaunchSpec {
            program,
            args,
            cwd,
            env,
        };
        // Claude Code is interactive: it expects a real PTY and exits with
        // code 1 if it can't open one. Our worker runs the child detached
        // (no controlling terminal), so we *always* wrap the launch in
        // `script(1)` for Claude. `record_stdout=false` still keeps the
        // recording (it has to land somewhere) — the field instead
        // controls whether we expose it in the UI. The cost is a 0-byte
        // recording file when nothing was written; harmless.
        let mut session_pty = session.clone();
        session_pty.record_stdout = true;
        maybe_wrap_with_script(spec, &session_pty)
    }

    async fn probe_activity(&self, session: &SessionConfig) -> Result<ActivityProbe> {
        let cwd = session.resolved_path()?;
        let project_dir = jsonl_dir_for(&cwd);
        let Some(latest) = newest_jsonl(&project_dir) else {
            return Ok(ActivityProbe::default());
        };

        // We only need the last ~40 lines. Read the *tail* of the file
        // instead of slurping the whole thing — long-lived Claude
        // sessions produce JSONLs that grow to many MB and we'd otherwise
        // re-read them every probe period.
        let raw = read_tail(&latest, 256 * 1024).await.unwrap_or_default();
        let lines: Vec<&str> = raw.lines().rev().take(40).collect();
        let mut recent: Vec<String> = lines.iter().rev().map(|s| s.to_string()).collect();

        let mut status: Option<SessionStatus> = None;
        let mut last_activity: Option<String> = None;
        let mut remote_url: Option<String> = None;
        let mut remote_online: Option<bool> = None;

        for line in lines.iter() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match kind {
                // Active conversation = working.
                "tool_use" | "assistant" | "user" => {
                    if status.is_none() {
                        status = Some(SessionStatus::Working);
                    }
                }
                // End of a turn / cycle — agent waiting for next user input.
                "result" => {
                    let sub = v.get("subtype").and_then(|x| x.as_str()).unwrap_or("");
                    if sub == "error_max_turns" || sub == "rate_limit" {
                        status = Some(SessionStatus::RateLimited);
                    } else {
                        status = Some(SessionStatus::Idle);
                    }
                }
                "needs_permission" | "tool_permission_request" => {
                    status = Some(SessionStatus::NeedsPermission);
                }
                "remote_control" => {
                    if let Some(url) = v.get("url").and_then(|x| x.as_str()) {
                        remote_url = Some(url.to_string());
                    }
                    if let Some(state) = v.get("state").and_then(|x| x.as_str()) {
                        remote_online = Some(state == "connected" || state == "online");
                    }
                }
                // Housekeeping types Claude writes between turns — agent
                // is alive, just not actively responding. Most live
                // sessions land here when the user opens the dashboard.
                "bridge-session" | "permission-mode" | "mode" | "ai-title"
                | "file-history-snapshot" | "system" | "summary" | "session-start"
                | "tool_result" => {
                    if status.is_none() {
                        status = Some(SessionStatus::Idle);
                    }
                }
                _ => {}
            }
            if last_activity.is_none() {
                last_activity = v
                    .get("timestamp")
                    .and_then(|x| x.as_str())
                    .map(str::to_string);
            }
        }

        // Final fallback — if the JSONL was modified recently but we
        // didn't recognise any type, the agent is still alive. Mark Idle
        // rather than leaving status as None (which keeps the worker's
        // last-set "starting" stuck on the UI forever).
        if status.is_none() {
            if let Ok(meta) = tokio::fs::metadata(&latest).await {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(age) = std::time::SystemTime::now().duration_since(mtime) {
                        if age < std::time::Duration::from_secs(15 * 60) {
                            status = Some(SessionStatus::Idle);
                        }
                    }
                }
            }
        }

        // Trim recent JSONL down to a smaller readable form
        if recent.len() > 12 {
            let start = recent.len() - 12;
            recent = recent[start..].to_vec();
        }

        Ok(ActivityProbe {
            status,
            last_activity,
            recent_lines: recent,
            remote_url,
            remote_online,
        })
    }

    fn discover_session_file(
        &self,
        session: &SessionConfig,
        pre_snapshot: &std::collections::HashMap<std::path::PathBuf, std::time::SystemTime>,
        spawn_at: chrono::DateTime<chrono::Utc>,
    ) -> Option<String> {
        // We're looking for the JSONL **this** spawn is writing to. Two
        // cases:
        //   1. claude --continue / --resume — appends to an existing
        //      file. Its mtime jumps past whatever value the pre-spawn
        //      snapshot recorded for that path.
        //   2. fresh start — claude creates a new `<uuid>.jsonl`. That
        //      path is NOT in the pre-snapshot at all.
        //
        // For both cases the deterministic answer is "the file whose
        // (path, mtime) tuple has changed the most relative to the
        // snapshot" — i.e. a new file always beats a touched file
        // (rank 1), and among touched files we pick the largest mtime
        // jump. That dodges the audit's race where a *concurrent* claude
        // in the same cwd touches its own jsonl in the same second and
        // beats ours on raw mtime.
        let cwd = session.resolved_path().ok()?;
        let project_dir = jsonl_dir_for(&cwd);
        let entries = std::fs::read_dir(&project_dir).ok()?;
        let spawn_st = chrono_to_system_time(spawn_at);

        #[derive(Clone)]
        struct Cand {
            path: std::path::PathBuf,
            mtime: std::time::SystemTime,
            delta: std::time::Duration,
        }
        let mut new_files: Vec<Cand> = Vec::new();
        let mut touched: Vec<Cand> = Vec::new();
        for ent in entries.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = ent.metadata() else { continue };
            let Ok(mt) = meta.modified() else { continue };
            match pre_snapshot.get(&p) {
                None => {
                    // File didn't exist at snapshot time → it's new.
                    new_files.push(Cand {
                        path: p,
                        mtime: mt,
                        delta: std::time::Duration::ZERO,
                    });
                }
                Some(prev_mt) => {
                    // Pre-existing file. Did its mtime advance past
                    // snapshot value? If yes, claude is appending.
                    if let Ok(delta) = mt.duration_since(*prev_mt) {
                        if !delta.is_zero() {
                            touched.push(Cand {
                                path: p,
                                mtime: mt,
                                delta,
                            });
                        }
                    }
                }
            }
        }

        // Prefer new files (no ambiguity: it didn't exist when we
        // snapshotted). If multiple, pick the most recent mtime.
        if !new_files.is_empty() {
            new_files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
            return Some(new_files[0].path.to_string_lossy().into_owned());
        }

        // Otherwise pick the touched file with the largest delta — that's
        // the one we just nudged via claude's append.
        if !touched.is_empty() {
            touched.sort_by(|a, b| b.delta.cmp(&a.delta));
            return Some(touched[0].path.to_string_lossy().into_owned());
        }

        // Snapshot was empty (fresh project dir). Fall back to "newest
        // mtime at-or-after spawn_at" so first-time discovery still
        // works without any pre-snapshot.
        let entries2 = std::fs::read_dir(&project_dir).ok()?;
        let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
        for ent in entries2.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = ent.metadata() else { continue };
            let Ok(mt) = meta.modified() else { continue };
            let mt_ok = mt
                >= spawn_st
                    .checked_sub(std::time::Duration::from_secs(2))
                    .unwrap_or(spawn_st);
            if !mt_ok {
                continue;
            }
            if best.as_ref().is_none_or(|(b, _)| mt > *b) {
                best = Some((mt, p));
            }
        }
        best.map(|(_, p)| p.to_string_lossy().into_owned())
    }

    fn snapshot_session_files(
        &self,
        session: &SessionConfig,
    ) -> std::collections::HashMap<std::path::PathBuf, std::time::SystemTime> {
        let Ok(cwd) = session.resolved_path() else {
            return Default::default();
        };
        let project_dir = jsonl_dir_for(&cwd);
        let Ok(entries) = std::fs::read_dir(&project_dir) else {
            return Default::default();
        };
        let mut out = std::collections::HashMap::new();
        for ent in entries.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(meta) = ent.metadata() {
                if let Ok(mt) = meta.modified() {
                    out.insert(p, mt);
                }
            }
        }
        out
    }

    fn discover_external(&self) -> Vec<DiscoveredSession> {
        // Match: `claude` invocations on the user's machine that we didn't
        // start. The `script` wrapper your existing setup uses
        // (`script /tmp/rc_<name>.txt claude --remote-control <name> ...`)
        // is matched too — we just look for `claude --remote-control` and
        // also bare `claude` interactive sessions.
        let candidates = scan_processes(&[
            "claude --remote-control",
            "claude --continue",
            "claude --resume",
        ]);
        let mut out = Vec::new();
        for (pid, cmd, cwd) in candidates {
            let display_name = extract_remote_name(&cmd)
                .unwrap_or_else(|| cwd_basename(&cwd).unwrap_or_else(|| format!("claude #{pid}")));
            out.push(DiscoveredSession {
                pid,
                backend_id: "claude-code".into(),
                display_name,
                cwd,
                args: split_command(&cmd),
                matches_session_id: None,
                status: None,
                last_activity: None,
            });
        }
        out
    }

    async fn auth_state(&self) -> Result<AuthState> {
        // 1. Binary must be resolvable via the user's login shell. If `claude`
        //    isn't on PATH the rest of the check is moot.
        if self.resolve_binary().is_err() {
            return Ok(AuthState::BinaryMissing);
        }
        // 2. Find credentials. Claude Code uses three storage strategies
        //    depending on version + OS:
        //      • macOS Keychain (default since mid-2025) — primary check.
        //      • `~/.claude/.credentials.json` — older / non-Keychain hosts.
        //      • `~/.claude/config.json` "oauth" field — early versions.
        //    Try Keychain first on macOS; if we get a JSON blob with a
        //    token field, we're logged in. Otherwise fall back to file.
        #[cfg(target_os = "macos")]
        {
            if let Some(blob) = read_macos_keychain() {
                if blob.contains("Token") || blob.contains("token") {
                    return Ok(AuthState::LoggedIn);
                }
            }
        }
        let Some(home) = dirs::home_dir() else {
            return Ok(AuthState::Unknown);
        };
        for candidate in [".credentials.json", "config.json", "auth.json"] {
            let path = home.join(".claude").join(candidate);
            if !path.is_file() {
                continue;
            }
            let raw = match tokio::fs::read_to_string(&path).await {
                Ok(r) => r,
                Err(_) => continue,
            };
            if raw.trim().is_empty() {
                continue;
            }
            let parsed: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if has_token_field(&parsed) {
                return Ok(AuthState::LoggedIn);
            }
        }
        Ok(AuthState::LoggedOut)
    }
}

/// Read the tail of `path`, at most `max_bytes`. Lossy-decodes bytes that
/// don't form a valid UTF-8 boundary (we trim back to the first newline
/// for clean line parsing).
async fn read_tail(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
    let mut file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let start = len.saturating_sub(max_bytes);
    if start > 0 {
        file.seek(SeekFrom::Start(start)).await?;
    }
    let mut buf = Vec::with_capacity(max_bytes.min(len) as usize);
    file.read_to_end(&mut buf).await?;
    // If we mid-cut a UTF-8 char or line, drop up to the first newline.
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        if let Some(idx) = s.find('\n') {
            s = s.split_off(idx + 1);
        }
    }
    Ok(s)
}

fn extract_remote_name(cmd: &str) -> Option<String> {
    // Look for "claude --remote-control <name>" — the name is the token
    // immediately after.
    let tokens: Vec<&str> = cmd.split_ascii_whitespace().collect();
    let pos = tokens.iter().position(|t| *t == "--remote-control")?;
    tokens.get(pos + 1).map(|s| s.to_string())
}

fn cwd_basename(p: &Path) -> Option<String> {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

fn split_command(cmd: &str) -> Vec<String> {
    // Lightweight split; we don't need shell-correct quoting because the
    // result is only displayed.
    cmd.split_ascii_whitespace().map(str::to_string).collect()
}

#[cfg(target_os = "macos")]
fn read_macos_keychain() -> Option<String> {
    use std::process::Command;
    // `security find-generic-password -s "<svc>" -w` writes the password to
    // stdout. Returns non-zero if the item isn't there or the user denies
    // access. We just want to know whether we got *anything* token-shaped.
    for service in &["Claude Code-credentials", "Claude-credentials"] {
        let out = Command::new("security")
            .args(["find-generic-password", "-s", service, "-w"])
            .output()
            .ok()?;
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

fn has_token_field(v: &serde_json::Value) -> bool {
    const KEYS: &[&str] = &[
        "access_token",
        "accessToken",
        "refresh_token",
        "refreshToken",
        "session_token",
        "sessionKey",
        "apiKey",
        "api_key",
        "token",
    ];
    fn obj_has(o: &serde_json::Map<String, serde_json::Value>) -> bool {
        for k in KEYS {
            if let Some(v) = o.get(*k) {
                if v.as_str().map(|s| !s.is_empty()).unwrap_or(false) {
                    return true;
                }
            }
        }
        false
    }
    if let Some(o) = v.as_object() {
        if obj_has(o) {
            return true;
        }
        for (_, child) in o {
            if let Some(co) = child.as_object() {
                if obj_has(co) {
                    return true;
                }
            }
        }
    }
    false
}

/// `~/.claude/projects/<encoded-cwd>` per Claude Code's local convention:
/// strip leading `/`, replace path separators with `-`, prepend `-`.
///
/// We deliberately do NOT fall back to "find a subdir that looks similar":
/// Claude's encoding decisions change across versions, and decoding `-` back
/// to `/` destroys legitimate hyphens in project names (`code/my-app` would
/// be looked up as `code/my/app`). If the primary path doesn't exist we
/// return it anyway and let `newest_jsonl` return `None`, which the probe
/// treats as "no activity yet". That's accurate; bad heuristics are not.
fn jsonl_dir_for(cwd: &Path) -> PathBuf {
    let Some(home) = dirs::home_dir() else {
        return PathBuf::new();
    };
    home.join(".claude")
        .join("projects")
        .join(encoded_cwd_name(cwd))
}

fn encoded_cwd_name(cwd: &Path) -> String {
    let mut encoded = cwd.to_string_lossy().to_string();
    if encoded.starts_with('/') {
        encoded.remove(0);
    }
    encoded = encoded.replace('/', "-");
    format!("-{encoded}")
}

/// Translate a chrono UTC timestamp to a `SystemTime`. Saturates on
/// negative / out-of-range values (the audit flagged the old `as u64`
/// cast as a panic risk for pre-1970 inputs).
fn chrono_to_system_time(t: chrono::DateTime<chrono::Utc>) -> std::time::SystemTime {
    let secs = t.timestamp();
    if secs >= 0 {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64)
    } else {
        std::time::UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs((-secs) as u64))
            .unwrap_or(std::time::UNIX_EPOCH)
    }
}

fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for ent in entries.flatten() {
        let p = ent.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = ent.metadata() else { continue };
        let Ok(mt) = meta.modified() else { continue };
        if best.as_ref().is_none_or(|(b, _)| mt > *b) {
            best = Some((mt, p));
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn read_tail_short_file() {
        let tmp = std::env::temp_dir().join("sm-rt-short.txt");
        let _ = tokio::fs::remove_file(&tmp).await;
        let mut f = tokio::fs::File::create(&tmp).await.unwrap();
        f.write_all(b"hello\nworld\n").await.unwrap();
        f.flush().await.unwrap();
        let out = read_tail(&tmp, 64 * 1024).await.unwrap();
        assert_eq!(out, "hello\nworld\n");
    }

    #[tokio::test]
    async fn read_tail_trims_partial_first_line() {
        let tmp = std::env::temp_dir().join("sm-rt-trim.txt");
        let _ = tokio::fs::remove_file(&tmp).await;
        let mut f = tokio::fs::File::create(&tmp).await.unwrap();
        for i in 0..100 {
            f.write_all(format!("line {i}\n").as_bytes()).await.unwrap();
        }
        f.flush().await.unwrap();
        // Read a small tail — guaranteed to land mid-line on the seek.
        let out = read_tail(&tmp, 32).await.unwrap();
        assert!(out.ends_with('\n'));
        // Every line in the output must be a complete "line N" — the
        // partial first line that the raw seek would expose has been
        // trimmed.
        for l in out.lines() {
            assert!(
                l.starts_with("line ") && l[5..].parse::<u32>().is_ok(),
                "leaked partial line: {l:?}"
            );
        }
        // And we got the actual tail content, not random earlier lines.
        assert!(out.contains("line 99"));
    }

    #[test]
    fn extract_remote_name_parses() {
        assert_eq!(
            extract_remote_name("claude --remote-control foo --bar"),
            Some("foo".into())
        );
        assert_eq!(
            extract_remote_name("script /tmp/x claude --remote-control trading-bot"),
            Some("trading-bot".into())
        );
        assert_eq!(extract_remote_name("claude --continue"), None);
    }
}

//! Codex backend. The exact flag set for Codex's remote handoff is one of the
//! open questions in spec §13.5 — this impl is structured so swapping the
//! actual flags later is a localized change.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use super::{
    maybe_wrap_with_script, minimal_env, scan_processes, shell_which, ActivityProbe, AuthState,
    Backend, BackendInfo, DiscoveredSession, LaunchSpec,
};
use crate::{PermissionMode, Result, ResumeMode, SessionConfig, SessionStatus};

pub struct CodexBackend;

#[async_trait]
impl Backend for CodexBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            id: "codex".into(),
            display_name: "Codex".into(),
        }
    }

    fn resolve_binary(&self) -> Result<PathBuf> {
        shell_which("codex")
    }

    fn build_launch(
        &self,
        session: &SessionConfig,
        _resume_hint: Option<&str>,
    ) -> Result<LaunchSpec> {
        let program = self.resolve_binary()?;
        let mut args: Vec<String> = Vec::new();

        // Codex remote handoff — placeholder flag, confirm per §13.5.
        if session.remote {
            args.push("--remote".into());
        }

        match session.permission {
            PermissionMode::Safe => {
                args.push("--ask-for-approval".into());
                args.push("untrusted".into());
            }
            PermissionMode::Ask => {
                args.push("--ask-for-approval".into());
                args.push("on-request".into());
            }
            PermissionMode::Danger => {
                args.push("--dangerously-bypass-approvals-and-sandbox".into());
            }
        }

        match session.resume {
            ResumeMode::Continue => args.push("--continue".into()),
            ResumeMode::Resume => {
                if !session.resume_id.is_empty() {
                    args.push("--resume".into());
                    args.push(session.resume_id.clone());
                }
            }
            ResumeMode::Fresh => {}
        }

        if !session.model.is_empty() && session.model != "default" {
            args.push("--model".into());
            args.push(session.model.clone());
        }
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
        maybe_wrap_with_script(spec, session)
    }

    async fn probe_activity(&self, _session: &SessionConfig) -> Result<ActivityProbe> {
        // TODO §13.5: read Codex's local session artifacts. For now we report
        // "no activity reading available" so the supervisor falls back to
        // pure process-state.
        Ok(ActivityProbe {
            status: Some(SessionStatus::Idle),
            ..Default::default()
        })
    }

    fn discover_external(&self) -> Vec<DiscoveredSession> {
        // Codex's CLI command shape isn't pinned (§13.5 in the spec) — we
        // match conservatively on the binary name plus any flag we think
        // is likely. If the user invokes Codex differently, this returns
        // empty and the UI just doesn't show it.
        let candidates = scan_processes(&[
            "codex --remote",
            "codex --resume",
            "codex --continue",
            " codex ",
        ]);
        let mut out = Vec::new();
        for (pid, cmd, cwd) in candidates {
            let display_name = cwd
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("codex #{pid}"));
            out.push(DiscoveredSession {
                pid,
                backend_id: "codex".into(),
                display_name,
                cwd,
                args: cmd.split_ascii_whitespace().map(str::to_string).collect(),
                matches_session_id: None,
                status: None,
                last_activity: None,
            });
        }
        out
    }

    async fn auth_state(&self) -> Result<AuthState> {
        if self.resolve_binary().is_err() {
            return Ok(AuthState::BinaryMissing);
        }
        let Some(home) = dirs::home_dir() else {
            return Ok(AuthState::Unknown);
        };
        let dir = home.join(".codex");
        if codex_credentials_present(&dir) {
            Ok(AuthState::LoggedIn)
        } else {
            Ok(AuthState::LoggedOut)
        }
    }
}

fn codex_credentials_present(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    for candidate in &["auth.json", "credentials.json", "session.json"] {
        if dir.join(candidate).is_file() {
            return true;
        }
    }
    false
}

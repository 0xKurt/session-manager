//! Tauri command surface (§7.3). The UI is thin: it sends commands and
//! consumes the `core-event` stream.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use session_manager_core::backend::{AuthState, BackendInfo};
use session_manager_core::config::{AppPreferences, SessionDefaults};
use session_manager_core::state::{RuntimeState, SessionRuntime};
use session_manager_core::{Result, SessionConfig, SessionStatus, SessionsFile};
use tauri::State;

use crate::AppState;

#[derive(Serialize)]
pub struct ListSessionsResp {
    pub file: SessionsFile,
    pub runtime: RuntimeState,
}

#[tauri::command]
pub async fn list_sessions(
    state: State<'_, AppState>,
) -> std::result::Result<ListSessionsResp, String> {
    let (file, runtime) = state.supervisor.snapshot().await;
    Ok(ListSessionsResp { file, runtime })
}

#[tauri::command]
pub async fn session_runtime_snapshot(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<Option<SessionRuntime>, String> {
    let (_, runtime) = state.supervisor.snapshot().await;
    Ok(runtime.sessions.get(&id).cloned())
}

#[tauri::command]
pub async fn start_session(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    state
        .supervisor
        .start_session(&id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_session(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    // user_initiated=true — Tauri IPC means the user clicked Stop in
    // the popover or main window.
    state
        .supervisor
        .stop_session(&id, true)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn restart_session(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    state
        .supervisor
        .restart_session(&id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_session(
    state: State<'_, AppState>,
    session: SessionConfig,
) -> std::result::Result<(), String> {
    state
        .supervisor
        .create_session(session)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_session(
    state: State<'_, AppState>,
    session: SessionConfig,
) -> std::result::Result<(), String> {
    state
        .supervisor
        .update_session(session)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_session(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    state
        .supervisor
        .delete_session(&id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_all(state: State<'_, AppState>) -> std::result::Result<(), String> {
    state.supervisor.stop_all(true).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reveal_path(state: State<'_, AppState>, path: String) -> std::result::Result<(), String> {
    state
        .supervisor
        .os()
        .reveal_path(std::path::Path::new(&path))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_in_os(path: String) -> std::result::Result<(), String> {
    session_manager_core::os::open_in_os(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

/// Bring the main window to the foreground. Used by the tray popover
/// when the user clicks a row that should reveal the main view — the
/// popover doesn't own that surface, so we hand off here.
///
/// On macOS, `set_focus()` calls `[NSWindow makeKeyAndOrderFront:]` which
/// per Apple docs does NOT activate the application. If the user clicks
/// from the popover while another app is foreground, the main window
/// orders front but sits BEHIND that app — the user sees nothing change
/// and assumes the click did nothing. We explicitly call `NSApp.activate()`
/// to bring the whole app forward before focusing. (Same trick we already
/// use for the popover itself in `tray::toggle_popover`.)
#[tauri::command]
pub fn focus_main_window(app: tauri::AppHandle) -> std::result::Result<(), String> {
    use tauri::Manager;
    let Some(w) = app.get_webview_window("main") else {
        return Err("main window not found".into());
    };
    w.show().map_err(|e| e.to_string())?;
    w.unminimize().map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).activate();
        }
    }
    w.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

/// Backends the UI is allowed to surface in the agent picker.
/// Codex stays in the core registry so existing sessions referencing it
/// continue to work, but we don't recommend creating new ones until the
/// flags are real.
const VISIBLE_BACKENDS: &[&str] = &["claude-code"];

#[tauri::command]
pub fn registry(_state: State<'_, AppState>) -> Vec<BackendInfo> {
    session_manager_core::backend::registry()
        .into_iter()
        .filter(|b| VISIBLE_BACKENDS.contains(&b.id.as_str()))
        .collect()
}

#[tauri::command]
pub async fn auth_states(
    state: State<'_, AppState>,
) -> std::result::Result<HashMap<String, AuthState>, String> {
    Ok(state.supervisor.auth_states().await)
}

#[derive(serde::Deserialize)]
pub struct PreferencesPatch {
    pub launch_at_login: Option<bool>,
    pub notifications_enabled: Option<bool>,
    pub keep_awake_master: Option<bool>,
    pub defaults: Option<SessionDefaults>,
    pub power_user_prompt_dismissed: Option<bool>,
    pub launch_at_login_prompt_dismissed: Option<bool>,
}

#[tauri::command]
pub async fn update_preferences(
    state: State<'_, AppState>,
    patch: PreferencesPatch,
) -> std::result::Result<AppPreferences, String> {
    state
        .supervisor
        .update_preferences(|p| {
            if let Some(v) = patch.launch_at_login {
                p.launch_at_login = v;
            }
            if let Some(v) = patch.notifications_enabled {
                p.notifications_enabled = v;
            }
            if let Some(v) = patch.keep_awake_master {
                p.keep_awake_master = v;
            }
            if let Some(v) = patch.defaults {
                p.defaults = v;
            }
            if let Some(v) = patch.power_user_prompt_dismissed {
                p.power_user_prompt_dismissed = v;
            }
            if let Some(v) = patch.launch_at_login_prompt_dismissed {
                p.launch_at_login_prompt_dismissed = v;
            }
        })
        .await
        .map_err(|e| e.to_string())?;
    let (file, _) = state.supervisor.snapshot().await;
    Ok(file.preferences)
}

#[tauri::command]
pub fn set_launch_at_login(
    state: State<'_, AppState>,
    enabled: bool,
) -> std::result::Result<bool, String> {
    let bin = std::env::current_exe().map_err(|e| e.to_string())?;
    let plan = session_manager_core::os::LaunchAtLoginPlan {
        binary: bin,
        start_hidden: true,
    };
    state
        .supervisor
        .os()
        .set_launch_at_login(enabled, &plan)
        .map_err(|e| e.to_string())?;
    Ok(enabled)
}

#[tauri::command]
pub fn config_file_path(state: State<'_, AppState>) -> String {
    state
        .supervisor
        .config_path()
        .to_string_lossy()
        .into_owned()
}

#[tauri::command]
pub async fn export_config(
    state: State<'_, AppState>,
    path: String,
) -> std::result::Result<(), String> {
    let src = state.supervisor.config_path().to_path_buf();
    std::fs::copy(&src, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Largest config file we'll accept on import. Real `sessions.toml` files
/// are KB-scale; anything beyond this is either an accident or a malicious
/// dialog target. 1 MB is generous.
const MAX_IMPORT_BYTES: u64 = 1024 * 1024;

#[tauri::command]
pub async fn import_config(
    state: State<'_, AppState>,
    path: String,
) -> std::result::Result<(), String> {
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_IMPORT_BYTES {
        return Err(format!(
            "refusing to import: file is {} bytes (max {})",
            meta.len(),
            MAX_IMPORT_BYTES
        ));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed: session_manager_core::SessionsFile =
        toml::from_str(&raw).map_err(|e| format!("invalid sessions.toml: {e}"))?;
    // Stop currently-running workers so the import doesn't double-spawn.
    // user_initiated=false — this is an internal teardown ahead of an
    // import; we don't want to park the existing fleet.
    state
        .supervisor
        .stop_all(false)
        .await
        .map_err(|e| e.to_string())?;
    // Replace the fleet wholesale (the UI confirms "replace your fleet").
    let (existing, _) = state.supervisor.snapshot().await;
    for s in existing.sessions {
        let _ = state.supervisor.delete_session(&s.id).await;
    }
    for session in parsed.sessions {
        if let Err(e) = state.supervisor.create_session(session.clone()).await {
            if matches!(e, session_manager_core::Error::SessionExists(_)) {
                state
                    .supervisor
                    .update_session(session)
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                return Err(e.to_string());
            }
        }
    }
    state
        .supervisor
        .update_preferences(|p| *p = parsed.preferences)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn external_sessions(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<session_manager_core::backend::DiscoveredSession>, String> {
    Ok(state.supervisor.external_sessions().await)
}

#[tauri::command]
pub fn stop_external(state: State<'_, AppState>, pid: u32) -> std::result::Result<(), String> {
    state
        .supervisor
        .stop_external(pid)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn adopt_external(
    state: State<'_, AppState>,
    pid: u32,
    session: SessionConfig,
) -> std::result::Result<(), String> {
    state
        .supervisor
        .adopt_external(pid, session)
        .await
        .map_err(|e| e.to_string())
}

/// Claim the running external process under management without killing
/// it. Persists the config and starts a probe-only worker that owns the
/// PID.
#[tauri::command]
pub async fn claim_external(
    state: State<'_, AppState>,
    pid: u32,
    session: SessionConfig,
) -> std::result::Result<(), String> {
    state
        .supervisor
        .claim_external(pid, session)
        .await
        .map_err(|e| e.to_string())
}

/// Resolve the on-disk log path the supervisor would use for a session.
/// Used by the UI's "Open log" fallback so the tilde doesn't leak.
#[tauri::command]
pub async fn resolved_log_path(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<String, String> {
    let (file, _) = state.supervisor.snapshot().await;
    let session = file
        .sessions
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("session {id} not found"))?;
    let p = session.resolved_log_path().map_err(|e| e.to_string())?;
    Ok(p.to_string_lossy().into_owned())
}

/// Check whether a user-supplied path (with `~` and env-vars) exists as a
/// directory. Used by the create-session form to surface bad input before
/// the supervisor tries to spawn into a nonexistent cwd.
#[tauri::command]
pub fn path_exists(path: String) -> bool {
    let expanded = shellexpand::full(&path)
        .map(|c| c.into_owned())
        .unwrap_or(path);
    std::path::Path::new(&expanded).is_dir()
}

/// Tell the form *why* a path is invalid: missing, or a file (not a folder).
/// Lets the user see a meaningful error instead of a generic "doesn't exist".
#[tauri::command]
pub fn path_kind(path: String) -> &'static str {
    let expanded = shellexpand::full(&path)
        .map(|c| c.into_owned())
        .unwrap_or(path);
    let p = std::path::Path::new(&expanded);
    if !p.exists() {
        "missing"
    } else if p.is_dir() {
        "dir"
    } else if p.is_file() {
        "file"
    } else {
        "other"
    }
}

/// Reset a crashed session's restart counter and start it again.
#[tauri::command]
pub async fn reset_and_retry(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    state
        .supervisor
        .reset_and_retry(&id)
        .await
        .map_err(|e| e.to_string())
}

// silence unused warning when nothing imports
#[allow(dead_code)]
fn _unused(_: Result<()>, _: SessionStatus, _: PathBuf) {}

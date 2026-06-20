//! Per-OS paths for config / state cache / logs (§7.8).

use std::path::PathBuf;

use crate::{Error, Result};

const APP: &str = "SessionManager";
const APP_LOWER: &str = "session-manager";

/// `sessions.toml` config directory.
pub fn config_dir() -> Result<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        dirs::config_dir().map(|p| p.join(APP))
    } else if cfg!(target_os = "windows") {
        dirs::config_dir().map(|p| p.join(APP))
    } else {
        dirs::config_dir().map(|p| p.join(APP_LOWER))
    };
    base.ok_or_else(|| Error::Other("could not resolve config dir".into()))
}

pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("sessions.toml"))
}

/// Runtime cache (PID, last-seen status, remote URLs).
pub fn state_dir() -> Result<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        dirs::cache_dir().map(|p| p.join(APP))
    } else if cfg!(target_os = "windows") {
        dirs::data_local_dir().map(|p| p.join(APP))
    } else {
        // Linux: ~/.local/state/session-manager
        dirs::home_dir().map(|p| p.join(".local/state").join(APP_LOWER))
    };
    base.ok_or_else(|| Error::Other("could not resolve state dir".into()))
}

pub fn state_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("runtime.json"))
}

/// Per-session log directory.
pub fn log_dir() -> Result<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        dirs::home_dir().map(|p| p.join("Library/Logs").join(APP))
    } else if cfg!(target_os = "windows") {
        dirs::data_local_dir().map(|p| p.join(APP).join("logs"))
    } else {
        dirs::home_dir().map(|p| p.join(".local/state").join(APP_LOWER).join("logs"))
    };
    base.ok_or_else(|| Error::Other("could not resolve log dir".into()))
}

pub fn ensure_dirs() -> Result<()> {
    std::fs::create_dir_all(config_dir()?)?;
    std::fs::create_dir_all(state_dir()?)?;
    std::fs::create_dir_all(log_dir()?)?;
    Ok(())
}

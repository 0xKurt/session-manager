//! Linux OS layer. Stub-but-compiling per §7.9.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::{KeepAwakeToken, LaunchAtLoginPlan, OsLayer, SleepEvent};
use crate::{Error, Result};

pub struct LinuxLayer {
    keep_awake_active: Arc<AtomicBool>,
}

impl LinuxLayer {
    pub fn new() -> Self {
        Self {
            keep_awake_active: Arc::new(AtomicBool::new(false)),
        }
    }

    fn autostart_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| Error::Os("no home".into()))?;
        Ok(home.join(".config/autostart/session-manager.desktop"))
    }
}

#[async_trait]
impl OsLayer for LinuxLayer {
    fn set_launch_at_login(&self, enabled: bool, plan: &LaunchAtLoginPlan) -> Result<()> {
        let path = Self::autostart_path()?;
        if !enabled {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        let exec = plan.binary.to_string_lossy();
        let body = format!(
            r#"[Desktop Entry]
Type=Application
Name=Session Manager
Exec={exec}{hidden}
X-GNOME-Autostart-enabled=true
"#,
            hidden = if plan.start_hidden { " --hidden" } else { "" },
        );
        std::fs::write(&path, body)?;
        Ok(())
    }

    fn launch_at_login_enabled(&self) -> Result<bool> {
        Ok(Self::autostart_path()?.exists())
    }

    fn acquire_keep_awake(&self, reason: &str) -> Result<Box<dyn KeepAwakeToken>> {
        // TODO §7.9: take a systemd-inhibit lock via logind D-Bus.
        // For now, spawn `systemd-inhibit --what=idle:sleep --mode=block sleep infinity`
        // and kill it on drop — matches the macOS `caffeinate` pattern.
        let child = std::process::Command::new("systemd-inhibit")
            .args([
                "--what=idle:sleep",
                "--who=session-manager",
                "--mode=block",
                &format!("--why={reason}"),
                "sleep",
                "infinity",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match child {
            Ok(child) => {
                self.keep_awake_active.store(true, Ordering::SeqCst);
                Ok(Box::new(LinuxKeepAwake {
                    pid: child.id(),
                    flag: Arc::clone(&self.keep_awake_active),
                    reason: reason.into(),
                }))
            }
            Err(_) => {
                // systemd-inhibit unavailable — return a no-op token; the UI
                // surfaces this as "keep-awake unavailable" indirectly via
                // the lack of "Awake" indicator change.
                Ok(Box::new(super::NoopKeepAwakeToken))
            }
        }
    }

    fn notify(&self, title: &str, body: &str, _urgent: bool) -> Result<()> {
        let _ = std::process::Command::new("notify-send")
            .arg(title)
            .arg(body)
            .status();
        Ok(())
    }

    fn watch_sleep_events(
        &self,
        _tx: tokio::sync::mpsc::UnboundedSender<SleepEvent>,
    ) -> Result<()> {
        // TODO §7.9: subscribe to logind's PrepareForSleep D-Bus signal via zbus.
        Ok(())
    }
}

pub struct LinuxKeepAwake {
    pid: u32,
    flag: Arc<AtomicBool>,
    #[allow(dead_code)]
    reason: String,
}
impl KeepAwakeToken for LinuxKeepAwake {
    fn release(&self) {
        self.flag.store(false, Ordering::SeqCst);
        let _ = std::process::Command::new("kill")
            .arg(self.pid.to_string())
            .status();
    }
}
impl Drop for LinuxKeepAwake {
    fn drop(&mut self) {
        self.release();
    }
}

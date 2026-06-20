//! OS layer trait + per-target impls (§7.9). Everything platform-specific
//! lives behind here.

use std::path::Path;
use std::process::{Command, Stdio};

use async_trait::async_trait;

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepEvent {
    WillSleep,
    DidWake,
}

#[derive(Debug, Clone)]
pub struct LaunchAtLoginPlan {
    /// Absolute path to the launcher binary (the bundled GUI).
    pub binary: std::path::PathBuf,
    /// Whether the launcher should start hidden in the tray.
    pub start_hidden: bool,
}

#[async_trait]
pub trait OsLayer: Send + Sync {
    /// Register or de-register the supervisor to launch at user login.
    fn set_launch_at_login(&self, enabled: bool, plan: &LaunchAtLoginPlan) -> Result<()>;
    fn launch_at_login_enabled(&self) -> Result<bool>;

    /// Acquire / release a sleep-inhibitor for the duration of `working`
    /// keep-awake sessions. The token is held by [`KeepAwakeToken`] — drop
    /// it (or call `release`) to release the lock.
    fn acquire_keep_awake(&self, reason: &str) -> Result<Box<dyn KeepAwakeToken>>;

    /// Native notification ("needs permission", "crashed", etc.).
    fn notify(&self, title: &str, body: &str, urgent: bool) -> Result<()>;

    /// Spawn a sleep/wake watcher that pushes events into the supplied sender.
    fn watch_sleep_events(&self, tx: tokio::sync::mpsc::UnboundedSender<SleepEvent>) -> Result<()>;

    /// Open a file/folder/URL in the OS's default handler.
    fn reveal_path(&self, path: &Path) -> Result<()> {
        let _ = path;
        Ok(())
    }
}

pub trait KeepAwakeToken: Send + Sync {
    fn release(&self);
}

/// Token that doesn't actually hold anything (used by stub OS layers).
pub struct NoopKeepAwakeToken;
impl KeepAwakeToken for NoopKeepAwakeToken {
    fn release(&self) {}
}

/// Choose the right impl at compile time.
pub fn default_layer() -> Box<dyn OsLayer> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacOsLayer::new())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsLayer::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxLayer::new())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Box::new(StubLayer)
    }
}

/// Cross-platform "open this path / URL in the system handler".
pub fn open_in_os(path: &Path) -> Result<()> {
    let p = path.to_string_lossy().into_owned();
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(&p);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", &p]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(&p);
        c
    };
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn()?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub struct StubLayer;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
#[async_trait]
impl OsLayer for StubLayer {
    fn set_launch_at_login(&self, _e: bool, _p: &LaunchAtLoginPlan) -> Result<()> {
        Ok(())
    }
    fn launch_at_login_enabled(&self) -> Result<bool> {
        Ok(false)
    }
    fn acquire_keep_awake(&self, _reason: &str) -> Result<Box<dyn KeepAwakeToken>> {
        Ok(Box::new(NoopKeepAwakeToken))
    }
    fn notify(&self, _t: &str, _b: &str, _u: bool) -> Result<()> {
        Ok(())
    }
    fn watch_sleep_events(
        &self,
        _tx: tokio::sync::mpsc::UnboundedSender<SleepEvent>,
    ) -> Result<()> {
        Ok(())
    }
}

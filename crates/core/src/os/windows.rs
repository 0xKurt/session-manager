//! Windows OS layer. Stub-but-compiling per §7.9. Fully implementing this is
//! a follow-up; the trait surface is exhaustive so the supervisor doesn't
//! need to know which platform it runs on.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::{KeepAwakeToken, LaunchAtLoginPlan, OsLayer, SleepEvent};
use crate::Result;

pub struct WindowsLayer {
    keep_awake_active: Arc<AtomicBool>,
}

impl WindowsLayer {
    pub fn new() -> Self {
        Self {
            keep_awake_active: Arc::new(AtomicBool::new(false)),
        }
    }

    fn run_key_path() -> PathBuf {
        // HKCU\Software\Microsoft\Windows\CurrentVersion\Run — managed at
        // runtime; we just store an expected install marker here for the
        // stub.
        PathBuf::from(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\SessionManager")
    }
}

#[async_trait]
impl OsLayer for WindowsLayer {
    fn set_launch_at_login(&self, enabled: bool, _plan: &LaunchAtLoginPlan) -> Result<()> {
        // TODO §7.9: write to HKCU Run key via `windows` crate.
        let _ = (enabled, Self::run_key_path());
        Ok(())
    }

    fn launch_at_login_enabled(&self) -> Result<bool> {
        // TODO §7.9
        Ok(false)
    }

    fn acquire_keep_awake(&self, reason: &str) -> Result<Box<dyn KeepAwakeToken>> {
        // TODO §7.9: SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED).
        self.keep_awake_active.store(true, Ordering::SeqCst);
        Ok(Box::new(WindowsKeepAwake {
            flag: Arc::clone(&self.keep_awake_active),
            reason: reason.into(),
        }))
    }

    fn notify(&self, _title: &str, _body: &str, _urgent: bool) -> Result<()> {
        // TODO §7.9: WinRT toast notifications.
        Ok(())
    }

    fn watch_sleep_events(
        &self,
        _tx: tokio::sync::mpsc::UnboundedSender<SleepEvent>,
    ) -> Result<()> {
        // TODO §7.9: WM_POWERBROADCAST window-message pump.
        Ok(())
    }
}

pub struct WindowsKeepAwake {
    flag: Arc<AtomicBool>,
    #[allow(dead_code)]
    reason: String,
}
impl KeepAwakeToken for WindowsKeepAwake {
    fn release(&self) {
        self.flag.store(false, Ordering::SeqCst);
        // TODO: SetThreadExecutionState(ES_CONTINUOUS) to clear.
    }
}
impl Drop for WindowsKeepAwake {
    fn drop(&mut self) {
        self.release();
    }
}

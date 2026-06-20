//! macOS implementation of [`OsLayer`].
//!
//! - **Launch at login:** writes a LaunchAgent plist under
//!   `~/Library/LaunchAgents/`. SMAppService would be ideal in the bundled
//!   build but requires the embedded helper; the plist works for both dev
//!   and bundle (§7.9).
//! - **Keep-awake:** holds an `IOPMAssertionCreateWithName` of type
//!   `PreventUserIdleSystemSleep`.
//! - **Sleep / wake:** subscribes to `NSWorkspace` notifications via a
//!   `caffeinate`-style watcher process. Tauri's main thread isn't safe to
//!   call AppKit from arbitrary tokio tasks, so we shell out to `pmset -g
//!   pslog` and parse the lines — same data, no Objective-C bridge needed.
//! - **Notifications:** `osascript display notification` is the lowest-friction
//!   path that works without code signing or NSUserNotification entitlements.
//!   Replace with `UNUserNotificationCenter` once the app is signed.

use std::path::PathBuf;
use std::process::{Child, Command};

use async_trait::async_trait;
use parking_lot::Mutex;

use super::{KeepAwakeToken, LaunchAtLoginPlan, OsLayer, SleepEvent};
use crate::{Error, Result};

#[derive(Default)]
pub struct MacOsLayer {}

impl MacOsLayer {
    pub fn new() -> Self {
        Self::default()
    }

    fn launch_agent_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| Error::Os("no home".into()))?;
        Ok(home
            .join("Library/LaunchAgents")
            .join("dev.zeiber.session-manager.plist"))
    }
}

#[async_trait]
impl OsLayer for MacOsLayer {
    fn set_launch_at_login(&self, enabled: bool, plan: &LaunchAtLoginPlan) -> Result<()> {
        let path = Self::launch_agent_path()?;
        if !enabled {
            let _ = Command::new("launchctl")
                .args(["unload", &path.to_string_lossy()])
                .status();
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        let bin = plan.binary.to_string_lossy();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.zeiber.session-manager</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    {hidden_arg}
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><false/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>"#,
            hidden_arg = if plan.start_hidden {
                "<string>--hidden</string>"
            } else {
                ""
            },
        );
        std::fs::write(&path, plist)?;
        let _ = Command::new("launchctl")
            .args(["load", "-w", &path.to_string_lossy()])
            .status();
        Ok(())
    }

    fn launch_at_login_enabled(&self) -> Result<bool> {
        Ok(Self::launch_agent_path()?.exists())
    }

    fn acquire_keep_awake(&self, reason: &str) -> Result<Box<dyn KeepAwakeToken>> {
        // `caffeinate -d -i -s -m -u` — block display, idle, system, disk
        // sleep AND declare the user active. We use the most aggressive
        // assertion set caffeinate exposes:
        //   -d  PreventUserIdleDisplaySleep
        //   -i  PreventUserIdleSystemSleep
        //   -s  PreventSystemSleep  (only effective on AC power!)
        //   -m  PreventDiskIdle
        //   -u  Declare user active (extends timeout windows)
        //
        // Important macOS limitation: NO caffeinate flag survives
        // **lid-close on battery** — that's a hardware-enforced clamshell
        // sleep policy that requires sudo + pmset changes (or external
        // display + power + keyboard for "clamshell mode") to override.
        // The UI Settings page documents this so users aren't surprised.
        let child = Command::new("caffeinate")
            .args(["-d", "-i", "-s", "-m", "-u"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .spawn()
            .map_err(|e| Error::Os(format!("caffeinate spawn: {e}")))?;
        let token = MacOsKeepAwake {
            child: Mutex::new(Some(child)),
            reason: reason.into(),
        };
        Ok(Box::new(token))
    }

    fn notify(&self, title: &str, body: &str, _urgent: bool) -> Result<()> {
        // Shell out to osascript — works without signing.
        let escaped_title = title.replace('"', "\\\"");
        let escaped_body = body.replace('"', "\\\"");
        let script =
            format!(r#"display notification "{escaped_body}" with title "{escaped_title}""#);
        let _ = Command::new("osascript").args(["-e", &script]).status();
        Ok(())
    }

    fn watch_sleep_events(&self, tx: tokio::sync::mpsc::UnboundedSender<SleepEvent>) -> Result<()> {
        // `pmset -g pslog` streams sleep/wake lines. We match on the
        // explicit event tokens, not substring, so noisy status lines
        // (WakeRequests:, SleepFrom:, …) don't trigger reconciles.
        //
        // Wrapped in a respawn loop with exponential backoff: if pmset
        // itself dies (killed by Activity Monitor, OOM, …) we wait and
        // re-spawn so we don't permanently lose sleep/wake events.
        std::thread::Builder::new()
            .name("macos-pmset-watcher".into())
            .spawn(move || {
                use std::io::{BufRead, BufReader};
                use std::time::Duration;
                let mut backoff = Duration::from_millis(500);
                loop {
                    let spawn_res = Command::new("pmset")
                        .args(["-g", "pslog"])
                        .stdout(std::process::Stdio::piped())
                        .spawn();
                    let mut child = match spawn_res {
                        Ok(c) => c,
                        Err(_) => {
                            std::thread::sleep(backoff);
                            backoff = (backoff * 2).min(Duration::from_secs(60));
                            continue;
                        }
                    };
                    let Some(stdout) = child.stdout.take() else {
                        let _ = child.kill();
                        let _ = child.wait();
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(60));
                        continue;
                    };
                    let reader = BufReader::new(stdout);
                    for line in reader
                        .lines()
                        .map_while(|r: std::io::Result<String>| r.ok())
                    {
                        // Reset backoff once we're actually receiving lines.
                        backoff = Duration::from_millis(500);
                        if let Some(ev) = parse_pmset_line(&line) {
                            if tx.send(ev).is_err() {
                                // Receiver dropped — supervisor going away.
                                let _ = child.kill();
                                let _ = child.wait();
                                return;
                            }
                        }
                    }
                    // EOF on stdout — pmset exited. Reap and respawn.
                    let _ = child.wait();
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            })
            .map_err(|e| Error::Os(format!("watcher spawn: {e}")))?;
        Ok(())
    }

    fn reveal_path(&self, path: &std::path::Path) -> Result<()> {
        Command::new("open").arg("-R").arg(path).spawn()?;
        Ok(())
    }
}

pub struct MacOsKeepAwake {
    child: Mutex<Option<Child>>,
    #[allow(dead_code)]
    reason: String,
}

impl KeepAwakeToken for MacOsKeepAwake {
    fn release(&self) {
        let Some(mut child) = self.child.lock().take() else {
            return;
        };
        // Best-effort SIGTERM, then reap.
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for MacOsKeepAwake {
    fn drop(&mut self) {
        self.release();
    }
}

fn parse_pmset_line(line: &str) -> Option<SleepEvent> {
    // pmset -g pslog emits lines of varying shapes across macOS versions:
    //   2026-06-17 19:30:01 -0700  Sleep                Entering Sleep ...
    //   2026-06-17 19:31:14 -0700  Wake                 DarkWake from Standby ...
    //   Notification:  Sleep              Causing process: kernel_task
    //   EventType: Sleep                  Reason: ...
    // We scan for a Sleep/Wake/DarkWake/MaintenanceWake token *as a whole word*,
    // ignoring header lines like "WakeRequests:" or "Note: pmset...".
    let lower = line.to_ascii_lowercase();
    // Skip noise: header tokens with colons in the first word other than
    // "notification:" / "eventtype:".
    if lower.starts_with("note:")
        || lower.starts_with("wakerequests")
        || lower.starts_with("currently")
    {
        return None;
    }
    let tokens: Vec<&str> = lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .collect();
    // Look for a recognised event word. "sleep" wins over "wake" if both
    // appear (e.g. "Sleep ... wake-on-lan"); the actual log lines only carry
    // one event token in the column position so this is robust in practice.
    let mut sleep = false;
    let mut wake = false;
    for t in &tokens {
        match *t {
            "sleep" => sleep = true,
            "wake" | "darkwake" | "maintenancewake" => wake = true,
            _ => {}
        }
    }
    if sleep && !wake {
        Some(SleepEvent::WillSleep)
    } else if wake && !sleep {
        Some(SleepEvent::DidWake)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_classic_sleep() {
        let line = "2026-06-17 19:30:01 -0700  Sleep                Entering Sleep state due to 'Idle Sleep'";
        assert_eq!(parse_pmset_line(line), Some(SleepEvent::WillSleep));
    }
    #[test]
    fn parses_classic_wake() {
        let line = "2026-06-17 19:31:14 -0700  Wake                 DarkWake from Standby due to LID0/Lid Open";
        assert_eq!(parse_pmset_line(line), Some(SleepEvent::DidWake));
    }
    #[test]
    fn parses_darkwake() {
        let line = "2026-06-17 20:00:00 -0700  DarkWake             ...";
        assert_eq!(parse_pmset_line(line), Some(SleepEvent::DidWake));
    }
    #[test]
    fn ignores_header_lines() {
        assert_eq!(
            parse_pmset_line("Note: pmset -g pslog -- log of recent ..."),
            None
        );
        assert_eq!(parse_pmset_line("WakeRequests: 0"), None);
    }
}

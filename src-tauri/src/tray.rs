//! Tray — the click-target *is* the overview.
//!
//! **Left-click**: toggles a frameless, transparent webview popover
//! anchored under the tray icon. The popover is a separate Tauri window
//! (`label = "popover"`, declared in tauri.conf.json) that loads the
//! React app at `#/popover` and reuses the existing store/session data.
//! Closes itself on focus-loss (handled in lib.rs).
//!
//! **Right-click**: pops the native NSMenu as a fallback — keeps a11y +
//! a way out when the popover misbehaves on a given macOS revision.

use std::sync::Arc;

use session_manager_core::events::{CoreEvent, SessionStatus};
use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::{App, AppHandle, LogicalPosition, Manager, Wry};
use tokio::sync::Notify;

use crate::AppState;

/// Three tray icon variants, glanceable from the menu bar:
///   - `calm`     (white template) — nothing needs your attention.
///   - `working`  (purple) — at least one session is doing work.
///   - `attention` (orange) — a session is asking for permission OR has
///     crashed; both are user-actionable.
const ICON_CALM: &[u8] = include_bytes!("../icons/tray-calm.png");
const ICON_WORKING: &[u8] = include_bytes!("../icons/tray-working.png");
const ICON_ATTENTION: &[u8] = include_bytes!("../icons/tray-attention.png");

pub struct TrayRebuilder(pub Arc<Notify>);

const MENU_HEADER: &str = "header";
const MENU_NEW: &str = "new-session";
const MENU_STOP_ALL: &str = "stop-all";
const MENU_OPEN: &str = "open-window";
const MENU_QUIT: &str = "quit";

/// Click a session row → open the session detail in the window.
const SESSION_OPEN_PREFIX: &str = "open::";
/// Click the Start/Stop child item → toggle.
const SESSION_TOGGLE_PREFIX: &str = "toggle::";

pub fn install(app: &mut App) -> tauri::Result<()> {
    let tray = app
        .tray_by_id("tray")
        .expect("tray icon configured in tauri.conf.json");
    // No native NSMenu attached — the popover is the only interaction
    // surface. Right-click is a no-op. The build_menu helpers stay in
    // this file (commented use) in case we ever want to bring back a
    // fallback menu, but `set_menu(None)` keeps the OS from drawing
    // anything on right-click.
    tray.set_menu(None::<Menu<Wry>>)?;
    tray.set_show_menu_on_left_click(false)?;
    tray.set_tooltip(Some("Session Manager"))?;

    // Coalesce events into a single debounced rebuild.
    let notify = Arc::new(Notify::new());
    app.manage(TrayRebuilder(Arc::clone(&notify)));
    let rebuild_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        loop {
            notify.notified().await;
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            rebuild_now(&rebuild_handle).await;
        }
    });

    // No `on_menu_event` — menu is gone. All click routing happens
    // inside the popover webview now.
    tray.on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            rect,
            ..
        } = event
        {
            // tray-icon's Rect is in the high-DPI-agnostic Position/Size
            // enum. Convert into physical pixels for set_position math.
            let pos = rect.position.to_physical::<f64>(1.0);
            let size = rect.size.to_physical::<f64>(1.0);
            toggle_popover(tray.app_handle(), pos, size);
        }
    });

    Ok(())
}

/// Show the popover anchored to the tray icon, or hide it if it's
/// already visible. Position is computed from the tray icon rect so
/// the popover sits centred under the icon with a small gap.
fn toggle_popover(
    app: &AppHandle,
    icon_pos: tauri::PhysicalPosition<f64>,
    icon_size: tauri::PhysicalSize<f64>,
) {
    let Some(popover) = app.get_webview_window("popover") else {
        return;
    };
    if popover.is_visible().unwrap_or(false) {
        let _ = popover.hide();
        return;
    }
    // Use logical positioning — Tauri handles the scale-factor conversion.
    let scale = popover.scale_factor().unwrap_or(1.0);
    let win_size = popover.outer_size().ok();
    let win_w_logical = win_size.map(|s| s.width as f64 / scale).unwrap_or(340.0);
    let icon_x = icon_pos.x / scale;
    let icon_y = icon_pos.y / scale;
    let icon_w = icon_size.width / scale;
    let icon_h = icon_size.height / scale;
    // Centre the popover horizontally under the icon. Drop down 6px so
    // it doesn't kiss the menu bar.
    let x = icon_x + (icon_w / 2.0) - (win_w_logical / 2.0);
    let y = icon_y + icon_h + 6.0;
    let _ = popover.set_position(LogicalPosition::new(x, y));
    let _ = popover.show();
    let _ = popover.set_focus();
}

#[derive(Debug, Clone)]
struct TrayRow {
    id: String,
    name: String,
    status: SessionStatus,
}

fn status_glyph(s: SessionStatus) -> &'static str {
    match s {
        SessionStatus::Working => "●",
        SessionStatus::NeedsPermission => "↪",
        SessionStatus::Idle => "○",
        SessionStatus::Done => "✓",
        SessionStatus::RateLimited => "◐",
        SessionStatus::Crashed => "⚠",
        SessionStatus::Stopped => "—",
        SessionStatus::Offline => "·",
        SessionStatus::Starting => "…",
    }
}

fn build_menu(app: &AppHandle, all_rows: &[TrayRow]) -> tauri::Result<Menu<Wry>> {
    let mut b = MenuBuilder::new(app);

    // Aggregate header — disabled, just visible.
    let summary = if all_rows.is_empty() {
        "No sessions defined".to_string()
    } else {
        summary_line(all_rows)
    };
    let header = MenuItemBuilder::with_id(MENU_HEADER, summary)
        .enabled(false)
        .build(app)?;
    b = b.item(&header).item(&PredefinedMenuItem::separator(app)?);

    // Attention rows first (needs-permission, crashed), then alphabetical.
    let mut rows = all_rows.to_vec();
    rows.sort_by(|a, b| attention_rank(&a.status).cmp(&attention_rank(&b.status)).then(a.name.cmp(&b.name)));

    if rows.is_empty() {
        let placeholder =
            MenuItemBuilder::with_id("__empty", "(create a session to see it here)")
                .enabled(false)
                .build(app)?;
        b = b.item(&placeholder);
    } else {
        for row in &rows {
            let label = format!("{}  {}", status_glyph(row.status), row.name);
            let open_item =
                MenuItemBuilder::with_id(format!("{SESSION_OPEN_PREFIX}{}", row.id), &label)
                    .build(app)?;
            let toggle_label = if row.status.is_running() {
                "      Stop"
            } else {
                "      Start"
            };
            let toggle_item = MenuItemBuilder::with_id(
                format!("{SESSION_TOGGLE_PREFIX}{}", row.id),
                toggle_label,
            )
            .build(app)?;
            b = b.item(&open_item).item(&toggle_item);
        }
    }

    b = b.item(&PredefinedMenuItem::separator(app)?);
    let new_session = MenuItemBuilder::with_id(MENU_NEW, "New session…").build(app)?;
    let stop_all = MenuItemBuilder::with_id(MENU_STOP_ALL, "Stop all sessions").build(app)?;
    let open = MenuItemBuilder::with_id(MENU_OPEN, "Open window").build(app)?;
    let quit = MenuItemBuilder::with_id(MENU_QUIT, "Quit Session Manager").build(app)?;
    b.item(&new_session)
        .item(&stop_all)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&open)
        .item(&quit)
        .build()
}

fn attention_rank(s: &SessionStatus) -> u8 {
    match s {
        SessionStatus::NeedsPermission => 0,
        SessionStatus::Crashed => 1,
        SessionStatus::Working => 2,
        SessionStatus::Starting => 3,
        SessionStatus::RateLimited => 4,
        SessionStatus::Idle => 5,
        SessionStatus::Offline => 6,
        SessionStatus::Done => 7,
        SessionStatus::Stopped => 8,
    }
}

fn summary_line(rows: &[TrayRow]) -> String {
    let mut running = 0usize;
    let mut needs_perm = 0usize;
    let mut crashed = 0usize;
    for r in rows {
        if r.status.is_running() { running += 1; }
        if r.status == SessionStatus::NeedsPermission { needs_perm += 1; }
        if r.status == SessionStatus::Crashed { crashed += 1; }
    }
    let mut parts = vec![format!("{running} running")];
    if needs_perm > 0 { parts.push(format!("{needs_perm} need permission")); }
    if crashed > 0 { parts.push(format!("{crashed} crashed")); }
    parts.join(" · ")
}

fn show_window(app: &AppHandle, route: Option<&str>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        if let Some(r) = route {
            let _ = w.eval(format!("window.location.hash = {r:?};"));
        }
    }
}

pub fn reflect_event(app: &AppHandle, event: &CoreEvent) -> tauri::Result<()> {
    if matches!(
        event,
        CoreEvent::StatusChanged { .. }
            | CoreEvent::ConfigReloaded
            | CoreEvent::KeepAwakeChanged { .. }
            | CoreEvent::NeedsPermission { .. }
    ) {
        if let Some(handle) = app.try_state::<TrayRebuilder>() {
            handle.0.notify_one();
        }
    }
    Ok(())
}

async fn rebuild_now(app: &AppHandle) {
    let sup = app.state::<AppState>().supervisor.clone();
    let (file, runtime) = sup.snapshot().await;
    let mut running = 0usize;
    let mut needs_perm = 0usize;
    let mut crashed = 0usize;
    let mut rows: Vec<TrayRow> = Vec::new();
    for cfg in &file.sessions {
        let st = runtime
            .sessions
            .get(&cfg.id)
            .map(|r| r.status)
            .unwrap_or(SessionStatus::Stopped);
        if st.is_running() { running += 1; }
        if st == SessionStatus::NeedsPermission { needs_perm += 1; }
        if st == SessionStatus::Crashed { crashed += 1; }
        rows.push(TrayRow {
            id: cfg.id.clone(),
            name: cfg.name.clone(),
            status: st,
        });
    }
    let mut parts: Vec<String> = vec![format!("{running} running")];
    if needs_perm > 0 { parts.push(format!("{needs_perm} need permission")); }
    if crashed > 0 { parts.push(format!("{crashed} crashed")); }
    if runtime.keep_awake_active { parts.push("awake".into()); }
    let tooltip = format!("Session Manager — {}", parts.join(" · "));
    if let Some(tray) = app.tray_by_id("tray") {
        let _ = tray.set_tooltip(Some(&tooltip));
        let (icon_bytes, as_template) = if needs_perm > 0 || crashed > 0 {
            (ICON_ATTENTION, false)
        } else if running > 0 {
            (ICON_WORKING, false)
        } else {
            (ICON_CALM, true)
        };
        if let Ok(img) = Image::from_bytes(icon_bytes) {
            let _ = tray.set_icon(Some(img));
            let _ = tray.set_icon_as_template(as_template);
        }
        // No menu rebuild — the popover reads its rows live from the
        // Zustand store on every render.
        let _ = rows; // silence unused-warning; we still compute counts for the icon/tooltip
    }
}

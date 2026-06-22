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

    // Convert the popover window (declared in tauri.conf.json) into a
    // non-activating NSPanel so it can show + accept input WITHOUT
    // activating the app. Stock NSWindow + `[makeKeyAndOrderFront:]`
    // doesn't activate the application (per Apple docs), so when the
    // user clicks the tray icon while the app is in the background, the
    // window would appear in a half-state — visible but not key, clicks
    // not delivered, focus events firing weirdly and triggering the
    // hide-on-blur handler in lib.rs. NSPanel with the NonactivatingPanel
    // style mask is what every polished menu-bar app uses (Raycast,
    // 1Password mini, Bartender).
    #[cfg(target_os = "macos")]
    if let Some(w) = app.handle().get_webview_window("popover") {
        if let Err(e) = crate::panel::make_popover_panel(&w) {
            tracing::warn!("popover panel conversion failed: {e}");
        }
    }

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
            tracing::info!(
                "tray click — position={:?} size={:?}",
                rect.position,
                rect.size
            );
            toggle_popover(tray.app_handle(), rect);
        }
    });

    Ok(())
}

/// Show the popover anchored to the tray icon, or hide it if it's
/// already visible. Position is computed from the tray icon rect so
/// the popover sits centred under the icon with a small gap.
///
/// The previous version did its own scale-factor math (physical →
/// logical via division) which on retina ended up halving an
/// already-logical position from Tauri, putting the popover off-screen
/// and looking like the click did nothing. We now convert the tray
/// rect's Position/Size enum directly to logical units via Tauri's
/// `to_logical(scale)` and position from there. Single conversion, no
/// double-divide.
fn toggle_popover(app: &AppHandle, rect: tauri::Rect) {
    let Some(popover) = app.get_webview_window("popover") else {
        tracing::warn!("popover window not found");
        return;
    };
    if popover.is_visible().unwrap_or(false) {
        let _ = popover.hide();
        return;
    }
    let scale = popover.scale_factor().unwrap_or(1.0);
    let icon_pos: tauri::LogicalPosition<f64> = rect.position.to_logical(scale);
    let icon_size: tauri::LogicalSize<f64> = rect.size.to_logical(scale);
    let (win_w, _win_h) = popover
        .outer_size()
        .ok()
        .map(|s| s.to_logical::<f64>(scale))
        .map(|l| (l.width, l.height))
        .unwrap_or((340.0, 480.0));
    // Centre the popover horizontally under the icon. Drop down 6px so
    // it doesn't kiss the menu bar.
    let x = icon_pos.x + (icon_size.width / 2.0) - (win_w / 2.0);
    let y = icon_pos.y + icon_size.height + 6.0;
    tracing::info!(
        "popover position: logical=({}, {}) win_w={} scale={}",
        x, y, win_w, scale
    );
    let _ = popover.set_position(LogicalPosition::new(x, y));
    let _ = popover.show();
    // Crucial on macOS: when the app is in the background (main window
    // hidden / another app is foreground), Tauri's `set_focus` is not
    // enough — NSApp only raises windows of the ACTIVE app, so without
    // explicit activation the popover comes up behind whatever's in
    // front and the click looks like a no-op. Force activation via
    // NSApp.activate() before focusing the popover.
    #[cfg(target_os = "macos")]
    activate_app_macos();
    let _ = popover.set_focus();
}

#[cfg(target_os = "macos")]
fn activate_app_macos() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    // tray click already lands on the main thread; MainThreadMarker::new
    // returns None outside it (defensive).
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let nsapp = NSApplication::sharedApplication(mtm);
    // macOS 14+ recommends `activate()`. It cooperates with the system
    // and works as long as the app has at least one visible window
    // (which we just made true by calling popover.show above).
    nsapp.activate();
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

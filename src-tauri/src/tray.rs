//! Tray menu — the click-target *is* the overview.
//!
//! Left-click on the tray icon pops the menu (it isn't a separate window).
//! The menu carries: an aggregate-status header, attention rows
//! (needs-permission / crashed) at the top, every defined session at top
//! level with its status glyph and an inline Start/Stop toggle, then the
//! global actions.

use std::sync::Arc;

use session_manager_core::events::{CoreEvent, SessionStatus};
use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::{App, AppHandle, Manager, Wry};
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
    let menu = build_menu(app.handle(), &[])?;
    tray.set_menu(Some(menu))?;
    // The click target IS the overview — the menu pops on left click.
    tray.set_show_menu_on_left_click(true)?;
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

    tray.on_menu_event(|app, event| {
        let id = event.id.as_ref();
        match id {
            // DD — the header is a disabled item; we route here so it
            // never accidentally triggers an action even if the OS
            // dispatches the event.
            MENU_HEADER => {} // disabled item — nothing to do
            MENU_NEW => show_window(app, Some("/new")),
            MENU_STOP_ALL => {
                let sup = app.state::<AppState>().supervisor.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = sup.stop_all().await;
                });
            }
            MENU_OPEN => show_window(app, None),
            MENU_QUIT => app.exit(0),
            other if other.starts_with(SESSION_OPEN_PREFIX) => {
                let session_id = other.trim_start_matches(SESSION_OPEN_PREFIX);
                show_window(app, Some(&format!("/session/{session_id}")));
            }
            other if other.starts_with(SESSION_TOGGLE_PREFIX) => {
                let session_id = other.trim_start_matches(SESSION_TOGGLE_PREFIX).to_string();
                let sup = app.state::<AppState>().supervisor.clone();
                tauri::async_runtime::spawn(async move {
                    let (_, runtime) = sup.snapshot().await;
                    let running = runtime
                        .sessions
                        .get(&session_id)
                        .map(|r| r.status.is_running())
                        .unwrap_or(false);
                    if running {
                        let _ = sup.stop_session(&session_id).await;
                    } else {
                        let _ = sup.start_session(&session_id).await;
                    }
                });
            }
            _ => {}
        }
    });
    // Optional: explicit left-click handler we leave OFF because
    // show_menu_on_left_click already pops the menu for us. Keeping the
    // tray-icon-event hook would compete with the native menu.
    Ok(())
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
        if let Ok(menu) = build_menu(app, &rows) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

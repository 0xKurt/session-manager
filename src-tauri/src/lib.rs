//! Tauri shell: builds the app, mounts the supervisor, exposes IPC, and
//! wires up the tray.

mod ipc;
#[cfg(target_os = "macos")]
mod panel;
mod tray;

use std::sync::Arc;

use session_manager_core::Supervisor;
use tauri::{Emitter, Manager};
use tokio::sync::broadcast;
use tracing_subscriber::EnvFilter;

pub struct AppState {
    pub supervisor: Arc<Supervisor>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();

    let supervisor = match Supervisor::open() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("fatal: open supervisor: {e}");
            std::process::exit(1);
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let supervisor_for_start = Arc::clone(&supervisor);
    runtime.spawn(async move {
        if let Err(e) = supervisor_for_start.start().await {
            tracing::error!("supervisor start: {e}");
        }
    });

    let supervisor_for_state = Arc::clone(&supervisor);
    let supervisor_for_shutdown = Arc::clone(&supervisor);
    let runtime_handle = runtime.handle().clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        // Auto-update from GitHub releases. Endpoints + Ed25519 pubkey live
        // in tauri.conf.json under `plugins.updater`. UI invokes via the
        // `@tauri-apps/plugin-updater` JS bindings.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            supervisor: supervisor_for_state,
        })
        .manage(RuntimeHandle(runtime.handle().clone()))
        .invoke_handler(tauri::generate_handler![
            ipc::list_sessions,
            ipc::session_runtime_snapshot,
            ipc::start_session,
            ipc::stop_session,
            ipc::restart_session,
            ipc::create_session,
            ipc::update_session,
            ipc::delete_session,
            ipc::stop_all,
            ipc::reveal_path,
            ipc::open_in_os,
            ipc::registry,
            ipc::auth_states,
            ipc::update_preferences,
            ipc::set_launch_at_login,
            ipc::config_file_path,
            ipc::export_config,
            ipc::import_config,
            ipc::resolved_log_path,
            ipc::path_exists,
            ipc::path_kind,
            ipc::reset_and_retry,
            ipc::external_sessions,
            ipc::stop_external,
            ipc::adopt_external,
            ipc::claim_external,
            ipc::focus_main_window,
        ])
        .setup(move |app| {
            // Tray.
            let tray_handle = app.handle().clone();
            tray::install(app)?;

            // SIGTERM / SIGINT handler: trigger Tauri's normal exit flow so
            // `RunEvent::Exit` fires → shutdown() drains sessions and removes
            // the socket. Without this, `kill <pid>` would orphan everything.
            #[cfg(unix)]
            {
                let exit_handle = app.handle().clone();
                let rh = app.state::<RuntimeHandle>().0.clone();
                rh.spawn(async move {
                    use tokio::signal::unix::{signal, SignalKind};
                    let Ok(mut term) = signal(SignalKind::terminate()) else {
                        return;
                    };
                    let Ok(mut intr) = signal(SignalKind::interrupt()) else {
                        return;
                    };
                    tokio::select! {
                        _ = term.recv() => {}
                        _ = intr.recv() => {}
                    }
                    exit_handle.exit(0);
                });
            }

            // Spawn event forwarder: core → UI. Two channels so a torrent
            // of log lines can't starve a permission prompt — both fan
            // into the same `core-event` Tauri event so the UI handler
            // stays uniform.
            let sup = app.state::<AppState>().supervisor.clone();
            let rh = app.state::<RuntimeHandle>().0.clone();
            let app_critical = tray_handle.clone();
            let mut critical_rx: broadcast::Receiver<_> = sup.subscribe();
            rh.spawn(async move {
                use session_manager_core::events::CoreEvent;
                use tauri_plugin_notification::NotificationExt;
                loop {
                    match critical_rx.recv().await {
                        Ok(ev) => {
                            let _ = app_critical.emit("core-event", &ev);
                            let _ = tray::reflect_event(&app_critical, &ev);
                            // Route native-notification requests through
                            // `tauri-plugin-notification` so the banner
                            // appears as "Session Manager" with the right
                            // OS permission grant flow (not "Script Editor"
                            // as the osascript fallback would render).
                            if let CoreEvent::NotifyRequested { title, body, .. } = &ev {
                                let _ = app_critical
                                    .notification()
                                    .builder()
                                    .title(title.clone())
                                    .body(body.clone())
                                    .show();
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            let app_logs = tray_handle.clone();
            let mut logs_rx: broadcast::Receiver<_> = sup.subscribe_logs();
            rh.spawn(async move {
                loop {
                    match logs_rx.recv().await {
                        Ok(ev) => {
                            let _ = app_logs.emit("core-event", &ev);
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Show the window unless launched with --hidden (autostart).
            let args: Vec<String> = std::env::args().collect();
            let hidden = args.iter().any(|a| a == "--hidden");
            if !hidden {
                if let Some(w) = app.get_webview_window("main") {
                    w.show().ok();
                    w.set_focus().ok();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // Don't quit on window close — stay in the tray (§7.2 model a).
                api.prevent_close();
                let _ = window.hide();
            }
            // Popover dismiss-on-click-outside: when the popover loses
            // focus, hide it. The user can re-open it with another tray
            // click. We deliberately only do this for the popover label;
            // the main window stays visible across focus changes (a
            // normal app expectation).
            tauri::WindowEvent::Focused(false) => {
                if window.label() == "popover" {
                    let _ = window.hide();
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(move |_handle, event| {
            // On user-initiated Quit (tray menu or app menu), gracefully
            // stop every running session before the process exits so we
            // don't leave orphaned agents behind (§8.1).
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                let sup = Arc::clone(&supervisor_for_shutdown);
                runtime_handle.block_on(async move { sup.shutdown().await });
            }
        });
}

pub struct RuntimeHandle(pub tokio::runtime::Handle);

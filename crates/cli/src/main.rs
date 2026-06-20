//! `session-manager` CLI.
//!
//! Connects to a running supervisor over a Unix domain socket when one is
//! available — so you can `session-manager start <id>` while the GUI is
//! running. Falls back to opening its own supervisor (and serving the same
//! socket) when `session-manager daemon` is the only thing on the box.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;
#[cfg(unix)]
use session_manager_core::ipc;
use session_manager_core::{
    backend::{registry, AuthState},
    config::{PermissionMode, ResumeMode, SessionConfig, SessionsFile},
    paths,
    state::RuntimeState,
    Supervisor,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    version,
    about = "Local supervisor for AI coding-agent sessions",
    long_about = "Local supervisor for AI coding-agent sessions.\n\n\
This CLI talks to a running supervisor over a local socket when one is up \
(the GUI app or `session-manager daemon`). For read-only commands (`list`, \
`status`, `path`, `auth`, `logs`) you can run it anytime — the CLI reads the \
config files directly when no supervisor is running."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List all defined sessions and their current state.
    List,
    /// Show status of a single session.
    Status { id: String },
    /// Start a session.
    Start { id: String },
    /// Stop a session (or all, with --all).
    Stop {
        id: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Restart a session.
    Restart { id: String },
    /// Print or tail the per-session log.
    Logs {
        id: String,
        #[arg(short, long)]
        follow: bool,
    },
    /// Add a new session.
    New {
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        path: String,
        #[arg(long, default_value = "claude-code")]
        agent: String,
        #[arg(long, default_value_t = false)]
        no_remote: bool,
        #[arg(long, default_value = "ask")]
        permission: String,
        #[arg(long, default_value = "continue")]
        resume: String,
        #[arg(long, default_value = "default")]
        model: String,
        #[arg(long)]
        keep_awake: bool,
        #[arg(long)]
        no_auto_restart: bool,
        #[arg(long)]
        group: Option<String>,
        /// Wrap the agent in `script(1)` to capture a full PTY transcript.
        #[arg(long)]
        record_stdout: bool,
    },
    /// Delete a session from config.
    Delete { id: String },
    /// Print the config file path.
    Path,
    /// Show backend login state.
    Auth,
    /// Run the supervisor headless (foreground). Useful for systemd / launchd
    /// or when there's no GUI.
    Daemon,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();

    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Daemon => return run_daemon().await,
        Cmd::List => return run_list().await,
        Cmd::Status { id } => return run_status(&id).await,
        Cmd::Logs { id, follow } => return run_logs(&id, follow).await,
        Cmd::Path => {
            println!("{}", paths::config_file()?.display());
            return Ok(());
        }
        Cmd::Auth => return run_auth().await,
        _ => {}
    }

    // State-changing commands require a running supervisor — either the GUI
    // app or `session-manager daemon`. We don't start one inline because the
    // CLI exiting would kill any worker we just spawned.
    #[cfg(unix)]
    {
        let sock = ipc::socket_path()?;
        let mut client = match ipc::Client::try_connect(&sock).await? {
            Some(c) => c,
            None => {
                anyhow::bail!(
                    "no supervisor is running.\n\
                     Start the GUI app, or run `session-manager daemon` in another terminal, \
                     then retry."
                );
            }
        };
        return dispatch_remote(&mut client, cli.cmd).await;
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("CLI client mode is currently Unix-only. Use the GUI app.");
    }
}

async fn run_daemon() -> Result<()> {
    let sup = Arc::new(Supervisor::open()?);
    sup.start().await?;
    println!("supervisor running; ctrl-c or SIGTERM to exit");
    wait_for_termination().await;
    println!("shutting down sessions…");
    sup.shutdown().await;
    Ok(())
}

#[cfg(unix)]
async fn wait_for_termination() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_termination() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(unix)]
async fn dispatch_remote(client: &mut ipc::Client, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Start { id } => {
            let _: serde_json::Value = client.call("start", json!({ "id": id })).await?;
            println!("started {id}");
        }
        Cmd::Stop { id, all } => {
            if all {
                let _: serde_json::Value = client.call("stop_all", json!({})).await?;
                println!("stopped all");
            } else if let Some(id) = id {
                let _: serde_json::Value = client.call("stop", json!({ "id": id })).await?;
                println!("stopped {id}");
            } else {
                anyhow::bail!("`stop` requires an id or --all");
            }
        }
        Cmd::Restart { id } => {
            let _: serde_json::Value = client.call("restart", json!({ "id": id })).await?;
            println!("restarting {id}");
        }
        Cmd::New {
            id,
            name,
            path,
            agent,
            no_remote,
            permission,
            resume,
            model,
            keep_awake,
            no_auto_restart,
            group,
            record_stdout,
        } => {
            let session = build_session(
                &id,
                name,
                path,
                agent,
                no_remote,
                &permission,
                &resume,
                model,
                keep_awake,
                no_auto_restart,
                group,
                record_stdout,
            )?;
            let _: serde_json::Value = client.call("create", json!({ "session": session })).await?;
            println!("created {id}");
        }
        Cmd::Delete { id } => {
            let _: serde_json::Value = client.call("delete", json!({ "id": id })).await?;
            println!("deleted {id}");
        }
        _ => unreachable!("read-only / daemon commands handled before dispatch_remote"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_session(
    id: &str,
    name: Option<String>,
    path: String,
    agent: String,
    no_remote: bool,
    permission: &str,
    resume: &str,
    model: String,
    keep_awake: bool,
    no_auto_restart: bool,
    group: Option<String>,
    record_stdout: bool,
) -> Result<SessionConfig> {
    let perm = match permission {
        "safe" => PermissionMode::Safe,
        "ask" => PermissionMode::Ask,
        "danger" => PermissionMode::Danger,
        other => anyhow::bail!("unknown permission `{other}` (safe|ask|danger)"),
    };
    let res = match resume {
        "continue" => ResumeMode::Continue,
        "resume" => ResumeMode::Resume,
        "fresh" => ResumeMode::Fresh,
        other => anyhow::bail!("unknown resume `{other}` (continue|resume|fresh)"),
    };
    Ok(SessionConfig {
        id: id.into(),
        name: name.unwrap_or_else(|| id.into()),
        agent,
        path,
        remote: !no_remote,
        permission: perm,
        resume: res,
        resume_id: String::new(),
        model,
        keep_awake,
        auto_restart: !no_auto_restart,
        restart_max: 5,
        env: Default::default(),
        log_path: None,
        group,
        record_stdout,
        extra_args: Vec::new(),
    })
}

// -- read-only commands ------------------------------------------------------

#[cfg(unix)]
async fn try_remote_snapshot() -> Result<Option<(SessionsFile, RuntimeState)>> {
    let sock = ipc::socket_path()?;
    if let Some(mut client) = ipc::Client::try_connect(&sock).await? {
        let snap: ipc::Snapshot = client.call("snapshot", json!({})).await?;
        return Ok(Some((snap.file, snap.runtime)));
    }
    Ok(None)
}
#[cfg(not(unix))]
async fn try_remote_snapshot() -> Result<Option<(SessionsFile, RuntimeState)>> {
    Ok(None)
}

async fn snapshot() -> Result<(SessionsFile, RuntimeState)> {
    if let Some(s) = try_remote_snapshot().await? {
        return Ok(s);
    }
    // Fall back to reading the on-disk files directly. Safe because we
    // didn't try to open a Supervisor, so no lock is taken.
    let file = SessionsFile::load(&paths::config_file()?)?;
    let runtime = RuntimeState::load_or_default(&paths::state_file()?);
    Ok((file, runtime))
}

async fn run_list() -> Result<()> {
    let (file, runtime) = snapshot().await?;
    if file.sessions.is_empty() {
        println!("no sessions defined. Use `session-manager new --id <id> --path <dir>`.");
        return Ok(());
    }
    println!(
        "{:<24} {:<14} {:<10} {:<14} PATH",
        "ID", "STATUS", "AGENT", "PERMISSION"
    );
    for s in &file.sessions {
        let rt = runtime.sessions.get(&s.id);
        let status = rt.map(|r| r.status.slug()).unwrap_or("stopped");
        println!(
            "{:<24} {:<14} {:<10} {:<14} {}",
            s.id,
            status,
            s.agent,
            format!("{:?}", s.permission).to_lowercase(),
            s.path
        );
    }
    Ok(())
}

async fn run_status(id: &str) -> Result<()> {
    let (_file, runtime) = snapshot().await?;
    let Some(rt) = runtime.sessions.get(id) else {
        anyhow::bail!("session {id} not found");
    };
    println!("{}", serde_json::to_string_pretty(rt)?);
    Ok(())
}

async fn run_logs(id: &str, follow: bool) -> Result<()> {
    let (file, _) = snapshot().await?;
    let Some(s) = file.sessions.iter().find(|s| s.id == id) else {
        anyhow::bail!("session {id} not found");
    };
    let path = s.resolved_log_path()?;
    if !path.exists() {
        anyhow::bail!("no log yet at {}", path.display());
    }
    if follow {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut file = tokio::fs::File::open(&path).await?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf).await?;
            if n > 0 {
                let s = String::from_utf8_lossy(&buf[..n]);
                print!("{s}");
                use std::io::Write;
                std::io::stdout().flush().ok();
                continue;
            }
            let cur = file.stream_position().await?;
            let len = tokio::fs::metadata(&path).await?.len();
            if cur > len {
                file = tokio::fs::File::open(&path).await?;
                continue;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    } else {
        let body = tokio::fs::read_to_string(&path).await?;
        print!("{body}");
    }
    Ok(())
}

async fn run_auth() -> Result<()> {
    for b in registry() {
        let backend = session_manager_core::backend::make_backend(&b.id)?;
        let state = backend.auth_state().await.unwrap_or(AuthState::Unknown);
        let label = match state {
            AuthState::LoggedIn => "logged in",
            AuthState::LoggedOut => "installed, not logged in",
            AuthState::BinaryMissing => "not installed",
            AuthState::Unknown => "unknown",
        };
        println!("{:<14}  {}", b.display_name, label);
    }
    Ok(())
}

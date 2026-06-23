import { DangerBadge } from "../components/DangerBadge";
import { StatusDot } from "../components/StatusDot";
import { Transcript } from "../components/Transcript";
import { api } from "../lib/api";
import { go } from "../lib/router";
import { useStore } from "../lib/store";

export function SessionDetail({ id }: { id: string }) {
  const session = useStore((s) => s.sessions.find((x) => x.id === id));
  const runtime = useStore((s) => s.runtime.sessions[id]);
  const transcripts = useStore((s) => s.transcripts[id]);
  const logs = useStore((s) => s.logs[id]);
  const start = useStore((s) => s.start);
  const stop = useStore((s) => s.stop);
  const restart = useStore((s) => s.restart);
  const remove = useStore((s) => s.remove);

  if (!session) {
    return (
      <>
        <header className="top">
          <button className="btn ghost" onClick={() => go("/")}>← Sessions</button>
        </header>
        <div className="content">
          <div className="empty-state"><p>Session not found.</p></div>
        </div>
      </>
    );
  }
  const status = runtime?.status ?? "stopped";
  const running = status !== "stopped" && status !== "done" && status !== "crashed";

  return (
    <>
      <header className="top">
        <button className="btn ghost" onClick={() => go("/")}>← Sessions</button>
        <div>
          <div className="title" style={{ display: "flex", alignItems: "center", gap: 10 }}>
            {session.name}
            <StatusDot status={status} />
            {session.permission === "danger" && <DangerBadge />}
          </div>
          <div className="sub mono">{session.path}</div>
        </div>
        <div className="spacer" />
        <button className="btn" onClick={() => go(`/edit/${encodeURIComponent(session.id)}`)}>Edit</button>
        {running ? (
          <button className="btn" onClick={() => stop(session.id)}>Stop</button>
        ) : (
          <button className="btn primary" onClick={() => start(session.id)}>Start</button>
        )}
        <button className="btn" onClick={() => restart(session.id)}>Restart</button>
      </header>
      <div className="content">
        {session.permission === "danger" && (
          <div className="danger-banner" style={{ marginBottom: 16, flexWrap: "wrap" }}>
            <strong>Running with permissions skipped.</strong>
            <span>This session can edit files and run commands without asking.</span>
            {session.remote && (
              <span style={{ flexBasis: "100%", marginTop: 6, color: "var(--text-soft)", fontSize: 12 }}>
                Combined with remote control, the danger surface is large. Consider isolating
                sensitive repos in a container or dedicated user account.
              </span>
            )}
          </div>
        )}

        {status === "needs-permission" && (
          <div
            className="danger-banner"
            style={{ marginBottom: 16, background: "rgba(231, 183, 95, 0.10)", borderColor: "rgba(231, 183, 95, 0.45)", color: "var(--warn)" }}
          >
            <strong>Waiting on a permission prompt.</strong>
            <span>Session Manager only signals this — answer the prompt in the agent app{session.remote ? " or remote channel" : ""}.</span>
            {runtime?.remote_url && (
              <button className="btn" onClick={() => runtime.remote_url && api.openInOs(runtime.remote_url).catch(console.error)}>
                Open in agent app
              </button>
            )}
          </div>
        )}

        {status === "crashed" && session.auto_restart && (runtime?.restart_count ?? 0) >= session.restart_max && (
          <div className="danger-banner" style={{ marginBottom: 16, flexWrap: "wrap" }}>
            <strong>Stopped retrying.</strong>
            <span>
              Failed {session.restart_max} times in a row. Inspect the log and reset the counter to try again.
            </span>
            <button
              className="btn"
              onClick={async () => {
                try {
                  const target = await api.resolvedLogPath(session.id);
                  await api.openInOs(target);
                } catch (err) { console.error(err); }
              }}
            >
              Open log
            </button>
            <button
              className="btn primary"
              onClick={() => api.resetAndRetry(session.id).catch((err) => alert(`Retry failed: ${err}`))}
            >
              Reset & retry
            </button>
          </div>
        )}

        <div className="detail-grid">
          <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
            <div className="card padded">
              <h3 className="section-title" style={{ marginBottom: 12 }}>
                Configuration
              </h3>
              <dl className="kv">
                <dt>Agent</dt><dd>{session.agent}</dd>
                <dt>Working directory</dt><dd className="mono">{session.path}</dd>
                <dt>Permission mode</dt><dd>{session.permission}</dd>
                <dt>Remote control</dt><dd>{session.remote ? `on (${runtime?.remote_online ? "online" : "offline"})` : "off"}</dd>
                <dt>Resume on start</dt><dd>{session.resume}{session.resume === "resume" && session.resume_id ? ` · ${session.resume_id}` : ""}</dd>
                <dt>Model</dt><dd>{session.model}</dd>
                <dt>Auto restart</dt><dd>{session.auto_restart ? `yes (max ${session.restart_max})` : "no"}</dd>
                {session.group && <><dt>Group</dt><dd>{session.group}</dd></>}
                {runtime?.reason && <><dt>Last reason</dt><dd>{runtime.reason}</dd></>}
              </dl>
            </div>

            <div className="card padded">
              <h3 className="section-title" style={{ marginBottom: 12 }}>
                Recent activity
              </h3>
              <div className="recent-tail-wrap">
                <div className="recent-tail" tabIndex={0}>
                  {transcripts && transcripts.length > 0 ? (
                    <Transcript lines={transcripts} />
                  ) : logs && logs.length > 0 ? (
                    logs.map((l, i) => (
                      <div key={i} className={l.is_stderr ? "stderr" : ""}>{l.line}</div>
                    ))
                  ) : (
                    <span className="muted">No output captured yet.</span>
                  )}
                </div>
              </div>
              <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
                <button
                  className="btn"
                  onClick={async () => {
                    try {
                      const target = await api.resolvedLogPath(session.id);
                      await api.openInOs(target);
                    } catch (err) {
                      console.error(err);
                    }
                  }}
                >
                  Open log
                </button>
                <button className="btn" onClick={() => api.revealPath(session.path).catch(console.error)}>
                  Reveal folder
                </button>
              </div>
            </div>
          </div>

          <aside style={{ display: "flex", flexDirection: "column", gap: 16 }}>
            <div className="aside-card remote-card">
              <div className="aside-card-head">
                <h3>Remote control</h3>
                {session.remote && (
                  <span className={`pulse-dot ${runtime?.remote_online ? "online" : "offline"}`}>
                    {runtime?.remote_online ? "Online" : "Offline"}
                  </span>
                )}
              </div>
              {!session.remote ? (
                <p className="muted">
                  Remote is off. Turn it on in{" "}
                  <button className="btn ghost" onClick={() => go(`/edit/${encodeURIComponent(session.id)}`)}>edit</button>.
                </p>
              ) : runtime?.remote_url ? (
                <>
                  {runtime.remote_qr && (
                    <div
                      className="qr-frame"
                      // SVG comes from the supervisor's `render_qr_svg`. It's
                      // generated server-side from the URL we already trust —
                      // safe to inline.
                      dangerouslySetInnerHTML={{ __html: runtime.remote_qr }}
                    />
                  )}
                  <p className="remote-url mono" title={runtime.remote_url}>
                    {runtime.remote_url}
                  </p>
                  <button
                    className="btn primary block"
                    onClick={() => runtime.remote_url && api.openInOs(runtime.remote_url).catch(console.error)}
                  >
                    Open in agent app
                  </button>
                </>
              ) : (
                <p className="muted">Waiting for the agent to announce its URL…</p>
              )}
            </div>

            <div className="aside-card">
              <h3>Supervision</h3>
              <dl className="kv">
                <dt>PID</dt><dd className="mono">{runtime?.pid ?? "—"}</dd>
                <dt>Started</dt><dd className="mono">{runtime?.started_at ?? "—"}</dd>
                <dt>Restarts</dt><dd>{runtime?.restart_count ?? 0}</dd>
              </dl>
            </div>

            {/* Delete is intentionally visible (no disclosure widget). The
                previous "Danger zone" collapsible was reported as undiscoverable:
                users with broken sessions couldn't find a way to remove them
                and had to drop to the CLI. confirm() guards against misclick. */}
            <div className="aside-card">
              <div style={{ fontWeight: 600, marginBottom: 4 }}>Delete session</div>
              <p className="muted" style={{ marginTop: 0, marginBottom: 8 }}>
                Removes the session from config. Stops it first.
              </p>
              <button
                className="btn danger"
                onClick={() => {
                  if (confirm(`Delete session "${session.name}"?`)) {
                    void remove(session.id).then(() => go("/"));
                  }
                }}
              >
                Delete session
              </button>
            </div>
          </aside>
        </div>
      </div>
    </>
  );
}

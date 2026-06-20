import { useEffect, useMemo, useRef, useState } from "react";

import { DangerBadge } from "../components/DangerBadge";
import { StatusDot } from "../components/StatusDot";
import { api } from "../lib/api";
import { go, useRoute } from "../lib/router";
import { useStore } from "../lib/store";
import type { DiscoveredSession, SessionConfig, SessionStatus } from "../types";

const STATUS_RANK: SessionStatus[] = [
  "needs-permission",
  "crashed",
  "working",
  "rate-limited",
  "starting",
  "idle",
  "offline",
  "done",
  "stopped",
];
function rank(s: SessionStatus) {
  const i = STATUS_RANK.indexOf(s);
  return i === -1 ? 99 : i;
}

function slug(s: string): string {
  return s.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
}

function describeFleet(managed: number, external: number): string {
  const parts: string[] = [];
  parts.push(`${managed} managed`);
  if (external > 0) parts.push(`${external} external`);
  return parts.join(" · ");
}

function uniqueId(base: string, sessions: SessionConfig[]): string {
  if (!sessions.some((s) => s.id === base)) return base;
  for (let i = 2; i < 1000; i++) {
    const cand = `${base}-${i}`;
    if (!sessions.some((s) => s.id === cand)) return cand;
  }
  return `${base}-${Date.now()}`;
}

function detectPermissionFromArgs(args: string[]): "safe" | "ask" | "danger" {
  if (args.some((a) => a.includes("dangerously-skip-permissions") || a.includes("bypass-approvals"))) {
    return "danger";
  }
  return "ask";
}

export function Dashboard() {
  const sessions = useStore((s) => s.sessions);
  const runtime = useStore((s) => s.runtime);
  const start = useStore((s) => s.start);
  const stop = useStore((s) => s.stop);
  const stopAll = useStore((s) => s.stopAll);
  const externalAll = useStore((s) => s.external);
  const stopExternal = useStore((s) => s.stopExternal);
  const refreshExternal = useStore((s) => s.refreshExternal);

  const [filter, setFilter] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);

  // URL-driven group filter — sidebar's `/?group=Work` populates this.
  // Empty / missing means "all groups". Implemented via the router so
  // the active filter survives reloads and links can be shared.
  const route = useRoute();
  const groupFilter =
    route.name === "dashboard" ? route.query?.get("group") ?? null : null;

  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase();
    let pool = sessions;
    if (groupFilter) {
      pool = pool.filter((s) => (s.group ?? "") === groupFilter);
    }
    if (!q) return pool;
    return pool.filter(
      (s) =>
        s.name.toLowerCase().includes(q) ||
        s.id.toLowerCase().includes(q) ||
        s.path.toLowerCase().includes(q) ||
        (s.group ?? "").toLowerCase().includes(q),
    );
  }, [sessions, filter, groupFilter]);

  const sortedSessions = useMemo(
    () =>
      [...filtered].sort(
        (a, b) =>
          rank(runtime.sessions[a.id]?.status ?? "stopped") -
          rank(runtime.sessions[b.id]?.status ?? "stopped"),
      ),
    [filtered, runtime],
  );

  const groups = new Map<string, SessionConfig[]>();
  for (const s of sortedSessions) {
    const key = s.group ?? "";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key)!.push(s);
  }

  // External (unmanaged) — only show ones that don't already correspond to a
  // managed session at the same cwd, otherwise the user sees both. Also
  // filter by the dashboard search so /-search works against external rows.
  const external: DiscoveredSession[] = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return externalAll
      .filter((e) => !e.matches_session_id)
      .filter(
        (e) =>
          !q ||
          e.display_name.toLowerCase().includes(q) ||
          e.cwd.toLowerCase().includes(q) ||
          e.backend_id.toLowerCase().includes(q),
      );
  }, [externalAll, filter]);

  /**
   * Build the SessionConfig we'll persist for an external row.
   * Pulled into a helper because Manage and Take-over both need it.
   */
  const buildConfigFor = (e: DiscoveredSession): SessionConfig => {
    const id = uniqueId(slug(e.display_name) || `external-${e.pid}`, sessions);
    return {
      id,
      name: e.display_name,
      agent: e.backend_id,
      path: e.cwd,
      permission: detectPermissionFromArgs(e.args),
      remote: true,
      resume: "continue",
      resume_id: "",
      model: "default",
      keep_awake: false,
      auto_restart: true,
      restart_max: 5,
      env: {},
      log_path: null,
      group: null,
      record_stdout: e.args.some((a) => a === "script") || e.args.some((a) => a.startsWith("/tmp/rc_")),
      extra_args: [],
    };
  };

  /** Manage = claim without killing — running process is preserved. */
  const manageOne = async (e: DiscoveredSession) => {
    try {
      await api.claimExternal(e.pid, buildConfigFor(e));
    } catch (err) {
      alert(`Manage failed: ${err}`);
    }
  };

  /** Take over now = SIGTERM the external + respawn fresh under us. */
  const takeOverOne = async (e: DiscoveredSession) => {
    try {
      await api.adoptExternal(e.pid, buildConfigFor(e));
    } catch (err) {
      alert(`Take over failed: ${err}`);
    }
  };

  // Keyboard shortcuts. Mod = ⌘ on mac, Ctrl elsewhere.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      const target = e.target as HTMLElement | null;
      const inField =
        !!target &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT");
      if (mod && e.key.toLowerCase() === "n") {
        e.preventDefault();
        go("/new");
      } else if (mod && e.key === ",") {
        e.preventDefault();
        go("/settings");
      } else if (e.key === "/" && !inField) {
        e.preventDefault();
        searchRef.current?.focus();
      } else if (e.key === "Escape") {
        if (filter) {
          setFilter("");
        } else {
          searchRef.current?.blur();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [filter]);

  return (
    <>
      <header className="top">
        <div>
          <div className="title" style={{ display: "flex", alignItems: "center", gap: 8 }}>
            {groupFilter ? groupFilter : "All sessions"}
            {groupFilter && (
              <button
                className="btn ghost small"
                title="Clear group filter"
                onClick={() => go("/")}
              >
                ✕
              </button>
            )}
          </div>
          <div className="sub">
            {sessions.length === 0 && externalAll.length === 0
              ? "Nothing here yet."
              : describeFleet(filtered.length, externalAll.length)}
          </div>
        </div>
        <div className="spacer" />
        <div className="search-box">
          <input
            ref={searchRef}
            className="search-input"
            placeholder="Search…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          {!filter && <span className="search-key">/</span>}
        </div>
        <button
          className="btn ghost"
          onClick={() => {
            const running = sessions.filter((s) => {
              const st = runtime.sessions[s.id]?.status;
              return st && st !== "stopped" && st !== "done" && st !== "crashed";
            }).length;
            if (running === 0) return;
            if (confirm(`Stop ${running} running session${running === 1 ? "" : "s"}? In-flight work will be lost.`)) {
              void stopAll();
            }
          }}
        >
          Stop all
        </button>
        <button className="btn primary" onClick={() => go("/new")} title="⌘N">New session</button>
      </header>
      <div className="content">
        {sortedSessions.length === 0 && external.length === 0 && filter && (
          <div className="empty-state">
            <p>No sessions match "{filter}".</p>
            <button className="btn ghost cta" onClick={() => setFilter("")}>Clear search</button>
          </div>
        )}
        {external.length > 0 && (
          <section className="section">
            <div className="row-spread" style={{ marginBottom: 12 }}>
              <h3 className="section-title">Running outside Session Manager ({external.length})</h3>
              <div className="hstack gap-1">
                <button className="btn ghost" onClick={() => void refreshExternal()}>
                  Re-scan
                </button>
                <button
                  className="btn"
                  onClick={async () => {
                    if (!confirm(
                      `Manage all ${external.length} external sessions?\n\n` +
                      `Each gets a managed config; the running processes keep running. ` +
                      `Use a session's Restart to actually hand it to the supervisor later.`
                    )) return;
                    for (const e of external) {
                      try { await manageOne(e); } catch (err) { console.error(err); }
                    }
                    await refreshExternal();
                  }}
                >
                  Manage all
                </button>
              </div>
            </div>
            <div className="list">
              {external.map((e) => {
                const st = e.status ?? "idle";
                return (
                  <div key={e.pid} className="session-row static external">
                    <div>
                      <div className="meta">
                        <span className="name">{e.display_name}</span>
                        <StatusDot status={st} />
                        <span className="tag">{e.backend_id}</span>
                        <span className="tag">PID {e.pid}</span>
                        <span className="tag info">external</span>
                      </div>
                      <div className="subline">{e.cwd}</div>
                    </div>
                    <div className="ctrl" onClick={(ev) => ev.stopPropagation()}>
                      <button
                        className="btn small"
                        onClick={() => void api.openInOs(e.cwd).catch(console.error)}
                      >
                        Reveal
                      </button>
                      <button
                        className="btn small"
                        onClick={() => {
                          if (confirm(`Send SIGTERM to PID ${e.pid} (${e.display_name})?`)) {
                            void stopExternal(e.pid);
                          }
                        }}
                      >
                        Stop
                      </button>
                      <button
                        className="btn small primary"
                        title="Add to managed config without killing the process. The running session keeps its context."
                        onClick={async () => {
                          await manageOne(e);
                          await refreshExternal();
                        }}
                      >
                        Manage
                      </button>
                      <button
                        className="btn small"
                        title="Stop the external NOW and respawn under the supervisor (loses unsaved context)."
                        onClick={async () => {
                          if (!confirm(`Take over "${e.display_name}" now?\n\nThis stops PID ${e.pid} and re-launches fresh under Session Manager.`)) return;
                          await takeOverOne(e);
                          await refreshExternal();
                        }}
                      >
                        Take over
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          </section>
        )}

        {sessions.length > 0 && (
          <h3 className="section-title" style={{ marginBottom: 12 }}>Managed ({sessions.length})</h3>
        )}
        {[...groups.entries()].map(([group, rows]) => (
          <section key={group} className="section">
            {group && (
              <h3 className="section-title" style={{ marginBottom: 12 }}>{group}</h3>
            )}
            <div className="list">
              {rows.map((s) => {
                const rt = runtime.sessions[s.id];
                const status: SessionStatus = rt?.status ?? "stopped";
                const danger = s.permission === "danger";
                const running = status !== "stopped" && status !== "done" && status !== "crashed";
                return (
                  <div
                    key={s.id}
                    className={`session-row ${danger ? "danger" : ""}`}
                    onClick={() => go(`/session/${encodeURIComponent(s.id)}`)}
                  >
                    <div>
                      <div className="meta">
                        <span className="name">{s.name}</span>
                        <StatusDot status={status} />
                        {/* remote tag only when its state would surprise — */}
                        {/* hide "remote offline" on stopped/crashed/done.  */}
                        {s.remote && running && rt?.remote_online && (
                          <span className="tag good">remote connected</span>
                        )}
                        {s.group && <span className="tag">{s.group}</span>}
                        {danger && <DangerBadge compact />}
                      </div>
                      <div className="subline">{s.path}</div>
                    </div>
                    <div className="ctrl" onClick={(e) => e.stopPropagation()}>
                      {running ? (
                        <button className="btn small" onClick={() => stop(s.id)}>Stop</button>
                      ) : (
                        <button className="btn small primary" onClick={() => start(s.id)}>Start</button>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          </section>
        ))}
      </div>
    </>
  );
}

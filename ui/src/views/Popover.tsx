import { useMemo } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { exit } from "@tauri-apps/plugin-process";

import { StatusDot } from "../components/StatusDot";
import { api } from "../lib/api";
import { go } from "../lib/router";
import { useStore } from "../lib/store";
import type { SessionStatus } from "../types";

/**
 * Frameless tray popover — the "click-target *is* the overview" view.
 *
 * Rendered into the secondary "popover" Tauri window (declared in
 * tauri.conf.json, opens at `#/popover`). Shares the Zustand store with
 * the main window so session state + status updates appear live.
 *
 * Layout intentionally mirrors Claude God's popover style: header card
 * (status counters + Awake badge), per-session rows with a status dot +
 * inline Stop/Start affordance, and a pinned footer (New / Stop all /
 * Open / Quit).
 *
 * The window auto-hides on focus-loss (lib.rs); inside the popover we
 * close it ourselves after navigating to the main window so the user
 * doesn't have to dismiss the popover and *then* see the window.
 */
export function Popover() {
  const sessions = useStore((s) => s.sessions);
  const runtime = useStore((s) => s.runtime);
  const externalCount = useStore((s) => s.external.length);
  const ready = useStore((s) => s.ready);
  const start = useStore((s) => s.start);
  const stop = useStore((s) => s.stop);
  const stopAll = useStore((s) => s.stopAll);

  const counts = useMemo(() => {
    let running = 0,
      needsPerm = 0,
      crashed = 0;
    for (const s of sessions) {
      const st = runtime.sessions[s.id]?.status ?? "stopped";
      if (
        st === "working" ||
        st === "idle" ||
        st === "starting" ||
        st === "rate-limited" ||
        st === "needs-permission" ||
        st === "offline"
      )
        running += 1;
      if (st === "needs-permission") needsPerm += 1;
      if (st === "crashed") crashed += 1;
    }
    return { running, needsPerm, crashed };
  }, [sessions, runtime.sessions]);

  // Click rows are sorted: attention first (needs-permission / crashed),
  // then alphabetical. Matches the native menu order so muscle memory
  // carries over.
  const sortedSessions = useMemo(() => {
    const rank = (st: SessionStatus): number => {
      switch (st) {
        case "needs-permission":
          return 0;
        case "crashed":
          return 1;
        case "working":
          return 2;
        case "starting":
          return 3;
        case "rate-limited":
          return 4;
        case "idle":
          return 5;
        case "offline":
          return 6;
        case "done":
          return 7;
        case "stopped":
        default:
          return 8;
      }
    };
    return [...sessions].sort((a, b) => {
      const stA = (runtime.sessions[a.id]?.status ?? "stopped") as SessionStatus;
      const stB = (runtime.sessions[b.id]?.status ?? "stopped") as SessionStatus;
      const ra = rank(stA);
      const rb = rank(stB);
      if (ra !== rb) return ra - rb;
      return a.name.localeCompare(b.name);
    });
  }, [sessions, runtime.sessions]);

  /** Close the popover after firing a navigation, so the main window
   *  gets the spotlight without the user manually dismissing. */
  const closeSelf = async () => {
    try {
      await getCurrentWindow().hide();
    } catch {
      /* in dev browser this is a no-op */
    }
  };

  const openMain = async (route?: string) => {
    if (route) go(route);
    // Tauri-only: bring the main window to front. The popover hides
    // itself once we yield focus.
    try {
      // Lazy import to keep this file usable in the dev browser stub.
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("focus_main_window").catch(() => {});
    } catch {
      /* ignore */
    }
    await closeSelf();
  };

  if (!ready) {
    return <div className="popover-loading">Loading…</div>;
  }

  return (
    <div className="popover-root">
      {/* Header — counters at a glance, matches the native menu summary
          line so users who know it already feel at home. */}
      <div className="popover-header">
        <div className="popover-title">Session Manager</div>
        <div className="popover-counts">
          {counts.running > 0 && <span className="pc good">● {counts.running} running</span>}
          {counts.needsPerm > 0 && <span className="pc warn">● {counts.needsPerm} needs perm</span>}
          {counts.crashed > 0 && <span className="pc danger">● {counts.crashed} crashed</span>}
          {externalCount > 0 && <span className="pc info">● {externalCount} external</span>}
          {counts.running === 0 && counts.crashed === 0 && externalCount === 0 && (
            <span className="pc muted">Nothing running</span>
          )}
        </div>
      </div>

      {/* Sessions list */}
      <div className="popover-body">
        {sortedSessions.length === 0 ? (
          <div className="popover-empty">No sessions yet. Click "New session" to get started.</div>
        ) : (
          sortedSessions.map((s) => {
            const st = (runtime.sessions[s.id]?.status ?? "stopped") as SessionStatus;
            const isRunning =
              st === "working" ||
              st === "idle" ||
              st === "starting" ||
              st === "rate-limited" ||
              st === "needs-permission" ||
              st === "offline";
            const attention =
              st === "needs-permission" ? "warn" : st === "crashed" ? "danger" : null;
            return (
              <div key={s.id} className={`popover-row ${attention ? `attn-${attention}` : ""}`}>
                <button
                  type="button"
                  className="popover-row-main"
                  title={`Open ${s.name} — ${st.replace("-", " ")}`}
                  onClick={() => void openMain(`/session/${encodeURIComponent(s.id)}`)}
                >
                  <StatusDot status={st} label={false} />
                  <span className="popover-row-name">{s.name}</span>
                  {attention === "warn" && (
                    <span className="popover-row-tag warn">needs perm</span>
                  )}
                  {attention === "danger" && (
                    <span className="popover-row-tag danger">crashed</span>
                  )}
                </button>
                <button
                  type="button"
                  className={`popover-row-action ${isRunning ? "" : "primary"}`}
                  title={isRunning ? `Stop ${s.name}` : `Start ${s.name}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    if (isRunning) void stop(s.id);
                    else void start(s.id);
                  }}
                >
                  {isRunning ? "Stop" : "Start"}
                </button>
              </div>
            );
          })
        )}
      </div>

      {/* Footer — pinned actions, same order as the native menu */}
      <div className="popover-footer">
        <button className="btn block primary" onClick={() => void openMain("/new")}>
          + New session
        </button>
        <div className="popover-footer-row">
          <button
            className="btn small"
            disabled={counts.running === 0}
            onClick={() => void stopAll()}
          >
            Stop all
          </button>
          <button className="btn small" onClick={() => void openMain("/")}>
            Open window
          </button>
          <button
            className="btn small"
            onClick={async () => {
              try {
                await api.stopAll();
              } catch {
                /* best-effort */
              }
              await exit(0);
            }}
            title="Quit Session Manager (stops all running sessions first)"
          >
            Quit
          </button>
        </div>
      </div>
    </div>
  );
}

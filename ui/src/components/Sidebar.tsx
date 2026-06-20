import { useRoute, go } from "../lib/router";
import { useStore } from "../lib/store";

/**
 * Sidebar layout (matches the original Keel mockup):
 *   - top: brand + Fleet entry, plus per-`group` entries when defined
 *   - middle (spacer): status counters (running / needs-permission / crashed)
 *   - bottom (pinned): primary actions — New session, Stop all, Settings
 *
 * The pinned-bottom actions were missing for a while; restored so the user
 * doesn't have to scroll-hunt for them.
 */
export function Sidebar() {
  const route = useRoute();
  const sessions = useStore((s) => s.sessions);
  const runtime = useStore((s) => s.runtime);
  const externalCount = useStore((s) => s.external.length);
  const stopAll = useStore((s) => s.stopAll);

  const counts = sessions.reduce(
    (acc, s) => {
      const st = runtime.sessions[s.id]?.status ?? "stopped";
      if (st === "needs-permission") acc.needsPerm += 1;
      // Mirror backend `SessionStatus::is_running()` — needs-permission +
      // offline both have a live process behind them and are stoppable, so
      // they count toward "running" for the Stop-all gate. The display
      // groups them with the friendlier word.
      if (
        st === "working" ||
        st === "idle" ||
        st === "starting" ||
        st === "rate-limited" ||
        st === "needs-permission" ||
        st === "offline"
      ) acc.running += 1;
      if (st === "crashed") acc.crashed += 1;
      return acc;
    },
    { needsPerm: 0, running: 0, crashed: 0 },
  );

  // Per-`group` lists (e.g. Work / Personal in the mockup). Derived from the
  // session config rather than a separate config store: groups are
  // effectively a label, not a first-class object.
  const groups = Array.from(
    sessions.reduce<Map<string, number>>((m, s) => {
      const g = s.group?.trim();
      if (!g) return m;
      m.set(g, (m.get(g) ?? 0) + 1);
      return m;
    }, new Map()).entries(),
  ).sort(([a], [b]) => a.localeCompare(b));

  const totalCount = sessions.length + externalCount;

  // "All sessions" is active when on the dashboard with NO group filter;
  // a group entry takes over the active state when that group is filtered.
  const activeGroup =
    route.name === "dashboard" ? route.query?.get("group") ?? null : null;
  const isDashboardAll = route.name === "dashboard" && !activeGroup;

  return (
    <aside className="sidebar">
      <div className="nav-section-label">Sessions</div>
      <button
        className={`nav-item ${isDashboardAll ? "active" : ""}`}
        onClick={() => go("/")}
      >
        All sessions
        {totalCount > 0 && <span className="badge">{totalCount}</span>}
      </button>
      {groups.map(([g, n]) => (
        <button
          key={g}
          className={`nav-item ${activeGroup === g ? "active" : ""}`}
          onClick={() => go(`/?group=${encodeURIComponent(g)}`)}
        >
          <span className="group-dot" aria-hidden />
          {g}
          <span className="badge">{n}</span>
        </button>
      ))}

      <div className="spacer" />

      {(counts.running + counts.needsPerm + counts.crashed > 0) && (
        <div className="sidebar-status">
          {counts.running > 0 && (
            <div className="status-line">
              <span className="dot run" /> {counts.running} running
            </div>
          )}
          {counts.needsPerm > 0 && (
            <div className="status-line">
              <span className="dot warn" /> {counts.needsPerm} needs permission
            </div>
          )}
          {counts.crashed > 0 && (
            <div className="status-line">
              <span className="dot danger" /> {counts.crashed} crashed
            </div>
          )}
        </div>
      )}

      <div className="sidebar-actions">
        <button
          className="btn primary block"
          onClick={() => go("/new")}
          title="⌘N"
        >
          + New session
        </button>
        <button
          className="btn block"
          onClick={() => void stopAll().catch(console.error)}
          disabled={counts.running === 0}
          title={counts.running === 0 ? "Nothing running" : "Stop every running session"}
        >
          <span className="square-glyph" aria-hidden /> Stop all
        </button>
        <button
          className={`btn ghost block ${route.name === "settings" ? "active" : ""}`}
          onClick={() => go("/settings")}
          title="⌘,"
        >
          <GearIcon /> Settings
        </button>
      </div>
    </aside>
  );
}

function GearIcon() {
  // Lucide "settings" gear — six rounded teeth, viewBox 0 0 24 24,
  // generally hand-drawn at large size then SVG-shrunk so the curves
  // stay smooth at 15px. The previous 16x16 path was a pixel-fitted
  // diamond-y mess that read as a blob at button height.
  return (
    <svg
      viewBox="0 0 24 24"
      width={16}
      height={16}
      aria-hidden
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ marginRight: 6 }}
    >
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}


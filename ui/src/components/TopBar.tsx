import { useStore } from "../lib/store";

/**
 * Application-wide top bar — matches the original Keel mockup.
 *
 * Left:   brand mark + "Session Manager". The mark used to live in the
 *         sidebar; moving it here lets the sidebar lead with a "FLEET"
 *         section label like the design.
 * Right:  "Awake" indicator — shown whenever the supervisor is holding
 *         a `caffeinate` sleep-inhibitor token. Single source of truth
 *         for the global keep-awake state (we removed the per-session
 *         toggle from the form because either the machine is awake or
 *         it isn't).
 *
 * The bar is `-webkit-app-region: drag` so the user can grab any empty
 * space to move the window — matches macOS Mail / Notes conventions.
 * Buttons + indicators opt out via `no-drag` so clicks still work.
 */
export function TopBar() {
  const keepAwake = useStore((s) => s.runtime.keep_awake_active);
  return (
    <div className="app-top" role="banner">
      <div className="app-top-left">
        <BrandMark />
        <span className="app-top-title">Session Manager</span>
      </div>
      <div className="app-top-right">
        {keepAwake && (
          <span
            className="awake-badge no-drag"
            title="caffeinate is active: machine will not sleep while any session is running."
          >
            <span className="awake-dot" /> Awake
          </span>
        )}
      </div>
    </div>
  );
}

function BrandMark() {
  // Three-row bullet list: small dot left, bar right. Reads as
  // "fleet of sessions" — the diamond it replaced didn't carry any
  // semantic load. Matches the rasterised tray + app-bundle icons.
  return (
    <div className="app-top-mark" aria-hidden>
      <svg viewBox="0 0 22 22" width={14} height={14}>
        <circle cx="5" cy="6"  r="2" fill="currentColor" />
        <rect x="9" y="5"  width="11" height="2" rx="1" fill="currentColor" />
        <circle cx="5" cy="11" r="2" fill="currentColor" />
        <rect x="9" y="10" width="11" height="2" rx="1" fill="currentColor" />
        <circle cx="5" cy="16" r="2" fill="currentColor" />
        <rect x="9" y="15" width="11" height="2" rx="1" fill="currentColor" />
      </svg>
    </div>
  );
}

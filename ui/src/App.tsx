import { useEffect, useState } from "react";
import { check } from "@tauri-apps/plugin-updater";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { Sidebar } from "./components/Sidebar";
import { Toasts } from "./components/Toasts";
import { TopBar } from "./components/TopBar";
import { go, useRoute } from "./lib/router";
import { useStore } from "./lib/store";
import { CreateSession } from "./views/CreateSession";
import { Dashboard } from "./views/Dashboard";
import { Onboarding } from "./views/Onboarding";
import { Popover } from "./views/Popover";
import { SessionDetail } from "./views/SessionDetail";
import { Settings } from "./views/Settings";

/**
 * Top-level router. Picks between the popover and the main app shell
 * based on the running Tauri window's label. This replaces the earlier
 * URL-hash routing (`index.html#/popover`) which didn't reliably load
 * in macOS release builds — the popover window came up blank and the
 * tray click looked like a no-op.
 *
 * Window label is read synchronously on first render via Tauri's
 * `getCurrentWindow().label`. If we're outside Tauri (vite dev preview
 * or a stubbed playwright run), the label call throws and we treat it
 * as "main".
 */
export function App() {
  const [label] = useState<string>(() => {
    try {
      return getCurrentWindow().label;
    } catch {
      return "main";
    }
  });
  return label === "popover" ? <AppPopover /> : <AppMain />;
}

function AppPopover() {
  const bootstrap = useStore((s) => s.bootstrap);
  const ready = useStore((s) => s.ready);
  useEffect(() => { void bootstrap(); }, [bootstrap]);
  if (!ready) {
    return (
      <div style={{ display: "grid", placeItems: "center", height: "100vh", color: "var(--text-muted)" }}>
        Loading…
      </div>
    );
  }
  return (
    <>
      <Popover />
      <Toasts />
    </>
  );
}

function AppMain() {
  const bootstrap = useStore((s) => s.bootstrap);
  const ready = useStore((s) => s.ready);
  const sessions = useStore((s) => s.sessions);
  const externalCount = useStore((s) => s.external.length);
  const pushToast = useStore((s) => s.pushToast);
  const route = useRoute();

  useEffect(() => { void bootstrap(); }, [bootstrap]);

  // Silent update check at startup. We deliberately don't download here;
  // the user clicks "Restart to install" inside Settings → About after
  // navigating there from the toast. Throttled to once per hour via
  // sessionStorage so opening + closing the window doesn't hammer the
  // GitHub API. Failures are swallowed (offline, rate-limited, etc.).
  useEffect(() => {
    if (!ready) return;
    const KEY = "sm.lastUpdateCheck";
    const last = Number(window.sessionStorage.getItem(KEY) ?? "0");
    if (Date.now() - last < 60 * 60 * 1000) return;
    window.sessionStorage.setItem(KEY, String(Date.now()));
    const t = window.setTimeout(() => {
      check()
        .then((update) => {
          if (!update) return;
          pushToast({
            title: `Update available: v${update.version}`,
            body: "Open Settings to download and install.",
            tone: "info",
            action: { label: "Open Settings", run: () => go("/settings") },
          });
        })
        .catch(() => {});
    }, 4000);
    return () => window.clearTimeout(t);
  }, [ready, pushToast]);

  // Per-route window title — helps with macOS app switcher + recents.
  useEffect(() => {
    const base = "Session Manager";
    const sub =
      route.name === "new" ? "New session" :
      route.name === "edit" ? `Edit ${route.id}` :
      route.name === "session" ? `Session — ${route.id}` :
      route.name === "settings" ? "Settings" :
      null;
    document.title = sub ? `${sub} — ${base}` : base;
  }, [route]);

  if (!ready) {
    return (
      <div style={{ display: "grid", placeItems: "center", height: "100vh", color: "var(--text-muted)" }}>
        Loading…
      </div>
    );
  }

  // Onboarding only when there is genuinely nothing — neither a managed
  // session nor an externally-running one. Otherwise we want the user to
  // land on the Dashboard so they can see and Manage what's already there.
  const showOnboarding =
    sessions.length === 0 && externalCount === 0 && route.name === "dashboard";

  return (
    <div className="app">
      <TopBar />
      <Sidebar />
      <main className="main">
        {showOnboarding ? (
          <Onboarding />
        ) : route.name === "session" ? (
          <SessionDetail id={route.id} />
        ) : route.name === "new" ? (
          <CreateSession key={route.query?.toString() ?? "new"} query={route.query} />
        ) : route.name === "edit" ? (
          <CreateSession editingId={route.id} />
        ) : route.name === "settings" ? (
          <Settings />
        ) : (
          <Dashboard />
        )}
      </main>
      <Toasts />
    </div>
  );
}

import { useEffect } from "react";

import { Sidebar } from "./components/Sidebar";
import { Toasts } from "./components/Toasts";
import { TopBar } from "./components/TopBar";
import { useRoute } from "./lib/router";
import { useStore } from "./lib/store";
import { CreateSession } from "./views/CreateSession";
import { Dashboard } from "./views/Dashboard";
import { Onboarding } from "./views/Onboarding";
import { SessionDetail } from "./views/SessionDetail";
import { Settings } from "./views/Settings";

export function App() {
  const bootstrap = useStore((s) => s.bootstrap);
  const ready = useStore((s) => s.ready);
  const sessions = useStore((s) => s.sessions);
  const externalCount = useStore((s) => s.external.length);
  const route = useRoute();

  useEffect(() => { void bootstrap(); }, [bootstrap]);

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
          // Key by the query string so navigating Adopt → different Adopt
          // remounts the form with fresh defaults instead of silently
          // overwriting the user's in-progress edits.
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

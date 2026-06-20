import { useEffect, useState } from "react";

export type Route =
  | { name: "dashboard"; query?: URLSearchParams }
  | { name: "new"; query?: URLSearchParams }
  | { name: "edit"; id: string }
  | { name: "session"; id: string }
  | { name: "settings" }
  | { name: "popover" };

function parse(hash: string): Route {
  const path = hash.replace(/^#/, "") || "/";
  const [base, qs] = path.split("?", 2);
  const query = qs ? new URLSearchParams(qs) : undefined;
  // Frameless tray popover. Loaded by the "popover" window (declared in
  // tauri.conf.json) with `url: "index.html#/popover"`. Same bundle, same
  // store — just a different React view.
  if (base === "/popover") return { name: "popover" };
  if (base === "/new") return { name: "new", query };
  if (base === "/settings") return { name: "settings" };
  if (base.startsWith("/session/")) {
    return { name: "session", id: decodeURIComponent(base.slice("/session/".length)) };
  }
  if (base.startsWith("/edit/")) {
    return { name: "edit", id: decodeURIComponent(base.slice("/edit/".length)) };
  }
  // Carry the query through to the Dashboard so the sidebar's per-group
  // entries (`/?group=Work`) can drive a filter instead of being a no-op.
  return { name: "dashboard", query };
}

export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parse(window.location.hash));
  useEffect(() => {
    const onHash = () => setRoute(parse(window.location.hash));
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  return route;
}

export function go(path: string) {
  window.location.hash = path;
}

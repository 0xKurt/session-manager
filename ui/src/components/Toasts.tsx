import { useEffect } from "react";

import { useStore } from "../lib/store";

export function Toasts() {
  const toasts = useStore((s) => s.toasts);
  const dismiss = useStore((s) => s.dismissToast);

  // Esc dismisses the topmost toast (LIFO — most recent goes first).
  useEffect(() => {
    if (toasts.length === 0) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        const top = toasts[toasts.length - 1];
        if (top) dismiss(top.id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toasts, dismiss]);

  if (toasts.length === 0) return null;
  return (
    <div className="toast-rail" role="status" aria-live="polite">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`toast ${t.tone}`}
        >
          <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div className="title">{t.title}</div>
              {t.body && <div className="body">{t.body}</div>}
            </div>
            {t.action && (
              <button
                className="btn"
                onClick={(e) => { e.stopPropagation(); t.action!.run(); dismiss(t.id); }}
              >
                {t.action.label}
              </button>
            )}
            <button
              className="btn ghost"
              style={{ padding: "4px 8px" }}
              aria-label="dismiss"
              onClick={(e) => { e.stopPropagation(); dismiss(t.id); }}
            >
              ✕
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}

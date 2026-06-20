import type { SessionStatus } from "../types";

export function StatusDot({ status, label = true }: { status: SessionStatus; label?: boolean }) {
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }} aria-label={status}>
      <span className={`dot ${status}`} aria-hidden />
      {label && <span className="status-label">{status.replace("-", " ")}</span>}
    </span>
  );
}

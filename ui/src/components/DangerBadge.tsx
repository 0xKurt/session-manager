/**
 * Renders the "this session skips permission prompts" indicator.
 * Intentionally neutral (amber, not red): the prior red `!` design read as
 * an error/problem, when in fact it's just a configured setting. Amber +
 * a shield-with-slash glyph reads as "deliberate looser-than-default
 * mode" without screaming "broken".
 */
export function DangerBadge({ compact = false }: { compact?: boolean }) {
  return (
    <span
      className="danger-badge"
      title="Skip-permissions enabled — this session can act without asking."
      aria-label="skip-permissions enabled"
    >
      <svg viewBox="0 0 16 16" width={12} height={12} aria-hidden>
        <path
          d="M8 1.5 L13 3.5 L13 8 C13 11 10.8 13.2 8 14.5 C5.2 13.2 3 11 3 8 L3 3.5 Z"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.4"
          strokeLinejoin="round"
        />
        <path
          d="M4 4 L12 12"
          stroke="currentColor"
          strokeWidth="1.4"
          strokeLinecap="round"
        />
      </svg>
      {!compact && "skip perms"}
    </span>
  );
}

import { go } from "../lib/router";
import { useStore } from "../lib/store";
import type { AuthState, BackendInfo } from "../types";

export function Onboarding() {
  const registry = useStore((s) => s.registry);
  const authStates = useStore((s) => s.authStates);

  return (
    <>
      <header className="top">
        <div>
          <div className="title">Welcome</div>
          <div className="sub">Get your first agent session managed.</div>
        </div>
      </header>
      <div className="content">
        <div className="empty-state">
          <h2>Welcome to Session Manager</h2>
          <p>
            Define an agent session once and the supervisor keeps it running
            across reboots and crashes — locally, no account, no relay.
          </p>
          <button className="btn primary cta" onClick={() => go("/new")}>Create your first session</button>
        </div>

        <section className="section" style={{ marginTop: 24 }}>
          <h3 className="section-title" style={{ marginBottom: 12 }}>Detected on this machine</h3>
          <div className="list">
            {registry.map((b) => (
              <BackendStatusRow key={b.id} backend={b} state={authStates[b.id]} />
            ))}
          </div>
        </section>
      </div>
    </>
  );
}

function BackendStatusRow({ backend, state }: { backend: BackendInfo; state: AuthState | undefined }) {
  const ready = state === "logged-in";
  const binaryMissing = state === "binary-missing";
  const label =
    state === "logged-in" ? "Ready" :
    state === "logged-out" ? "Installed — log in to use" :
    state === "binary-missing" ? "Not installed" :
    "Unknown";
  const hint =
    state === "logged-in" ? "Binary located via your login shell." :
    state === "logged-out" ? `Run \`${binaryName(backend.id)} login\` in a terminal, then come back.` :
    state === "binary-missing" ? `Install the ${backend.display_name} CLI and make sure it's on your shell PATH.` :
    "Couldn't determine state.";
  const tagClass = ready ? "tag accent" : binaryMissing ? "tag warn" : "tag";
  return (
    <div className="session-row static">
      <div>
        <div className="meta">
          <span className="name">{backend.display_name}</span>
          <span className={tagClass}>{label}</span>
        </div>
        <div className="subline">{hint}</div>
      </div>
    </div>
  );
}

function binaryName(backendId: string): string {
  if (backendId === "claude-code") return "claude";
  return backendId;
}

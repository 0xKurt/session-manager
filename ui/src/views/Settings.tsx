import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getVersion } from "@tauri-apps/api/app";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { useEffect, useState } from "react";

import { api } from "../lib/api";
import { go } from "../lib/router";
import { useStore } from "../lib/store";
import type { PermissionMode, ResumeMode } from "../types";

function OpenIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden>
      <path d="M3 3h7v1.5H4.5v7H11v-3h1.5v4.5h-9V3z" fill="currentColor" />
      <path d="M8 8 13 3M13 3h-3.5M13 3v3.5" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
function FinderIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden>
      <path d="M2 4.5C2 3.67 2.67 3 3.5 3h2.59c.4 0 .78.16 1.06.44L8.24 4.5H12.5c.83 0 1.5.67 1.5 1.5V12c0 .83-.67 1.5-1.5 1.5h-9C2.67 13.5 2 12.83 2 12V4.5z" stroke="currentColor" strokeWidth="1.25" fill="none" />
    </svg>
  );
}

export function Settings() {
  const preferences = useStore((s) => s.preferences);
  const setPrefs = useStore((s) => s.setPrefs);
  const setLaunchAtLogin = useStore((s) => s.setLaunchAtLogin);
  const configPath = useStore((s) => s.configPath);
  const registry = useStore((s) => s.registry);

  if (!preferences) return null;
  const defaults = preferences.defaults;

  return (
    <>
      <header className="top">
        <button className="btn ghost" onClick={() => go("/")}>← Sessions</button>
        <div>
          <div className="title">Settings</div>
          <div className="sub">Defaults, system integration, and config location.</div>
        </div>
      </header>
      <div className="content">
        <div style={{ maxWidth: 760, margin: "0 auto" }}>
        <section className="card padded section">
          <h3 className="section-title" style={{ marginBottom: 12 }}>System</h3>
          <ToggleRow
            label="Launch at login"
            description="Start the supervisor when you log in. Defined sessions with auto-restart come back automatically."
            checked={preferences.launch_at_login}
            onChange={(v) => void setLaunchAtLogin(v)}
          />
          <ToggleRow
            label="Native notifications"
            description="needs-permission / crashed / done. Always actionable where the OS allows."
            checked={preferences.notifications_enabled}
            onChange={(v) => void setPrefs({ notifications_enabled: v })}
          />
          <ToggleRow
            label="Keep machine awake while any session is running"
            description="Holds `caffeinate -d -i -s -m -u` for as long as at least one session is in a running state — display, idle, system and disk sleep all blocked while sessions are active. macOS limitation: on battery, closing the lid still sleeps the machine regardless (clamshell sleep is hardware-enforced; no userspace tool can override it without sudo + pmset changes). Use AC power if you need to keep sessions running with the lid closed."
            checked={preferences.keep_awake_master}
            onChange={(v) => void setPrefs({ keep_awake_master: v })}
          />
        </section>

        <section className="card padded section">
          <div className="row-spread" style={{ marginBottom: 12 }}>
            <h3 className="section-title">Defaults for new sessions</h3>
            <button
              className="btn"
              title="Set defaults that match a power-user pattern: danger permission, remote on, auto-restart on, keep-awake on."
              onClick={() => {
                if (!confirm("Apply power-user defaults?\n\n• permission = danger (skip prompts)\n• remote = on\n• auto-restart = on\n• keep-awake = on\n\nNew sessions will be created with these defaults.")) return;
                void setPrefs({
                  defaults: {
                    ...defaults,
                    permission: "danger",
                    remote: true,
                    auto_restart: true,
                    keep_awake: true,
                  },
                  power_user_prompt_dismissed: true,
                });
              }}
            >
              Power-user preset
            </button>
          </div>
          <div className="form-row">
            <div className="field">
              <label>Agent</label>
              <select
                value={defaults.agent}
                onChange={(e) => void setPrefs({ defaults: { ...defaults, agent: e.target.value } })}
              >
                {registry.map((b) => <option key={b.id} value={b.id}>{b.display_name}</option>)}
              </select>
            </div>
            <div className="field">
              <label>Permission mode</label>
              <select
                value={defaults.permission}
                onChange={(e) => void setPrefs({ defaults: { ...defaults, permission: e.target.value as PermissionMode } })}
              >
                <option value="safe">Safe</option>
                <option value="ask">Ask</option>
                <option value="danger">Skip permissions</option>
              </select>
            </div>
            <div className="field">
              <label>Resume mode</label>
              <select
                value={defaults.resume}
                onChange={(e) => void setPrefs({ defaults: { ...defaults, resume: e.target.value as ResumeMode } })}
              >
                <option value="continue">Continue</option>
                <option value="fresh">Fresh</option>
                <option value="resume">Resume by ID</option>
              </select>
            </div>
            <div className="field">
              <label>Model</label>
              <input
                value={defaults.model}
                onChange={(e) => void setPrefs({ defaults: { ...defaults, model: e.target.value } })}
              />
            </div>
            <div className="field full">
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={defaults.remote}
                  onChange={(e) => void setPrefs({ defaults: { ...defaults, remote: e.target.checked } })}
                />
                <span className="track" />
                <span>Remote control on by default</span>
              </label>
              <span className="hint">New sessions enable native remote control unless you turn it off.</span>
            </div>
          </div>
        </section>

        <section className="card padded">
          <h3 className="section-title" style={{ marginBottom: 12 }}>Config file</h3>
          <p className="muted" style={{ marginTop: 0, marginBottom: 12 }}>
            Your fleet is a single human-readable TOML file. Edit it by hand and Session Manager reconciles on change.
          </p>
          <div className="config-path-row">
            <span className="mono">{configPath}</span>
            <div className="hstack gap-1">
              <button
                className="btn icon"
                title="Open in default editor"
                aria-label="Open in default editor"
                onClick={() => configPath && api.openInOs(configPath).catch(console.error)}
              >
                <OpenIcon />
              </button>
              <button
                className="btn icon"
                title="Reveal in Finder"
                aria-label="Reveal in Finder"
                onClick={() => configPath && api.revealPath(configPath).catch(console.error)}
              >
                <FinderIcon />
              </button>
            </div>
          </div>
          <div className="stack gap-2" style={{ marginTop: 16 }}>
            <div className="toggle-row-label">Import / export</div>
            <div className="toggle-row-desc">Share a fleet setup across machines. Import replaces the running fleet.</div>
            <div className="hstack gap-2" style={{ marginTop: 4 }}>
              <button
                className="btn"
                onClick={async () => {
                  const path = await saveDialog({
                    title: "Export sessions.toml",
                    defaultPath: "sessions.toml",
                    filters: [{ name: "TOML", extensions: ["toml"] }],
                  });
                  if (typeof path === "string") {
                    await api.exportConfig(path);
                  }
                }}
              >
                Export…
              </button>
              <button
                className="btn"
                onClick={async () => {
                  const picked = await openDialog({
                    title: "Import sessions.toml",
                    multiple: false,
                    filters: [{ name: "TOML", extensions: ["toml"] }],
                  });
                  if (typeof picked === "string") {
                    if (!confirm("Import will stop running sessions and replace your fleet. Continue?")) return;
                    try {
                      await api.importConfig(picked);
                      await useStore.getState().refresh();
                    } catch (err) {
                      alert(`Import failed: ${err}`);
                    }
                  }
                }}
              >
                Import…
              </button>
            </div>
          </div>
        </section>

        <UpdatesSection />

        </div>
      </div>
    </>
  );
}

/**
 * About + update self-check. Pulls the running app version via Tauri's
 * `getVersion()` and queries `latest.json` on demand. Auto-updates require
 * the Ed25519 pubkey embedded in tauri.conf.json to match the private
 * key used to sign the bundle; mismatch → silent failure with the
 * `verification failed` error surfaced to the user.
 */
function UpdatesSection() {
  const [version, setVersion] = useState<string>("…");
  const [status, setStatus] = useState<
    | { kind: "idle" }
    | { kind: "checking" }
    | { kind: "up-to-date" }
    | { kind: "available"; version: string; notes?: string | null }
    | { kind: "downloading"; pct: number }
    | { kind: "ready" }
    | { kind: "error"; message: string }
  >({ kind: "idle" });

  useEffect(() => {
    getVersion().then(setVersion).catch(() => setVersion("?"));
  }, []);

  const onCheck = async () => {
    setStatus({ kind: "checking" });
    try {
      const update = await check();
      if (!update) {
        setStatus({ kind: "up-to-date" });
        return;
      }
      setStatus({ kind: "available", version: update.version, notes: update.body });
      // Begin the download immediately — the user already accepted the
      // intent by clicking Check. They confirm the actual restart below.
      let downloaded = 0;
      let total = 0;
      await update.downloadAndInstall((ev) => {
        if (ev.event === "Started") {
          total = ev.data.contentLength ?? 0;
        } else if (ev.event === "Progress") {
          downloaded += ev.data.chunkLength;
          const pct = total > 0 ? Math.round((downloaded / total) * 100) : 0;
          setStatus({ kind: "downloading", pct });
        } else if (ev.event === "Finished") {
          setStatus({ kind: "ready" });
        }
      });
    } catch (e) {
      setStatus({ kind: "error", message: String(e) });
    }
  };

  return (
    <section className="card padded section">
      <div className="row-spread" style={{ marginBottom: 12 }}>
        <h3 className="section-title">About</h3>
      </div>
      <dl className="kv">
        <dt>Version</dt>
        <dd className="mono">{version}</dd>
        <dt>Updates</dt>
        <dd>
          <div className="hstack gap-2" style={{ alignItems: "center" }}>
            <button
              className="btn"
              onClick={() => void onCheck()}
              disabled={
                status.kind === "checking" || status.kind === "downloading"
              }
            >
              {status.kind === "checking"
                ? "Checking…"
                : status.kind === "downloading"
                ? `Downloading ${status.pct}%`
                : "Check for updates"}
            </button>
            {status.kind === "ready" && (
              <button className="btn primary" onClick={() => void relaunch()}>
                Restart to install
              </button>
            )}
            {status.kind === "up-to-date" && <span className="muted">You're on the latest.</span>}
            {status.kind === "available" && (
              <span className="muted">v{status.version} ready.</span>
            )}
            {status.kind === "error" && (
              <span style={{ color: "var(--danger)" }}>{status.message}</span>
            )}
          </div>
        </dd>
      </dl>
    </section>
  );
}

function ToggleRow({
  label,
  description,
  checked,
  onChange,
}: {
  label: string;
  description?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="toggle-row">
      <div>
        <div className="toggle-row-label">{label}</div>
        {description && <div className="toggle-row-desc">{description}</div>}
      </div>
      <label className="toggle">
        <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
        <span className="track" />
      </label>
    </div>
  );
}

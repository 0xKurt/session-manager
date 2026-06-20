import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useState } from "react";

import { DangerBadge } from "../components/DangerBadge";
import { api } from "../lib/api";
import { go } from "../lib/router";
import { useStore } from "../lib/store";
import type { PermissionMode, ResumeMode, SessionConfig } from "../types";

const PERMISSION_OPTIONS: { value: PermissionMode; label: string }[] = [
  { value: "safe", label: "Safe (always asks)" },
  { value: "ask", label: "Ask before tools" },
  { value: "danger", label: "Skip permissions" },
];

const RESUME_OPTIONS: { value: ResumeMode; label: string; help: string }[] = [
  {
    value: "continue",
    label: "Continue most recent",
    help: "Picks up the latest conversation for this folder. Multiple sessions in the same folder share this stack — they'll fight over the most-recent slot.",
  },
  {
    value: "resume",
    label: "Resume specific session…",
    help: "Pin to one conversation by its UUID. Useful when you have several sessions in the same folder.",
  },
  {
    value: "fresh",
    label: "Start fresh",
    help: "Ignore prior conversations and begin a new one each start.",
  },
];

const EXPERIMENTAL_BACKENDS = new Set(["codex"]);

export function CreateSession({ editingId, query }: { editingId?: string; query?: URLSearchParams } = {}) {
  const sessions = useStore((s) => s.sessions);
  const defaults = useStore((s) => s.preferences?.defaults);
  const registry = useStore((s) => s.registry);
  const create = useStore((s) => s.create);
  const update = useStore((s) => s.update);

  const existing = editingId ? sessions.find((s) => s.id === editingId) : undefined;

  // Initial form values — computed ONCE on mount via lazy state init.
  // We deliberately do NOT re-sync from `existing` / `defaults` / `query`
  // after mount: every probe tick replaces the sessions array reference,
  // which would otherwise fire a `setForm(initial)` and wipe the user's
  // in-progress edits. App.tsx already remounts CreateSession by `key`
  // when the target session id or query changes, so a fresh form for a
  // genuinely-different target is still handled — the remount IS the
  // sync mechanism.
  const [form, setForm] = useState<SessionConfig>(() => {
    if (existing) return existing;
    const b = blank(defaults);
    if (query) {
      return {
        ...b,
        name: query.get("name") ?? "",
        id: query.get("id") ?? "",
        path: query.get("path") ?? "",
        agent: query.get("agent") ?? b.agent,
      };
    }
    return b;
  });
  // `initial` is a frozen snapshot of mount-time defaults, used by isDirty
  // to detect whether the user actually changed anything. NOT a reactive
  // value — see the lazy initialiser above.
  const initial = useMemo<SessionConfig>(() => form, []); // eslint-disable-line react-hooks/exhaustive-deps
  const [dangerConfirm, setDangerConfirm] = useState(false);
  const [errors, setErrors] = useState<{ name?: string; id?: string; path?: string; submit?: string }>({});
  const [submitting, setSubmitting] = useState(false);

  // Esc to cancel (matches macOS sheet conventions). The handler uses
  // refs to the latest form/initial so we don't reattach on every keystroke.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        const target = e.target as HTMLElement | null;
        // Don't steal Escape if a control or text input is focused — it has
        // its own conventions (clear search, close dropdown, etc.).
        if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT")) {
          return;
        }
        e.preventDefault();
        if (isDirty(form, initial)) {
          if (!confirm("Discard your changes?")) return;
        }
        go("/");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [form, initial]);

  const onPickFolder = async () => {
    try {
      const picked = await open({ directory: true, multiple: false });
      if (typeof picked === "string") setForm((f) => ({ ...f, path: picked }));
    } catch (err) { console.error(err); }
  };

  const wasDanger = existing?.permission === "danger";
  const switchedToDanger = form.permission === "danger" && !wasDanger;

  const validate = async (target: SessionConfig): Promise<{ name?: string; id?: string; path?: string } | null> => {
    const e: { name?: string; id?: string; path?: string } = {};
    if (!target.name.trim()) e.name = "Required.";
    if (!target.id.trim()) e.id = "Required.";
    else if (!/^[a-z0-9][a-z0-9-]*$/.test(target.id)) e.id = "Lowercase letters, digits, dashes.";
    if (!editingId && sessions.some((s) => s.id === target.id)) e.id = `"${target.id}" is taken.`;
    if (!target.path.trim()) e.path = "Required.";
    else {
      try {
        const kind = await api.pathKind(target.path);
        if (kind === "missing") e.path = "Folder doesn't exist.";
        else if (kind === "file") e.path = "That's a file. Pick a folder.";
        else if (kind === "other") e.path = "Not a regular folder.";
      } catch (err) {
        console.error(err);
      }
    }
    return Object.keys(e).length ? e : null;
  };

  const onSubmit = async (start: boolean) => {
    const target: SessionConfig = {
      ...form,
      id: (form.id || slug(form.name)).trim(),
      name: form.name.trim() || form.id.trim(),
    };
    setErrors({});
    const v = await validate(target);
    if (v) {
      setErrors(v);
      return;
    }
    if (switchedToDanger && !dangerConfirm) {
      setDangerConfirm(true);
      return;
    }
    setSubmitting(true);
    try {
      if (existing) {
        await update(target);
      } else {
        await create(target);
      }
      if (start) {
        await useStore.getState().start(target.id);
      }
      go(`/session/${encodeURIComponent(target.id)}`);
    } catch (err) {
      setErrors({ submit: String(err) });
    } finally {
      setSubmitting(false);
      setDangerConfirm(false);
    }
  };

  const argv = useMemo(() => buildArgvPreview(form), [form]);

  return (
    <>
      <header className="top">
        <button className="btn ghost" onClick={() => go("/")}>← Sessions</button>
        <div>
          <div className="title">{editingId ? `Edit ${initial.name || initial.id}` : "New session"}</div>
          <div className="sub">{editingId ? "Update configuration" : "Define a session — the supervisor takes it from there."}</div>
        </div>
      </header>
      <div className="content">
        <form
          className="card padded form"
          style={{ maxWidth: 720, margin: "0 auto" }}
          onSubmit={(e) => { e.preventDefault(); void onSubmit(true); }}
        >

          {/* ── Basics ───────────────────────────────────────────────── */}
          <section className="form-section">
            <div className="form-section-head">
              <h4>Basics</h4>
              <span className="note">Identity + where it runs.</span>
            </div>

            <div className="form-row thirds">
              <div className="field">
                <label>Name <span className="req">*</span></label>
                <input
                  value={form.name}
                  onChange={(e) => setForm((f) => ({ ...f, name: e.target.value, id: editingId ? f.id : (f.id || slug(e.target.value)) }))}
                  placeholder="My project"
                  aria-invalid={!!errors.name}
                />
                {errors.name && <span className="hint" style={{ color: "var(--danger)" }}>{errors.name}</span>}
              </div>
              <div className="field">
                <label>
                  ID <span className="req">*</span>
                  <span className="help">slug</span>
                </label>
                <input
                  value={form.id}
                  onChange={(e) => setForm((f) => ({ ...f, id: e.target.value }))}
                  placeholder="my-project"
                  disabled={!!editingId}
                  className="mono"
                  aria-invalid={!!errors.id}
                />
                {errors.id && <span className="hint" style={{ color: "var(--danger)" }}>{errors.id}</span>}
              </div>
            </div>

            <div className="field full">
              <label>Working directory <span className="req">*</span></label>
              <div className="field-inline">
                <input
                  value={form.path}
                  onChange={(e) => setForm((f) => ({ ...f, path: e.target.value }))}
                  placeholder="~"
                  className="mono"
                  aria-invalid={!!errors.path}
                />
                <button type="button" className="btn" onClick={onPickFolder}>Pick folder</button>
              </div>
              {errors.path
                ? <span className="hint" style={{ color: "var(--danger)" }}>{errors.path}</span>
                : <span className="hint">~ and $VARS expand at launch.</span>}
            </div>

            <div className="form-row">
              <div className="field">
                <label>Agent</label>
                <select value={form.agent} onChange={(e) => setForm((f) => ({ ...f, agent: e.target.value }))}>
                  {registry.map((b) => (
                    <option key={b.id} value={b.id}>
                      {b.display_name}{EXPERIMENTAL_BACKENDS.has(b.id) ? " — experimental" : ""}
                    </option>
                  ))}
                </select>
              </div>
              <div className="field">
                <label>Permission mode</label>
                <select
                  value={form.permission}
                  onChange={(e) => setForm((f) => ({ ...f, permission: e.target.value as PermissionMode }))}
                >
                  {PERMISSION_OPTIONS.map((opt) => (
                    <option key={opt.value} value={opt.value}>{opt.label}</option>
                  ))}
                </select>
                {form.permission === "danger" && (
                  <span className="hint" style={{ color: "var(--danger)" }}>
                    Runs without asking before file edits or shell commands.
                  </span>
                )}
              </div>
            </div>
          </section>

          {/* ── Behavior ────────────────────────────────────────────── */}
          <section className="form-section">
            <div className="form-section-head">
              <h4>Behavior</h4>
              <span className="note">How the session starts + survives.</span>
            </div>

            <div className="form-row">
              <div className="field">
                <label>Resume mode</label>
                <select
                  value={form.resume}
                  onChange={(e) => setForm((f) => ({ ...f, resume: e.target.value as ResumeMode }))}
                >
                  {RESUME_OPTIONS.map((opt) => (
                    <option key={opt.value} value={opt.value}>{opt.label}</option>
                  ))}
                </select>
                <span className="hint">
                  {RESUME_OPTIONS.find((o) => o.value === form.resume)?.help}
                </span>
                {form.resume === "resume" && (
                  <input
                    className="mono"
                    style={{ marginTop: 6 }}
                    value={form.resume_id}
                    placeholder="uuid (from ~/.claude/projects/-<cwd>/)"
                    onChange={(e) => setForm((f) => ({ ...f, resume_id: e.target.value }))}
                  />
                )}
                {form.resume === "continue" && cwdCollidesWithExisting(form, sessions, editingId) && (
                  <span className="hint" style={{ color: "var(--warn)" }}>
                    Another session uses this folder. <strong>Continue most recent</strong> is
                    non-deterministic across them — consider <strong>Resume specific session</strong>
                    {" "}or change the folder.
                  </span>
                )}
              </div>
              <div className="field">
                <label>Model</label>
                <input value={form.model} onChange={(e) => setForm((f) => ({ ...f, model: e.target.value }))} />
              </div>
            </div>

            <div className="form-row">
              <div className="field">
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={form.remote}
                    onChange={(e) => setForm((f) => ({ ...f, remote: e.target.checked }))}
                  />
                  <span className="track" />
                  <span>Remote control</span>
                </label>
                <span className="hint">Uses the agent's native remote feature. Not proxied.</span>
              </div>
              <div className="field">
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={form.auto_restart}
                    onChange={(e) => setForm((f) => ({ ...f, auto_restart: e.target.checked }))}
                  />
                  <span className="track" />
                  <span>Auto restart on crash + at login</span>
                </label>
                <span className="hint">Required for reboot survival.</span>
              </div>
            </div>

            <div className="form-row">
              <div className="field">
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={form.record_stdout}
                    onChange={(e) => setForm((f) => ({ ...f, record_stdout: e.target.checked }))}
                  />
                  <span className="track" />
                  <span>Record stdout transcript</span>
                </label>
                <span className="hint">Wraps in <code className="mono">script(1)</code> with a PTY.</span>
              </div>
              <div className="field" />
            </div>

            <div className="form-row">
              <div className="field">
                <label>Restart attempts before giving up</label>
                <input
                  type="number"
                  min={1}
                  max={50}
                  value={form.restart_max}
                  onChange={(e) => setForm((f) => ({ ...f, restart_max: Math.max(1, parseInt(e.target.value || "1", 10)) }))}
                />
              </div>
              <div className="field">
                <label>Group <span className="help">optional</span></label>
                <input
                  value={form.group ?? ""}
                  onChange={(e) => setForm((f) => ({ ...f, group: e.target.value || null }))}
                  placeholder="e.g. work"
                />
              </div>
            </div>
          </section>

          {/* ── Advanced (collapsed by default) ──────────────────────── */}
          <details className="disclosure">
            <summary>Advanced</summary>
            <div className="disclosure-body">
              <div className="field full">
                <label>Extra command-line arguments</label>
                <input
                  value={form.extra_args.join(" ")}
                  onChange={(e) => setForm((f) => ({ ...f, extra_args: splitShellArgs(e.target.value) }))}
                  placeholder={'--add-dir /path/to/other-repo --allowed-tools "Bash(git *)"'}
                  className="mono"
                />
                <span className="hint">Appended verbatim. Quoted strings stay whole.</span>
              </div>
              <div className="field full">
                <label>Environment variables <span className="help">KEY=value, one per line</span></label>
                <textarea
                  rows={4}
                  value={Object.entries(form.env).map(([k, v]) => `${k}=${v}`).join("\n")}
                  onChange={(e) => setForm((f) => ({ ...f, env: parseEnv(e.target.value) }))}
                  placeholder="OPENAI_API_KEY=sk-…&#10;FOO=bar"
                  className="mono"
                />
                <span className="hint">Merged onto the safe shell env. Stored in <code className="mono">sessions.toml</code>.</span>
              </div>
            </div>
          </details>

          {/* ── Live argv preview ────────────────────────────────────── */}
          <div className="argv-preview" title="The supervisor will exec this when you start the session.">
            <span className="head">Will exec</span>
            {argv}
          </div>

          {dangerConfirm && (
            <div className="danger-banner" style={{ flexWrap: "wrap" }}>
              <DangerBadge compact />
              <span>
                {existing
                  ? `"${existing.name}" will run without asking permission. Continue?`
                  : "This session will run without asking permission. Continue?"}
              </span>
              <button
                className="btn primary"
                disabled={submitting}
                onClick={() => { setDangerConfirm(false); void onSubmit(true); }}
              >
                Yes, {existing ? "save" : "create"}
              </button>
              <button className="btn ghost" onClick={() => setDangerConfirm(false)}>Cancel</button>
            </div>
          )}

          {errors.submit && (
            <div className="danger-banner">
              <span>{errors.submit}</span>
            </div>
          )}
          {/* Invisible submit target so <form onSubmit> picks up Enter. */}
          <button type="submit" style={{ display: "none" }} aria-hidden tabIndex={-1} />
        </form>
      </div>
      {/* Footer is a sibling of .content so it stays visible while the
          form scrolls. The inner row matches the card max-width so the
          buttons sit under the form they submit. */}
      <div className="form-footer">
        <div className="form-footer-inner">
          <button className="btn ghost" onClick={onCancelClick}>Cancel</button>
          <span className="spacer" />
          <button className="btn" disabled={submitting} onClick={() => onSubmit(false)}>
            Save for later
          </button>
          <button className="btn primary" disabled={submitting} onClick={() => onSubmit(true)}>
            {submitting && <span className="spinner" />}
            {editingId ? "Save & restart" : "Create & start"}
          </button>
        </div>
      </div>
    </>
  );

  function onCancelClick() {
    if (isDirty(form, initial)) {
      if (!confirm("Discard your changes?")) return;
    }
    go("/");
  }
}

/** True when the user has typed anything that differs from the initial form. */
function isDirty(cur: SessionConfig, base: SessionConfig): boolean {
  return JSON.stringify(cur) !== JSON.stringify(base);
}

function blank(defaults?: { agent: string; permission: PermissionMode; remote: boolean; resume: ResumeMode; model: string; keep_awake: boolean; auto_restart: boolean }): SessionConfig {
  return {
    id: "",
    name: "",
    agent: defaults?.agent ?? "claude-code",
    path: "",
    remote: defaults?.remote ?? true,
    permission: defaults?.permission ?? "ask",
    resume: defaults?.resume ?? "continue",
    resume_id: "",
    model: defaults?.model ?? "default",
    keep_awake: defaults?.keep_awake ?? false,
    auto_restart: defaults?.auto_restart ?? true,
    restart_max: 5,
    env: {},
    log_path: null,
    group: null,
    record_stdout: false,
    extra_args: [],
  };
}

function slug(s: string) {
  return s.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
}

/** True when another (non-this) session is already configured for the same path. */
function cwdCollidesWithExisting(
  cur: SessionConfig,
  all: SessionConfig[],
  editingId?: string,
): boolean {
  const norm = (s: string) => s.trim().replace(/\/+$/, "");
  const p = norm(cur.path);
  if (!p) return false;
  return all.some((s) => s.id !== editingId && s.id !== cur.id && norm(s.path) === p);
}

function splitShellArgs(raw: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quote: '"' | "'" | null = null;
  for (let i = 0; i < raw.length; i++) {
    const ch = raw[i];
    if (quote) {
      if (ch === quote) { quote = null; }
      else { cur += ch; }
    } else if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === " " || ch === "\t") {
      if (cur.length) { out.push(cur); cur = ""; }
    } else {
      cur += ch;
    }
  }
  if (cur.length) out.push(cur);
  return out;
}

function parseEnv(raw: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of raw.split(/\n+/)) {
    const t = line.trim();
    if (!t || t.startsWith("#")) continue;
    const eq = t.indexOf("=");
    if (eq <= 0) continue;
    const key = t.slice(0, eq).trim();
    const val = t.slice(eq + 1).trim();
    if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) {
      out[key] = val;
    }
  }
  return out;
}

/**
 * Mirrors what `ClaudeCodeBackend::build_launch` produces server-side. Lets
 * power users see exactly what argv we'll exec before they click Start.
 * Codex is structurally similar — we render it the same way; the wrapper
 * around script(1) is rendered when record_stdout is on.
 */
function buildArgvPreview(f: SessionConfig): string {
  const args: string[] = [];
  const program = f.agent === "claude-code" ? "claude" : "codex";
  if (f.remote) {
    args.push("--remote-control");
    args.push(quote(f.name || f.id || "session"));
  }
  if (f.agent === "claude-code") {
    if (f.permission === "safe") { args.push("--permission-mode"); args.push("safe"); }
    if (f.permission === "danger") args.push("--dangerously-skip-permissions");
    if (f.resume === "continue") args.push("--continue");
    if (f.resume === "resume" && f.resume_id) { args.push("--resume"); args.push(f.resume_id); }
  } else {
    if (f.permission === "safe") { args.push("--ask-for-approval"); args.push("untrusted"); }
    if (f.permission === "ask")  { args.push("--ask-for-approval"); args.push("on-request"); }
    if (f.permission === "danger") args.push("--dangerously-bypass-approvals-and-sandbox");
    if (f.remote) args.push("--remote");
    if (f.resume === "continue") args.push("--continue");
    if (f.resume === "resume" && f.resume_id) { args.push("--resume"); args.push(f.resume_id); }
  }
  if (f.model && f.model !== "default") { args.push("--model"); args.push(f.model); }
  for (const x of f.extra_args) if (x.trim()) args.push(x);

  const tokens = [program, ...args];
  if (f.record_stdout) {
    const recId = f.id || slug(f.name) || "session";
    return ["script", "-q", `~/Library/Caches/SessionManager/recordings/${recId}.log`, ...tokens].join(" ");
  }
  return tokens.join(" ");
}

function quote(s: string): string {
  if (/^[A-Za-z0-9._/-]+$/.test(s)) return s;
  return `"${s.replace(/"/g, '\\"')}"`;
}

# Remote Agent Manager — Product & Technical Specification

**Working title:** *Helm* (placeholder — name TBD)
**Document status:** Draft for handoff
**Audience:** Product designer + application developer
**Last updated:** 2026-06-17

---

## 0. How to read this document

This spec is written for two readers:

- **Developer** — Sections 6, 7, 8, 10, 11, 12 are the build. Section 7 (Architecture) is the contract; everything else hangs off it.
- **Designer** — Sections 1–5 give you the *why* and the product principles. Section 9 (UX & Design) is yours, but read Section 8 (Feature spec) first because the states defined there are what you'll be designing screens for.

Where a decision is still open, it's flagged in **Section 13**.

---

## 1. Summary (TL;DR)

*Helm* is a small, local, cross-platform desktop app that manages the lifecycle of multiple AI coding-agent sessions (Claude Code and Codex) running on the user's own machine. It is the layer that starts, supervises, restarts, and surfaces those sessions — so a developer running six agents across six projects no longer babysits six terminal windows and never loses their setup to a reboot.

The defining design choice: **Helm is a supervisor, not a relay.** Remote access ("use my agent from my phone") is already solved by each agent's *native* remote feature — Claude Code Remote Control, Codex's remote handoff. Helm does not build its own cloud service, account, or relay. It launches every session in its agent's native remote mode by default and lets that native channel do the phone/anywhere part. The user's code never touches a third-party server, and there is no second login.

What Helm owns: process supervision (replacing ad-hoc `tmux`/`launchd` setups), declarative session definitions, restart-on-crash / restore-on-reboot, a tray + window UI to start/stop/create/monitor sessions, OS-level niceties like "keep the machine awake while agents are working," and a clear safety UX around permission-skipping sessions.

What Helm explicitly does **not** do: usage/cost analytics, token dashboards, a plugin marketplace, or any kind of cloud sync. Those are separate products.

---

## 2. Problem & motivation

Developers increasingly run several long-lived coding-agent sessions in parallel — one per project, each in its own terminal. This breaks down in predictable ways:

1. **Sessions are bound to terminal windows.** Close a window or lose the shell and the process gets `SIGHUP` and dies, taking any in-flight work with it.
2. **A reboot wipes everything.** Every session must be relaunched by hand, with the right flags, in the right directory, often by digging up session IDs.
3. **Remote access exists but isn't managed.** Native Remote Control lets you drive a session from your phone, but only while the local process is alive — and there's no single place to see, start, or restart the fleet.
4. **Existing tools miss the target.** Monitors (c9watch, Claude-God, etc.) only *watch* sessions; full orchestrators (Conductor, Omnara) either replace the user's workflow or route through their own account/relay.

There is no small, local, account-free tool whose job is simply **"keep my agents running and let me manage them."** That is Helm.

---

## 3. Product principles (non-negotiables)

1. **Supervisor, not a relay.** Helm manages process lifecycle. It never proxies agent traffic, never holds credentials beyond what's needed to launch, and never requires its own account.
2. **Delegate remote to native.** "Control from anywhere" = the agent's own remote feature, toggled on by Helm. We do not reinvent it.
3. **Local-first, zero telemetry.** No data leaves the machine. No analytics calls. Code, prompts, and transcripts stay local.
4. **Cross-platform from day one.** macOS, Windows, Linux. Platform-specific code is isolated to a thin OS layer.
5. **Stay small.** Every feature must serve "start / keep alive / restart / surface / control." If it's analytics, marketplace, or cloud, it's out.
6. **Survive the UI.** Sessions are owned by a background supervisor, not the window. Quitting the UI, closing the laptop, or rebooting must not silently kill work beyond what's physically unavoidable.

---

## 4. Users & use cases

**Primary user:** an individual developer running multiple agent sessions across several repos, who wants them persistent and reachable from a phone while away from the desk.

Representative use cases:

- *Fleet at login.* User reboots; Helm relaunches all defined sessions with `--continue`, each in its project directory, remote-on. Within seconds the phone shows all of them online.
- *Kick off and walk away.* User starts a long agentic run, closes the laptop UI, leaves. The supervisor keeps the process alive (and, if configured, keeps the machine awake); the user steers it from the Claude app on the train.
- *New project in 10 seconds.* User clicks "New session," picks/creates a folder, picks Claude Code, accepts the default profile (remote on, ask-permissions), hits start.
- *Panic.* A skip-permissions agent goes sideways; user hits "Stop all" from the tray.
- *Mixed agents.* Three Claude Code sessions and two Codex sessions, managed identically in one list.

---

## 5. Scope

### In scope (v1 ambition)
- Declarative, persistent session definitions
- Start / stop / restart / create / delete sessions
- Background supervisor: survives UI close, auto-restart on crash, restore on reboot
- Per-session permission mode with a prominent danger indicator
- Native remote control, **on by default**, per-session override
- Live per-session status (working / needs-permission / idle / done / crashed / offline / rate-limited)
- "Needs permission" surfacing + native notifications
- System integration: launch-at-login, keep-awake, sleep/wake + reconnect handling
- Agent backends: Claude Code and Codex (pluggable interface)
- Tray/menu-bar app + management window
- macOS, Windows, Linux
- Local CLI mirroring core actions (automation surface)

### Explicitly out of scope (and why)
- **Usage / cost / token analytics, burn-rate, ROI, heatmaps** — separate product category; dedicated tools already do this well. *Exception:* a single boolean "rate-limited" status flag, only so the user knows *why* an agent stalled.
- **Custom relay / cloud execution / cloud handoff** — violates "supervisor, not a relay." Remote is delegated to native features.
- **Accounts, sign-up, server-side anything.**
- **Plugin / extension marketplace.**
- **In-app code editing / diff review** — that's the agent's app and the user's IDE. Helm links out, it doesn't reimplement.

---

## 6. Core concepts & glossary

- **Session** — a single managed agent process, defined declaratively (agent + working directory + flags + behavior). The unit of everything.
- **Backend / agent** — the underlying CLI agent a session runs (Claude Code, Codex). Behind a common interface.
- **Supervisor** — the long-lived background process that owns and monitors all sessions. Separate from the UI.
- **Remote control** — the agent's *native* feature that bridges a local session to its mobile/web app. Helm toggles it; it does not implement it.
- **Permission mode** — how much the agent may do without asking: `safe`, `ask`, or `danger` (skip permissions).
- **Profile** — a reusable template of session defaults (agent + flags + env + behavior).

---

## 7. Architecture (developer)

### 7.1 High-level shape

```
┌──────────────────────────────────────────────────┐
│  UI (web front-end in Tauri webview)              │
│  - tray menu  - management window                 │
│  talks to core over Tauri IPC / local commands    │
└───────────────▲──────────────────────────────────┘
                │ IPC (commands + event stream)
┌───────────────┴──────────────────────────────────┐
│  Core (Rust)                                       │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────┐ │
│  │ Supervisor  │  │ Backend       │  │ OS layer  │ │
│  │ (lifecycle) │  │ registry      │  │ (per-OS)  │ │
│  └──────┬──────┘  └──────┬────────┘  └─────┬─────┘ │
│         │                │                  │       │
│  spawns/monitors   builds launch cmd   autostart,   │
│  child processes   per agent           keep-awake,  │
│                                        notifications│
└───────────────┬───────────────────────────────────┘
                │ spawns
        ┌───────┴────────┬───────────────┐
   claude (sess A)   claude (sess B)   codex (sess C)
   --remote-control  --remote-control  (native remote)
```

### 7.2 Process / supervision model (the crux)

- The **supervisor is a long-lived process, decoupled from the UI window.** Closing the window must not kill sessions. Two acceptable implementations — pick one in Section 13:
  - (a) Supervisor runs inside the Tauri app but the app keeps running headless in the tray when the window closes (tray app, no dock/taskbar requirement). Simpler; "quit" still kills sessions, but "close window" doesn't.
  - (b) Supervisor is a separate daemon/agent process the UI attaches to. Survives even a UI crash/quit. More robust, more plumbing.
  - **Recommended:** start with (a) for MVP, design the IPC boundary so (b) is a later swap.
- Each session is spawned as a **detached child process** with its own working directory and environment. The supervisor:
  - tracks PID + state, captures `stdout`/`stderr` to a per-session log file (this replaces the user's old `script` wrapper — Helm captures output itself).
  - detects exit and applies the session's restart policy (crash → restart with backoff; clean exit → mark "done", no restart).
  - on launch/login, reconciles the desired set (from config) against running processes and starts what's missing.
- **Reboot behavior:** the supervisor is registered to launch at login (per-OS, see 7.9). On start it relaunches each session whose `auto_restart` is true, using its `resume` policy (`--continue` / `--resume <id>`). Conversation state is restored by the agent; in-flight tasks are *not* (documented limitation — see 8.6).

### 7.3 Tech stack

- **Tauri** (Rust core + web UI). Rationale: native tray on all three OSes, small binary, Rust is a good fit for a process supervisor, and it's the only way to hit macOS + Windows + Linux without three codebases. (Electron is the fallback if a required capability is missing in Tauri, but default to Tauri.)
- **UI:** web stack of the team's choice (React/Svelte/Solid). No heavy framework lock-in required; the UI is thin — it renders state and sends commands.
- **State transport:** Tauri IPC commands (UI → core) + an event stream (core → UI) for live status updates. The UI holds no source-of-truth state; the core does.
- **Config storage:** a single human-readable file (TOML recommended) plus a small state cache. See 7.8.

### 7.4 Agent backend abstraction

A backend is an implementation of a common trait. Adding an agent = implementing this interface, nothing else touches it.

A backend must declare:
- `id` and display name (e.g. `claude-code`, `codex`)
- **binary resolution** — how to find the executable. Must handle version-manager installs: resolve via `$SHELL -l -c "which <bin>"` (macOS/Linux) and the shell-aware equivalent on Windows, *not* a bare `PATH` lookup. (Lifted from Claude-God's hard-won fix.)
- **launch command builder** — given a session config, produce argv + env + cwd. Encodes the agent's flags for: remote mode, permission mode, resume mode, model.
- **remote flag** — the native remote-control invocation (Claude Code: remote-control mode; Codex: its remote handoff).
- **status detection** — how to read the agent's live state (see 7.6).
- **auth/login check** — is the user logged in to this agent? (read-only; see 7.6.)

Example mapping (Claude Code, session = remote + danger + continue):
`claude --remote-control "<name>" --dangerously-skip-permissions --continue`, run with `cwd = <path>`.

### 7.5 Remote control delegation

- `remote: true` is the **default** for every session (Section 13 confirms; this is the product thesis).
- Setting it resolves, per backend, to that agent's native remote invocation. Helm's only jobs around remote:
  1. launch with the right flag,
  2. surface the resulting connect affordance (URL/QR the agent emits) in the UI,
  3. reflect remote connection state (online/offline) in session status,
  4. on reconnect/relaunch, re-establish it.
- Helm does **not** transport, store, or proxy any remote traffic. If the agent's remote feature requires a particular plan/version, that's the agent's concern; Helm surfaces a clear error if launch-with-remote fails and offers to launch local-only.

### 7.6 Status detection

- **Process state** from the supervisor (running / exited / crashed).
- **Agent activity state** by reading the agent's local session artifacts — for Claude Code, tail the project's `~/.claude/projects/**/*.jsonl`; map the last entries to: `working`, `needs-permission`, `idle`, `done`. (Same approach as c9watch / Claude-God.) Each backend supplies its own parser.
- **Rate-limited** flag: detected from the agent's local signals where available; surfaced only as a status reason, never as analytics.
- **Auth check:** read-only detection that the user is logged into the agent (e.g. presence/validity of local credentials). Helm never performs login flows itself beyond launching the agent's own `login` if the user asks.

### 7.7 Data model — session schema

```toml
# ~/.config/helm/sessions.toml  (path is per-OS, see 7.8)

[[session]]
id            = "trading-bot"        # stable key, slug
name          = "Trading Bot"        # display name
agent         = "claude-code"        # claude-code | codex
path          = "~/code/trading-bot" # working directory
remote        = true                 # default true
permission    = "danger"             # safe | ask | danger
resume        = "continue"           # continue | resume | fresh
resume_id     = ""                   # required iff resume = "resume"
model         = "default"
keep_awake    = true                 # keep machine awake while this session is working
auto_restart  = true                 # restart on crash + relaunch on reboot
restart_max   = 5                    # backoff cap
env           = { }                  # extra environment
log_path      = "~/.local/state/helm/logs/trading-bot.log"
group         = "work"               # optional tag for batch ops
```

Runtime state (PID, current status, last-seen, remote URL) lives in a separate cache, never hand-edited.

### 7.8 Configuration & storage paths

| Purpose | macOS | Windows | Linux |
|---|---|---|---|
| Config (`sessions.toml`) | `~/Library/Application Support/Helm/` | `%APPDATA%\Helm\` | `~/.config/helm/` |
| State cache | `~/Library/Caches/Helm/` | `%LOCALAPPDATA%\Helm\` | `~/.local/state/helm/` |
| Session logs | `~/Library/Logs/Helm/` | `%LOCALAPPDATA%\Helm\logs\` | `~/.local/state/helm/logs/` |

Config is human-readable and editable; the app watches it and reconciles on change. (A power user can edit `sessions.toml` directly and Helm picks it up — the GUI is the friendly path, not the only path.)

### 7.9 Cross-platform implementation matrix

| Concern | macOS | Windows | Linux |
|---|---|---|---|
| Tray / menu bar | `NSStatusItem` (via Tauri tray) | system tray icon | `StatusNotifierItem` / AppIndicator |
| Launch at login | `SMAppService` / Login Item (LaunchAgent) | Registry `Run` key or Task Scheduler | systemd **user** service or XDG autostart |
| Keep awake | `IOPMAssertion` (`PreventUserIdleSystemSleep`) | `SetThreadExecutionState(ES_CONTINUOUS \| ES_SYSTEM_REQUIRED)` | `systemd-inhibit` / `org.freedesktop.login1` inhibitor lock |
| Sleep / wake events | `NSWorkspace` will-sleep / did-wake | `WM_POWERBROADCAST` | logind `PrepareForSleep` D-Bus signal |
| Native notifications | `UserNotifications` | WinRT toast | `libnotify` / D-Bus `Notifications` |
| Credential / login detection | Keychain + `~/.claude/.credentials.json` | Credential Manager + `~/.claude\` | libsecret + `~/.claude/` |
| Binary resolution | `$SHELL -l -c "which <bin>"` | shell-aware lookup (not bare `where`) | `$SHELL -l -c "which <bin>"` |
| Detached child process | posix spawn, new session/process group | `CREATE_NEW_PROCESS_GROUP` / job object | `setsid`, new process group |

The OS layer is the only place these differ; everything above it is shared Rust.

### 7.10 Logging / transcripts

- The supervisor captures each session's `stdout`/`stderr` to `log_path`. This replaces the user's previous `script`-wrapper approach.
- Logs are append-mode and persist across reboots (never in `/tmp` — macOS cleans it). Optional size cap / rotation.
- The UI exposes "open log" and "reveal in file manager" per session.

---

## 8. Feature specification (detailed)

### 8.1 Session lifecycle
- **Start** — spawn per launch command; transition to `starting` → `working`/`idle`.
- **Stop** — graceful: send interrupt, then terminate after a timeout; transition to `stopped`. Never orphan a child.
- **Restart** — stop then start; preserves `resume` policy (so it continues the conversation).
- **Delete** — stop + remove from config (confirm dialog).
- **Restart policies:** crash → auto-restart with exponential backoff up to `restart_max`, then mark `crashed` and notify. Clean exit → `done`, no restart.
- **Reboot restore:** at login, relaunch all `auto_restart` sessions via their `resume` policy.

### 8.2 Create-session flow
A short guided flow (see 9.3):
1. Pick **folder** — choose existing, or create a new directory (Helm makes it).
2. Pick **agent** — Claude Code / Codex.
3. Pick **profile** or set: name, permission mode, remote (default on), resume mode, model, keep-awake, auto-restart.
4. Start now / save for later.

### 8.3 Permissions & safety
- Per-session `permission`: `safe` / `ask` / `danger` (skip permissions).
- **`danger` sessions must be visually unmistakable** everywhere they appear (list, detail, tray) — see 9.5.
- **Stop-all / panic** control in the tray and window: immediately stops every running session.
- Optional confirmation when creating or starting a `danger` session.
- Security guidance surfaced in-app (Section 10) for `danger` + remote combinations.

### 8.4 Remote access
- `remote` defaults **on**. Toggle per session.
- When on, the UI surfaces the agent's connect affordance (URL / QR) and a clear online/offline indicator.
- Deep link / "open in agent app" where the agent supports it.
- On reconnect after a network drop, Helm re-establishes the session's remote channel automatically (8.6).

### 8.5 Monitoring & status
- Per-session live status: `working`, `needs-permission`, `idle`, `done`, `crashed`, `stopped`, `offline`, `rate-limited`.
- **`needs-permission` floats to the top** of the list and fires a native notification — the user should never unknowingly block an agent.
- Per session: current state, working dir, git branch, agent, remote state, last activity, link to log and parent terminal (if any).
- Expandable view to read the recent conversation tail (read-only).
- Group/tag view for batch operations.

### 8.6 System integration
- **Launch at login** (toggle in settings) — registers the supervisor per 7.9; relaunches the fleet.
- **Keep-awake** — per session (`keep_awake`) and a global toggle. While any keep-awake session is `working`, Helm holds an OS sleep-inhibitor so closing the lid / idle timeout doesn't suspend the machine. Released when no such session is active. *Note for design:* this needs a visible indicator (the machine is being kept awake on purpose) and a one-click override.
- **Sleep/wake handling** — on wake, re-check session health and re-establish remote channels; on sleep, note that local processes pause with the machine.
- **Restart-after-connection-lost** — detect remote/network disconnects and re-establish; detect agent process death and apply restart policy.
- **Documented limitation:** a full OS reboot or process death loses *in-flight task execution*. `--continue`/`--resume` restores the *conversation*, not a running task mid-execution. This is inherent and must be communicated in the UI (not hidden).

### 8.7 Configuration & profiles
- Declarative `sessions.toml` is the source of truth; GUI edits write to it; external edits are picked up.
- **Profiles/templates:** reusable defaults (agent + flags + env). "New session from profile."
- Import/export config (share a fleet setup).
- Per-session env vars and model selection.

### 8.8 Multi-agent
- Claude Code and Codex day one via the backend interface; UI treats them identically. Mixed fleets in one list. New agents = new backend impls only.

### 8.9 CLI / automation surface
- A local `helm` CLI mirroring core actions: `helm start <id>`, `helm stop <id>`, `helm restart <id>`, `helm list`, `helm new`. Lets power users script and lets the GUI and CLI share one core. (Analogous to exposing OS-level "intents/shortcuts" — optional stretch.)

---

## 9. UX & design specification (designer)

You own the visual craft. This section defines the information architecture, the surfaces, the views, and — most importantly — the **states** every view must handle. Build for the states, not just the happy path.

### 9.1 Information architecture
Two surfaces, one mental model ("my fleet of sessions"):
- **Tray / menu bar** — glanceable + quick actions. Always present.
- **Management window** — full control, opened from the tray.

### 9.2 Surfaces

**Tray menu (glance + act):**
- Aggregate status (e.g. "5 running · 1 needs permission") with a color that reflects the worst state.
- Per-session quick rows: name, status dot, start/stop toggle.
- Global actions: New session, Stop all, Open window, Quit.
- A visible indicator when keep-awake is holding the machine awake.

**Management window (control):** see 9.3.

**Notifications:** native, for `needs-permission`, `crashed`, and (optionally) `done` on long runs. Actionable where the OS allows (e.g. "Open session").

### 9.3 Key views (required elements)

**A. Dashboard / session list** (home)
- One row/card per session: name, agent badge, status (with state color), working dir / git branch, remote online/offline, **danger badge if applicable**, quick start/stop/restart.
- Sorting: `needs-permission` first, then `working`, then the rest.
- Grouping by tag (optional).
- Empty state (9.6).

**B. Session detail**
- Full status + state history at a glance.
- Working dir, agent, model, permission mode, remote state + connect affordance (URL/QR).
- Recent conversation tail (read-only).
- Controls: start/stop/restart, edit config, open log, reveal in file manager, open in agent app / terminal.
- Prominent danger treatment if `danger`.

**C. Create / edit session** (the flow in 8.2)
- Folder picker with "create new folder."
- Agent picker; profile picker.
- Fields: name, permission mode, remote (default ON, clearly shown as the default), resume mode, model, keep-awake, auto-restart, env (advanced/collapsible).
- Confirmation step when permission = `danger`.

**D. Settings**
- Launch at login (toggle).
- Global keep-awake behavior.
- Default profile.
- Notification preferences.
- Config file location (with "open").
- Supervisor behavior (Section 13 outcome).

### 9.4 Session states & visual language
Design a distinct, instantly-readable treatment for each. These are the heart of the UI:

| State | Meaning | Priority |
|---|---|---|
| `working` | agent actively running/executing | normal |
| `needs-permission` | blocked, waiting on the user | **highest — float + notify** |
| `idle` | alive, waiting for input | normal |
| `done` | finished cleanly | low |
| `rate-limited` | stalled due to limits | medium (informational) |
| `crashed` | died unexpectedly | high |
| `stopped` | stopped by user | low |
| `offline` | remote channel down | medium |

Use color + shape + label (not color alone — accessibility).

### 9.5 The danger / permission UX
Sessions running with skipped permissions have real blast radius and must be **unmistakable** without being so alarming the user tunes it out. Needed:
- A persistent danger badge on the session everywhere it appears.
- A distinct treatment in the tray aggregate when any `danger` session is running.
- A confirmation moment at create/start.
- This should read as "informed and in control," not "scary pop-up spam."

### 9.6 Empty / first-run / onboarding
- **First run:** detect installed agents (Claude Code / Codex) and login state; guide the user to create their first session. If an agent isn't logged in, link to its login — Helm doesn't reinvent auth.
- **Empty dashboard:** a clear, friendly "Create your first session" call to action.
- **Agent-not-found / not-logged-in** inline states with the fix.

### 9.7 Visual direction & tone
- A focused developer utility, not a dashboard. Calm, dense-but-legible, glanceable. Dark + light, follows system appearance.
- The aesthetic the user gravitates toward is "intelligence-grade, editorial, restrained" — clean cards, clear type hierarchy, minimal chrome. Avoid playful/consumer styling and avoid analytics-dashboard clutter (we deliberately have no charts).
- The tray must be readable at a glance in a crowded menu bar.

### 9.8 Accessibility
- Never rely on color alone for state (pair with icon + label).
- Full keyboard navigation; screen-reader labels on all interactive elements.
- Respect reduced-motion.
- Adequate contrast in both themes.

### 9.9 Interaction & motion
- Status changes animate subtly (no jank, respect reduced-motion).
- Destructive actions (delete, stop-all, start-danger) confirm.
- Live updates stream in without full reloads (state pushed from core).

---

## 10. Security & privacy

- **Local-first, no telemetry.** No network calls except those the agents themselves make. Document this prominently — it's a feature.
- **Credentials:** read-only detection of agent login state; Helm stores no agent credentials of its own.
- **Skip-permissions + remote** is the highest-risk combination. In-app guidance should recommend isolating such sessions (container/VM/dedicated user) for sensitive repos, and the danger UX (9.5) keeps it visible.
- **Config file** may reference paths and env; treat env values as potentially sensitive (don't log them in plaintext logs).
- **Supervisor** runs with the user's own privileges — no elevation. It must never expose a network listener of its own.

---

## 11. Distribution & updates

- **macOS:** signed + notarized `.dmg`; Homebrew cask.
- **Windows:** signed installer (MSI/NSIS); winget manifest.
- **Linux:** AppImage + `.deb`/`.rpm`; consider Flatpak.
- **Auto-update:** GitHub-releases-style update check (Tauri updater), user-controlled.
- CI builds all three from one repo.

---

## 12. MVP & milestones

**M0 — Core supervisor (headless, no UI polish)**
- Backend interface + Claude Code backend; binary/credential detection.
- Spawn/stop/restart detached children; log capture; crash backoff.
- `sessions.toml` read/reconcile; `--continue` on relaunch.
- Local `helm` CLI.

**M1 — Tray + window MVP**
- Tray with status + quick start/stop + new + stop-all.
- Dashboard + create/edit flow + session detail.
- Status detection (working/needs-permission/idle/done/crashed/stopped).
- Launch-at-login + reboot restore.
- Native notifications for needs-permission/crashed.

**M2 — Remote + system polish**
- Remote-on-by-default with connect affordance + online/offline state + reconnect.
- Keep-awake (per-OS) + indicator.
- Sleep/wake handling.

**M3 — Multi-agent + cross-platform hardening**
- Codex backend.
- Windows + Linux parity on the 7.9 matrix.
- Profiles, groups/batch ops, import/export.
- Distribution pipeline (11).

Everything past M3 (voice, advanced automation, etc.) is post-v1.

---

## 13. Open questions / decisions needed

1. **Supervisor model:** tray-app-with-headless-core (simpler, MVP) vs separate daemon (survives UI quit/crash). *Recommendation: ship (a), design IPC for (b).*
2. **Default permission mode** for new sessions: `ask` (safer default) vs `danger` (matches the user's current workflow). *Recommendation: default `ask`, make `danger` a deliberate choice.*
3. **Name** — "Helm" is a placeholder. Run the naming process before public release (note: a k8s tool is also called Helm — check the collision).
4. **UI framework** for the webview (React/Svelte/Solid) — developer's call.
5. **Codex remote specifics** — confirm Codex's native remote invocation and status-detection artifacts when building that backend.
6. **Keep-awake default** — off globally, opt-in per session? *Recommendation: yes — opt-in, with clear indicator.*

---

## 14. Appendix — example `sessions.toml`

```toml
[[session]]
id           = "trading-bot"
name         = "Trading Bot"
agent        = "claude-code"
path         = "~/code/trading-bot"
remote       = true
permission   = "danger"
resume       = "continue"
model        = "default"
keep_awake   = true
auto_restart = true
group        = "work"

[[session]]
id           = "tnp-protocol"
name         = "T&P Protocol"
agent        = "claude-code"
path         = "~/code/tnp"
remote       = true
permission   = "ask"
resume       = "continue"
auto_restart = true
group        = "work"

[[session]]
id           = "codex-scratch"
name         = "Codex Scratch"
agent        = "codex"
path         = "~/code/experiments"
remote       = true
permission   = "ask"
resume       = "fresh"
auto_restart = false
group        = "play"
```

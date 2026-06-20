# Spec addendum — implementation drifts from `spec.md`

The original spec (Helm/Keel) was written before any code existed. After
several rounds of audit + iteration, the code differs from the spec on
purpose in a handful of places. This file is the authoritative record of
those differences so future readers don't think the code is wrong.

If you change something here, change `spec.md` too — they should stay in
sync going forward.

## Naming

- **Final product name:** *Session Manager* (placeholder `Helm`/`Keel` retired).
- **macOS config dir:** `~/Library/Application Support/SessionManager/`
  (spec §7.8 said `Helm/`).
- **Bundle identifier:** `dev.zeiber.session-manager`.

## CLI ↔ supervisor over a Unix domain socket

Spec §8.9 said the CLI mirrors core actions. In practice, the singleton
runtime lock means only one supervisor can hold the config file at a
time — so the CLI cannot open its own supervisor if the GUI app is up.

The shipped model:

- **Supervisor (GUI or `session-manager daemon`) serves a UDS** at
  `state_dir/supervisor.sock` (mode `0600`, same-user only).
- **CLI is a client.** State-changing commands (`start`, `stop`,
  `restart`, `new`, `delete`) connect to the socket; if no supervisor is
  running they error out with a clear message.
- **Read-only commands (`list`, `status`, `path`, `auth`, `logs`)** try
  IPC first, then fall back to reading the on-disk config and runtime
  files directly. They always work.
- **`session-manager daemon`** is the headless replacement for the GUI on
  servers / no-GUI hosts.

This is option (b) from spec §13.1 in spirit (separate daemon), but it
shares the same binary — the GUI process *is* the daemon. A future
refactor could split them into two binaries without changing the IPC
protocol.

## Codex is experimental and UI-hidden

Spec §8.8 expects Claude Code and Codex on day one. The Codex CLI's
real flag set wasn't pinned (open question §13.5), so:

- The `Codex` backend is implemented but with **placeholder flags**.
- It stays in the core `registry()` so existing `sessions.toml` files
  referencing it keep working.
- The Tauri IPC `registry()` filters it out via a `VISIBLE_BACKENDS`
  constant in `src-tauri/src/ipc.rs`, so the UI agent picker shows only
  Claude Code.
- `discover_external()` is implemented for Codex too (best-effort).
- Re-enable Codex by adding it to `VISIBLE_BACKENDS` once §13.5 is closed.

## External-session discovery (new §7.11)

Not in the original spec. Real users have Claude/Codex sessions running
in regular terminal windows before they install Session Manager. The
shipped product:

- Every backend has an optional `discover_external()` method
  (`crates/core/src/backend/mod.rs`).
- The Supervisor merges per-backend lists, **dedups** script-wrapper +
  child processes by `(backend_id, name, cwd)`, filters out our own PID
  and the PIDs of managed workers, and annotates entries whose `cwd`
  matches a managed session's `path`.
- Dashboard shows a "Running outside Session Manager" section with
  `Reveal` / `Stop` / `Adopt` per row.
  - `Adopt` navigates to `/new?name=…&id=…&path=…&agent=…` so the user
    can convert it into a managed session.
  - `Stop` sends SIGTERM via `kill`, after re-verifying the PID's
    command line still looks like an agent (`claude`/`codex`) — protects
    against PID reuse.
- Scans run on the tokio blocking pool (`ps -A` + `lsof` per match).
- The frontend refreshes the list every 8 s and on demand.

## Log rotation

Spec §7.10 listed "optional size cap / rotation". Shipped as required:
each session log rotates to `<id>.log.1` at 5 MB and keeps one prior file.

## Auth check semantics

Spec §7.6 said "presence/validity of local credentials." Shipped:

- **Binary resolution first.** If `claude` or `codex` isn't on the
  user's login-shell PATH we return `BinaryMissing` (a new variant of
  `AuthState`) and Onboarding shows "Not installed".
- **macOS Keychain** is the primary credential source for Claude Code
  (default since mid-2025). We shell out to
  `security find-generic-password -s "Claude Code-credentials"`. Falls
  back to scanning `~/.claude/{.credentials,config,auth}.json` for
  token-shaped fields.
- We deliberately don't validate the token over the network. "Logged in
  but token expired" still shows as `LoggedIn` — verifying would require
  the agent's API and a network round trip, and §3 says local-first.

## Soft-data-loss safeguards

Spec didn't address these; shipped:

- **Malformed `sessions.toml` edits** emit `CoreEvent::ConfigError` →
  toast in the UI, in-memory state untouched.
- **Externally-emptied `sessions.toml`** (e.g. `> sessions.toml`,
  accidental `rm`, an editor saving an empty buffer) is *ignored* when
  the previous file had `N > 0` sessions, and surfaced as a toast.

## Singleton lock via advisory `flock`

Spec didn't specify the mechanism; original implementation used a
PID-file + `kill(pid, 0)` heuristic. Replaced with `fs2`'s advisory
exclusive file lock so:

- The kernel releases the lock on any process exit, including SIGKILL.
- No stale lock cleanup logic to get wrong.
- Works on Windows the same way (`LockFileEx`).

## Graceful shutdown handlers

The Tauri GUI and the CLI daemon both trap `SIGTERM`/`SIGINT` and run
`Supervisor::shutdown()` before exiting:

- `shutdown()` drains every running session via `stop_session` (with the
  oneshot-ack pattern that prevents orphaned children).
- Releases the keep-awake `caffeinate` PID.
- Removes the IPC socket file.

This is the operationally important detail that lets `kill <pid>` work
the way a user expects.

## Things still on the to-do list (not "drifted", just "not built")

- **Profile templates** (§8.7) — `SessionDefaults` are a single global
  blob, not named templates.
- **Windows OS layer** (§7.9) — compiles but the four methods
  (`set_launch_at_login`, `acquire_keep_awake`, `notify`, sleep/wake
  watcher) are all TODO.
- **Linux sleep/wake watcher** (§7.9) — the rest of the Linux layer
  works, but `watch_sleep_events` is a no-op.
- **State-history view** (§9.3.B) — only current status is tracked.
- **Batch operations on groups** (§8.5) — only `Stop all`.
- **Signing / notarization secrets** (§11) — placeholders in
  `release.yml`; fill before tagging a release.
- **Tauri updater** plugin and public key — not wired.

## What is explicitly *not* going to be built

- **Permission-prompt reply path.** The product is a fleet *supervisor*,
  not a permission broker. When an agent asks for permission we
  signal it (badge + native notification + tray attention row); the
  user answers in the agent's own app or remote channel. UI copy is
  written to make this honest.
- **Custom relay / cloud sync / accounts / telemetry.** Anti-features
  per spec §3 and §5.

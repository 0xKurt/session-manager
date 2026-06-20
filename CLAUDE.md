# Session Manager — Claude notes

A Tauri 2 + Rust + React app that supervises multiple AI coding-agent
sessions.

## Release / update signing key

The Ed25519 private key used to sign update bundles lives at:

```
~/.config/session-manager/updater.key      (mode 0600)
~/.config/session-manager/updater.key.pub  (matching pubkey)
```

The **pubkey** is also embedded in `src-tauri/tauri.conf.json` under
`plugins.updater.pubkey` (safe to publish, ships in every binary).

**Lose the private key and every installed copy stops accepting your
updates** — the embedded pubkey will reject anything not signed by it.
Back it up to a password manager / encrypted drive. NEVER commit it;
`.gitignore` already excludes `updater.key{,.pub}`.

To cut a release: bump `version` in `src-tauri/tauri.conf.json`, then
`./scripts/release.sh --notes "What changed"` — that builds, signs with
the key above, and creates the GitHub release on `0xKurt/session-manager`.

## CI

No CI. Builds are local via `scripts/release.sh`. GitHub Actions
workflows were intentionally removed — keep them gone unless you want
the cross-platform Tauri build matrix back (expensive on Actions
minutes; cheaper to just `tauri build` on the dev machine).

## Layout
- `crates/core` — supervisor, backend trait, OS layer, paths, events
- `crates/cli` — `session-manager` CLI
- `src-tauri` — Tauri shell: tray, IPC commands, window event handlers
- `ui` — React+Vite+TS frontend

## Build commands
- `cargo check --workspace` — fast sanity check across all crates
- `cd ui && npm run typecheck` — TS
- `cd ui && npm run build` — production frontend
- `cargo run -p session-manager-app` — launch the GUI (already-built `ui/dist` is loaded; no Tauri CLI needed)
- `~/git/session-manager/target/debug/session-manager <cmd>` — the CLI

## DO NOT run

- `cargo install tauri-cli` — the dep tree is enormous (~150 crates, includes a rebuild of `tauri-utils`, `wry`, `tao`, `oxc`, `jsonrpsee`, image codecs). On this machine it spawned rustc processes at 500% CPU each and the laptop fan went wild. We don't need it: `cargo run -p session-manager-app` boots the GUI directly because `tauri.conf.json` already points at the built `ui/dist`.
- `cargo install` *anything* big in a background shell without flagging it to the user first.

## Architectural invariants (do not break)
- The supervisor is a separate long-lived actor; window close hides, doesn't quit (`src-tauri/src/lib.rs` window-event handler).
- `Tauri::RunEvent::Exit` calls `Supervisor::shutdown` to drain sessions; never let children orphan.
- `stop_session` waits for a oneshot `ack` from the worker before returning. Never `JoinHandle::abort()`; would orphan the child (setsid).
- Children spawn under `setsid` (Unix) / `CREATE_NEW_PROCESS_GROUP` (Windows). On graceful stop we SIGTERM then SIGKILL after a short deadline.
- Spawned env is `minimal_env(session)` only — host env must not leak.
- Single supervisor per user: PID lockfile at `state_dir/supervisor.lock`.
- `intentionally_stopped` is in-memory only — reboot resets, so auto_restart sessions come back at login.
- Notifications go through `notify_gated`, not `os.notify` directly, so the user pref is honoured.
- Remote control: agent's native feature. We never proxy. The supervisor watches stdout for the URL and emits `RemoteAffordance`.

## OS layer
- `crates/core/src/os/macos.rs` is the full impl reference.
- `crates/core/src/os/windows.rs` and `linux.rs` compile but stub the system pieces (launch-at-login, keep-awake, sleep/wake) — fill them in as the project ships those OSes.

## Adding a new backend
Implement `Backend` in `crates/core/src/backend/<id>.rs`, then register in `make_backend()` and `registry()` in `backend/mod.rs`. Nothing else touches it.


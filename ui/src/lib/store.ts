import { create } from "zustand";

import { api } from "./api";
import { subscribeCoreEvents } from "./events";
import type {
  AppPreferences,
  AuthState,
  BackendInfo,
  CoreEvent,
  DiscoveredSession,
  RuntimeState,
  SessionConfig,
  SessionRuntime,
  SessionStatus,
  SessionsFile,
} from "../types";

export type LogLineEntry = {
  line: string;
  is_stderr: boolean;
  at_ms: number;
};

interface StoreState {
  ready: boolean;
  preferences: AppPreferences | null;
  sessions: SessionConfig[];
  runtime: RuntimeState;
  registry: BackendInfo[];
  authStates: Record<string, AuthState>;
  configPath: string | null;
  external: DiscoveredSession[];
  // Per-session log tails and transcripts.
  logs: Record<string, LogLineEntry[]>;
  transcripts: Record<string, string[]>;
  // Toasts (in-app notifications mirroring native ones for visibility).
  toasts: {
    id: string;
    title: string;
    body: string;
    tone: "info" | "danger";
    action?: { label: string; run: () => void };
  }[];
  // Actions
  bootstrap: () => Promise<void>;
  refresh: () => Promise<void>;
  refreshExternal: () => Promise<void>;
  stopExternal: (pid: number) => Promise<void>;
  start: (id: string) => Promise<void>;
  stop: (id: string) => Promise<void>;
  restart: (id: string) => Promise<void>;
  stopAll: () => Promise<void>;
  create: (s: SessionConfig) => Promise<void>;
  update: (s: SessionConfig) => Promise<void>;
  remove: (id: string) => Promise<void>;
  setPrefs: (patch: Partial<AppPreferences>) => Promise<void>;
  setLaunchAtLogin: (enabled: boolean) => Promise<void>;
  dismissToast: (id: string) => void;
}

const LOG_TAIL_KEEP = 200;

// Module-level singletons so a re-render or HMR cycle doesn't accumulate
// duplicate event subscriptions / intervals.
let bootstrapStarted = false;
let externalIntervalId: number | undefined;
let unsubscribeCoreEvents: (() => Promise<void>) | undefined;
if (typeof import.meta !== "undefined" && (import.meta as { hot?: { dispose?: (cb: () => void) => void } }).hot) {
  (import.meta as { hot?: { dispose?: (cb: () => void) => void } }).hot!.dispose!(() => {
    if (externalIntervalId !== undefined) {
      window.clearInterval(externalIntervalId);
      externalIntervalId = undefined;
    }
    if (unsubscribeCoreEvents) {
      void unsubscribeCoreEvents();
      unsubscribeCoreEvents = undefined;
    }
    bootstrapStarted = false;
  });
}

export const useStore = create<StoreState>((set, get) => ({
  ready: false,
  preferences: null,
  sessions: [],
  runtime: { sessions: {}, keep_awake_active: false },
  registry: [],
  authStates: {},
  configPath: null,
  external: [],
  logs: {},
  transcripts: {},
  toasts: [],

  async bootstrap() {
    // Hot-reload safe: don't double-subscribe or double-interval if
    // bootstrap runs twice (React Strict Mode, HMR, Tauri reloads).
    if (bootstrapStarted) return;
    bootstrapStarted = true;

    const [list, registry, authStates, configPath, external] = await Promise.all([
      api.listSessions(),
      api.registry(),
      api.authStates(),
      api.configFilePath(),
      api.externalSessions().catch(() => [] as DiscoveredSession[]),
    ]);
    set({
      ready: true,
      preferences: list.file.preferences,
      sessions: list.file.sessions,
      runtime: list.runtime,
      registry,
      authStates,
      configPath,
      external,
    });
    unsubscribeCoreEvents = subscribeCoreEvents((ev) => handleEvent(ev, set, get));
    // Refresh the external list on a cadence — they're discovered by
    // scanning ps, which is too noisy to push as events.
    const tick = async () => {
      try { await get().refreshExternal(); } catch (err) { console.error(err); }
    };
    externalIntervalId = window.setInterval(tick, 8000);

    // Power-user preset suggestion: if 3+ existing sessions are `danger`
    // and the current default is still `ask`, offer to flip the default.
    // Honors a dismiss flag so we only ask once.
    const dangerCount = list.file.sessions.filter((s) => s.permission === "danger").length;
    if (
      !list.file.preferences.power_user_prompt_dismissed &&
      list.file.preferences.defaults.permission !== "danger" &&
      dangerCount >= 3
    ) {
      pushToast(set, {
        title: "You consistently use skip-permissions",
        body: `${dangerCount} of your sessions are danger. Set danger as the default for new sessions?`,
        tone: "info",
        action: {
          label: "Apply",
          run: () => {
            void api
              .updatePreferences({
                defaults: { ...list.file.preferences.defaults, permission: "danger", remote: true, auto_restart: true, keep_awake: true },
                power_user_prompt_dismissed: true,
              })
              .then((next) => set({ preferences: next }));
          },
        },
      });
      // Also flip the dismiss flag if the user ignores the toast — once is
      // enough.
      window.setTimeout(() => {
        if (!get().preferences?.power_user_prompt_dismissed) {
          void api
            .updatePreferences({ power_user_prompt_dismissed: true })
            .then((next) => set({ preferences: next }));
        }
      }, 20_000);
    }
  },

  async refresh() {
    const list = await api.listSessions();
    set({
      preferences: list.file.preferences,
      sessions: list.file.sessions,
      runtime: list.runtime,
    });
  },

  async refreshExternal() {
    try {
      const external = await api.externalSessions();
      set({ external });
    } catch (err) {
      console.error(err);
    }
  },

  async stopExternal(pid) {
    await api.stopExternal(pid);
    await get().refreshExternal();
  },

  async start(id) { await api.startSession(id); },
  async stop(id) { await api.stopSession(id); },
  async restart(id) { await api.restartSession(id); },
  async stopAll() { await api.stopAll(); },

  async create(s) {
    await api.createSession(s);
    await get().refresh();
    maybeOfferLaunchAtLogin(set, get);
  },
  async update(s) {
    await api.updateSession(s);
    await get().refresh();
  },
  async remove(id) {
    // Cache the config so the user can undo within the toast window.
    const snapshot = get().sessions.find((s) => s.id === id);
    await api.deleteSession(id);
    await get().refresh();
    if (snapshot) {
      pushToast(set, {
        title: `Deleted "${snapshot.name}"`,
        body: "",
        tone: "info",
        action: {
          label: "Undo",
          run: () => { void api.createSession(snapshot).then(() => get().refresh()); },
        },
      });
    }
  },
  async setPrefs(patch) {
    const next = await api.updatePreferences(patch);
    set({ preferences: next });
  },
  async setLaunchAtLogin(enabled) {
    await api.setLaunchAtLogin(enabled);
    await get().setPrefs({ launch_at_login: enabled });
  },

  dismissToast(id) {
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
  },
}));

function handleEvent(
  event: CoreEvent,
  set: (
    partial:
      | Partial<StoreState>
      | ((s: StoreState) => Partial<StoreState>)
  ) => void,
  get: () => StoreState,
) {
  switch (event.type) {
    case "status-changed": {
      const r = { ...get().runtime };
      const sessions = { ...r.sessions };
      const prev = sessions[event.session_id] ?? blankRuntime(event.session_id);
      sessions[event.session_id] = {
        ...prev,
        status: event.status,
        reason: event.reason ?? prev.reason,
      };
      set({ runtime: { ...r, sessions } });
      break;
    }
    case "log-line": {
      const logs = { ...get().logs };
      const cur = logs[event.session_id] ?? [];
      const next = [...cur, { line: event.line, is_stderr: event.is_stderr, at_ms: event.at_ms }];
      if (next.length > LOG_TAIL_KEEP) {
        next.splice(0, next.length - LOG_TAIL_KEEP);
      }
      logs[event.session_id] = next;
      set({ logs });
      break;
    }
    case "transcript-tail": {
      const transcripts = { ...get().transcripts };
      transcripts[event.session_id] = event.lines;
      set({ transcripts });
      break;
    }
    case "remote-affordance": {
      const r = { ...get().runtime };
      const sessions = { ...r.sessions };
      const prev = sessions[event.session_id] ?? blankRuntime(event.session_id);
      sessions[event.session_id] = {
        ...prev,
        remote_url: event.url ?? prev.remote_url,
        remote_online: !!event.url,
        // Carry the QR SVG through so SessionDetail can render it live.
        // Without this the supervisor-generated QR was only visible after
        // a full refresh round-trip through list_sessions.
        remote_qr: event.qr ?? prev.remote_qr,
      };
      set({ runtime: { ...r, sessions } });
      break;
    }
    case "needs-permission": {
      const session = get().sessions.find((s) => s.id === event.session_id);
      pushToast(set, {
        title: `${session?.name ?? event.session_id} needs permission`,
        body: event.prompt ?? "Open the session to answer.",
        tone: "danger",
      });
      break;
    }
    case "config-reloaded": {
      void get().refresh();
      break;
    }
    case "config-error": {
      pushToast(set, {
        title: "sessions.toml problem",
        body: event.message,
        tone: "danger",
      });
      break;
    }
    case "keep-awake-changed": {
      const r = { ...get().runtime };
      set({ runtime: { ...r, keep_awake_active: event.active } });
      break;
    }
    case "binary-upgraded": {
      const running = Object.values(get().runtime.sessions).filter(
        (r) => r.status === "working" || r.status === "starting" || r.status === "idle" || r.status === "needs-permission" || r.status === "rate-limited" || r.status === "offline",
      );
      pushToast(set, {
        title: `${event.backend_id} updated`,
        body: `New binary at ${event.new_path}. Running sessions are still on the old one — restart to pick up the update.`,
        tone: "info",
        action: running.length > 0
          ? {
              label: `Restart ${running.length}`,
              run: () => {
                for (const r of running) void api.restartSession(r.id);
              },
            }
          : undefined,
      });
      break;
    }
  }
}

function pushToast(
  set: (
    partial:
      | Partial<StoreState>
      | ((s: StoreState) => Partial<StoreState>)
  ) => void,
  t: {
    title: string;
    body: string;
    tone: "info" | "danger";
    action?: { label: string; run: () => void };
  },
) {
  const id = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  set((s) => ({ toasts: [...s.toasts, { id, ...t }] }));
  const dwell = t.action ? 4500 : 6500;
  setTimeout(() => {
    set((s) => ({ toasts: s.toasts.filter((x) => x.id !== id) }));
  }, dwell);
}

/// After the user has at least one managed session, offer to flip
/// launch_at_login so their fleet comes back after reboot — that's the
/// product's headline promise. Honors a one-time dismiss flag.
function maybeOfferLaunchAtLogin(
  set: (
    partial:
      | Partial<StoreState>
      | ((s: StoreState) => Partial<StoreState>)
  ) => void,
  get: () => StoreState,
) {
  const s = get();
  const prefs = s.preferences;
  if (!prefs) return;
  if (prefs.launch_at_login) return;
  if (prefs.launch_at_login_prompt_dismissed) return;
  if (s.sessions.length === 0) return;
  pushToast(set, {
    title: "Want your sessions back after a reboot?",
    body: "Turn on Launch at login and Session Manager will restore your fleet automatically.",
    tone: "info",
    action: {
      label: "Enable",
      run: () => {
        void api.setLaunchAtLogin(true).then(() =>
          api.updatePreferences({ launch_at_login: true, launch_at_login_prompt_dismissed: true })
            .then((next) => set({ preferences: next })),
        );
      },
    },
  });
  // Auto-dismiss after 25s — once the toast is gone, we shouldn't keep
  // showing it on every create.
  window.setTimeout(() => {
    if (!get().preferences?.launch_at_login_prompt_dismissed) {
      void api
        .updatePreferences({ launch_at_login_prompt_dismissed: true })
        .then((next) => set({ preferences: next }));
    }
  }, 25_000);
}

function blankRuntime(id: string): SessionRuntime {
  return {
    id,
    status: "stopped" as SessionStatus,
    restart_count: 0,
    remote_online: false,
  };
}

export type { SessionsFile };

import { invoke } from "@tauri-apps/api/core";

import type {
  AppPreferences,
  AuthState,
  BackendInfo,
  DiscoveredSession,
  ListSessionsResp,
  SessionConfig,
  SessionRuntime,
} from "../types";

export const api = {
  listSessions: () => invoke<ListSessionsResp>("list_sessions"),
  sessionRuntimeSnapshot: (id: string) =>
    invoke<SessionRuntime | null>("session_runtime_snapshot", { id }),
  startSession: (id: string) => invoke<void>("start_session", { id }),
  stopSession: (id: string) => invoke<void>("stop_session", { id }),
  restartSession: (id: string) => invoke<void>("restart_session", { id }),
  createSession: (session: SessionConfig) => invoke<void>("create_session", { session }),
  updateSession: (session: SessionConfig) => invoke<void>("update_session", { session }),
  deleteSession: (id: string) => invoke<void>("delete_session", { id }),
  stopAll: () => invoke<void>("stop_all"),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),
  openInOs: (path: string) => invoke<void>("open_in_os", { path }),
  registry: () => invoke<BackendInfo[]>("registry"),
  authStates: () => invoke<Record<string, AuthState>>("auth_states"),
  updatePreferences: (patch: Partial<AppPreferences>) =>
    invoke<AppPreferences>("update_preferences", { patch }),
  setLaunchAtLogin: (enabled: boolean) =>
    invoke<boolean>("set_launch_at_login", { enabled }),
  configFilePath: () => invoke<string>("config_file_path"),
  exportConfig: (path: string) => invoke<void>("export_config", { path }),
  importConfig: (path: string) => invoke<void>("import_config", { path }),
  resolvedLogPath: (id: string) => invoke<string>("resolved_log_path", { id }),
  pathExists: (path: string) => invoke<boolean>("path_exists", { path }),
  pathKind: (path: string) => invoke<"dir" | "file" | "missing" | "other">("path_kind", { path }),
  resetAndRetry: (id: string) => invoke<void>("reset_and_retry", { id }),
  externalSessions: () => invoke<DiscoveredSession[]>("external_sessions"),
  stopExternal: (pid: number) => invoke<void>("stop_external", { pid }),
  adoptExternal: (pid: number, session: SessionConfig) =>
    invoke<void>("adopt_external", { pid, session }),
  claimExternal: (pid: number, session: SessionConfig) =>
    invoke<void>("claim_external", { pid, session }),
};

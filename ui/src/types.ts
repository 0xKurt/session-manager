// TS mirrors of crates/core types. Keep in sync.

export type PermissionMode = "safe" | "ask" | "danger";
export type ResumeMode = "continue" | "resume" | "fresh";
export type AuthState = "logged-in" | "logged-out" | "binary-missing" | "unknown";

export type SessionStatus =
  | "starting"
  | "working"
  | "needs-permission"
  | "idle"
  | "done"
  | "rate-limited"
  | "crashed"
  | "stopped"
  | "offline";

export interface SessionConfig {
  id: string;
  name: string;
  agent: string;
  path: string;
  remote: boolean;
  permission: PermissionMode;
  resume: ResumeMode;
  resume_id: string;
  model: string;
  keep_awake: boolean;
  auto_restart: boolean;
  restart_max: number;
  env: Record<string, string>;
  log_path?: string | null;
  group?: string | null;
  record_stdout: boolean;
  extra_args: string[];
}

export interface SessionDefaults {
  agent: string;
  permission: PermissionMode;
  remote: boolean;
  resume: ResumeMode;
  model: string;
  keep_awake: boolean;
  auto_restart: boolean;
}

export interface AppPreferences {
  launch_at_login: boolean;
  notifications_enabled: boolean;
  keep_awake_master: boolean;
  defaults: SessionDefaults;
  power_user_prompt_dismissed: boolean;
  launch_at_login_prompt_dismissed: boolean;
}

export interface SessionsFile {
  preferences: AppPreferences;
  sessions: SessionConfig[];
}

export interface SessionRuntime {
  id: string;
  pid?: number | null;
  status: SessionStatus;
  reason?: string | null;
  started_at?: string | null;
  last_seen?: string | null;
  restart_count: number;
  remote_url?: string | null;
  remote_online: boolean;
  remote_qr?: string | null;
  claude_jsonl_path?: string | null;
  last_activity?: string | null;
}

export interface RuntimeState {
  sessions: Record<string, SessionRuntime>;
  keep_awake_active: boolean;
}

export interface BackendInfo {
  id: string;
  display_name: string;
}

export interface DiscoveredSession {
  pid: number;
  backend_id: string;
  display_name: string;
  cwd: string;
  args: string[];
  matches_session_id?: string | null;
  status?: SessionStatus | null;
  last_activity?: string | null;
}

export interface ListSessionsResp {
  file: SessionsFile;
  runtime: RuntimeState;
}

export type CoreEvent =
  | { type: "status-changed"; session_id: string; status: SessionStatus; reason?: string | null }
  | { type: "log-line"; session_id: string; line: string; is_stderr: boolean; at_ms: number }
  | { type: "transcript-tail"; session_id: string; lines: string[] }
  | { type: "remote-affordance"; session_id: string; url?: string | null; qr?: string | null }
  | { type: "needs-permission"; session_id: string; prompt?: string | null }
  | { type: "config-reloaded" }
  | { type: "config-error"; message: string }
  | { type: "keep-awake-changed"; active: boolean; reason: string }
  | { type: "binary-upgraded"; backend_id: string; old_path: string; new_path: string };

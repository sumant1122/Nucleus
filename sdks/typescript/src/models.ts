/** Data models for the Nucleus SDK. */

export interface SandboxSpec {
  image?: string;
  command?: string[];
  env?: string[];
  workdir?: string;
  memory?: string;
  cpus?: number;
  pids_limit?: number;
  rootless?: boolean;
  readonly?: boolean;
  network?: "none" | "allowlist" | "open";
  network_allowlist?: string[];
  ttl_seconds?: number;
  idle_timeout_seconds?: number;
  labels?: Record<string, string>;
}

export interface Sandbox {
  id: string;
  image: string;
  status: string;
  pid?: number;
  ip?: string;
  memory?: string;
  cpus?: number;
  pids_limit?: number;
  network?: string;
  created_at?: string;
  started_at?: string;
  stopped_at?: string;
  exit_code?: number;
  ttl_seconds?: number;
  idle_timeout_seconds?: number;
  labels?: Record<string, string>;
}

export interface ExecRequest {
  command: string[];
  env?: string[];
  workdir?: string;
  timeout_seconds?: number;
  output_cap_bytes?: number;
  stdin?: string;
}

export interface ExecResult {
  exit_code: number;
  stdout: string;
  stderr: string;
  duration_ms: number;
  truncated: boolean;
}

export interface StreamExecRequest {
  command: string[];
  env?: string[];
  workdir?: string;
  timeout_seconds?: number;
  output_cap_bytes?: number;
}

export interface ExecEvent {
  type: "stdout" | "stderr" | "exit";
  data: string;
}

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
}

export interface ListFilesResponse {
  path: string;
  entries: FileEntry[];
}

export interface SnapshotResponse {
  snapshot_id: string;
  name: string;
  created_at: string;
}

export interface ListSandboxesResponse {
  sandboxes: Sandbox[];
}

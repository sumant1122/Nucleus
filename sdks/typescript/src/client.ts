/** Synchronous-style client for the Nucleus sandbox API (uses fetch). */

import {
  ExecEvent,
  ExecRequest,
  ExecResult,
  FileEntry,
  ListFilesResponse,
  ListSandboxesResponse,
  Sandbox,
  SandboxSpec,
  SnapshotResponse,
  StreamExecRequest,
} from "./models";
import {
  NucleusAPIError,
  NucleusConnectionError,
  NucleusError,
  NucleusNotFoundError,
  NucleusTimeoutError,
  mapHttpError,
} from "./exceptions";

export class NucleusClient {
  private baseUrl: string;
  private timeout: number;
  private headers: Record<string, string>;

  constructor(
    baseUrl = "http://127.0.0.1:8080",
    timeout = 30000,
    headers: Record<string, string> = {}
  ) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.timeout = timeout;
    this.headers = headers;
  }

  private async request<T = any>(
    method: string,
    path: string,
    options: {
      params?: Record<string, string>;
      body?: any;
    } = {}
  ): Promise<T> {
    const url = new URL(`${this.baseUrl}${path}`);

    if (options.params) {
      for (const [key, value] of Object.entries(options.params)) {
        url.searchParams.set(key, value);
      }
    }

    const init: RequestInit = {
      method,
      headers: { ...this.headers },
    };

    if (options.body !== undefined) {
      init.headers = {
        ...init.headers,
        "Content-Type": "application/json",
      };
      init.body = JSON.stringify(options.body);
    }

    let response: Response;
    try {
      const controller = new AbortController();
      const timeoutId = setTimeout(() => controller.abort(), this.timeout);
      init.signal = controller.signal;

      response = await fetch(url.toString(), init);
      clearTimeout(timeoutId);
    } catch (err: any) {
      if (err.name === "AbortError") {
        throw new NucleusTimeoutError(`Request timed out: ${method} ${path}`);
      }
      throw new NucleusConnectionError(
        `Cannot connect to Nucleus daemon at ${this.baseUrl}`
      );
    }

    if (response.status >= 400) {
      let detail: string | undefined;
      try {
        const body = (await response.json()) as { detail?: string };
        detail = body.detail;
      } catch {
        detail = response.statusText;
      }
      throw mapHttpError(response.status, response.statusText, detail);
    }

    if (response.status === 204) return null as unknown as T;

    return (await response.json()) as T;
  }

  /** Create a new sandbox. */
  async createSandbox(spec: SandboxSpec): Promise<Sandbox> {
    const data = await this.request<Sandbox>("POST", "/sandboxes", {
      body: spec,
    });
    return data;
  }

  /** List all sandboxes. */
  async listSandboxes(): Promise<ListSandboxesResponse> {
    return this.request<ListSandboxesResponse>("GET", "/sandboxes");
  }

  /** Get a sandbox by ID. */
  async getSandbox(sandboxId: string): Promise<Sandbox> {
    return this.request<Sandbox>("GET", `/sandboxes/${sandboxId}`);
  }

  /** Start a sandbox. */
  async startSandbox(sandboxId: string): Promise<Sandbox> {
    return this.request<Sandbox>("POST", `/sandboxes/${sandboxId}/start`);
  }

  /** Stop a sandbox. */
  async stopSandbox(sandboxId: string): Promise<Sandbox> {
    return this.request<Sandbox>("POST", `/sandboxes/${sandboxId}/stop`);
  }

  /** Destroy a sandbox and reclaim all resources. */
  async destroySandbox(sandboxId: string): Promise<void> {
    await this.request<void>("DELETE", `/sandboxes/${sandboxId}`);
  }

  /** Execute a command in a sandbox and wait for completion. */
  async execSandbox(
    sandboxId: string,
    command: string[],
    opts?: {
      env?: string[];
      workdir?: string;
      timeoutSeconds?: number;
      outputCapBytes?: number;
    }
  ): Promise<ExecResult> {
    const body: ExecRequest = { command, ...opts };
    return this.request<ExecResult>("POST", `/sandboxes/${sandboxId}/exec`, {
      body,
    });
  }

  /** Execute a command and stream output events via SSE. */
  async *execStream(
    sandboxId: string,
    command: string[],
    opts?: {
      env?: string[];
      workdir?: string;
      timeoutSeconds?: number;
      outputCapBytes?: number;
    }
  ): AsyncGenerator<ExecEvent> {
    const body: StreamExecRequest = { command, ...opts };
    const url = new URL(`${this.baseUrl}/sandboxes/${sandboxId}/exec/stream`);

    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), this.timeout);

    let response: Response;
    try {
      response = await fetch(url.toString(), {
        method: "POST",
        headers: { ...this.headers, "Content-Type": "application/json" },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      clearTimeout(timeoutId);
    } catch (err: any) {
      clearTimeout(timeoutId);
      if (err.name === "AbortError") {
        throw new NucleusTimeoutError(
          `Streaming exec timed out: ${sandboxId}`
        );
      }
      throw new NucleusConnectionError(
        `Cannot connect to Nucleus daemon at ${this.baseUrl}`
      );
    }

    if (response.status >= 400) {
      let detail: string | undefined;
      try {
        const body = (await response.json()) as { detail?: string };
        detail = body.detail;
      } catch {
        detail = response.statusText;
      }
      throw mapHttpError(response.status, response.statusText, detail);
    }

    const reader = response.body?.getReader();
    if (!reader) {
      throw new NucleusConnectionError("No response body");
    }

    const decoder = new TextDecoder();
    let buffer = "";

    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() ?? "";

        for (const line of lines) {
          if (!line) continue;
          if (line.startsWith("data: ")) {
            try {
              const payload = JSON.parse(line.slice(6));
              yield {
                type: payload.type as "stdout" | "stderr" | "exit",
                data: payload.data ?? "",
              };
            } catch {
              // Skip malformed SSE data lines
            }
          }
        }
      }
    } finally {
      reader.releaseLock();
    }
  }

  /** Read a file from a sandbox. */
  async readFile(sandboxId: string, path: string): Promise<Buffer> {
    const data = await this.request<any>(
      "GET",
      `/sandboxes/${sandboxId}/files`,
      { params: { path } }
    );
    const content = data.content;
    if (typeof content === "string") {
      return Buffer.from(content);
    }
    return Buffer.from(content);
  }

  /** Write a file to a sandbox. */
  async writeFile(
    sandboxId: string,
    path: string,
    content: Buffer | string,
    mode?: number
  ): Promise<void> {
    const contentBytes =
      typeof content === "string" ? Buffer.from(content) : content;
    const params: Record<string, string> = {};
    if (mode !== undefined) {
      params.mode = mode.toString(8);
    }

    await this.request<void>(
      "PUT",
      `/sandboxes/${sandboxId}/files`,
      {
        params,
        body: {
          path,
          content: Array.from(contentBytes),
        },
      }
    );
  }

  /** List files in a directory inside a sandbox. */
  async listFiles(
    sandboxId: string,
    path = "/"
  ): Promise<ListFilesResponse> {
    return this.request<ListFilesResponse>(
      "GET",
      `/sandboxes/${sandboxId}/files`,
      { params: { path, list: "true" } }
    );
  }

  /** Create a snapshot of a sandbox's overlay filesystem. */
  async snapshotSandbox(
    sandboxId: string,
    name?: string
  ): Promise<SnapshotResponse> {
    const params: Record<string, string> = {};
    if (name) params.name = name;
    return this.request<SnapshotResponse>(
      "POST",
      `/sandboxes/${sandboxId}/snapshot`,
      { params }
    );
  }

  /** Restore a sandbox from a snapshot. */
  async resetSandbox(
    sandboxId: string,
    snapshotName: string
  ): Promise<SnapshotResponse> {
    return this.request<SnapshotResponse>(
      "POST",
      `/sandboxes/${sandboxId}/reset`,
      { params: { name: snapshotName } }
    );
  }
}

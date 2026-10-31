"""Nucleus MCP server.

Exposes Nucleus agent sandbox operations as Model Context Protocol tools,
allowing AI agents to directly create, manage, and execute commands in
Nucleus sandboxes.
"""

from __future__ import annotations

import asyncio
import json
import sys
from typing import Any

import httpx

from mcp.server import Server, ServerRequestContext
from mcp.server.stdio import stdio_server
from mcp.types import (
    CallToolRequestParams,
    ListToolsResult,
    PaginatedRequestParams,
    TextContent,
    Tool,
)

# ---------------------------------------------------------------------------
# Nucleus daemon client
# ---------------------------------------------------------------------------

DEFAULT_BASE_URL = "http://127.0.0.1:8080"


class NucleusMCPClient:
    """Thin HTTP client talking to sandboxd."""

    def __init__(self, base_url: str = DEFAULT_BASE_URL):
        self.base_url = base_url.rstrip("/")
        self._client = httpx.AsyncClient(timeout=30.0)

    async def close(self) -> None:
        await self._client.aclose()

    async def _request(
        self,
        method: str,
        path: str,
        *,
        params: dict[str, Any] | None = None,
        json: dict[str, Any] | None = None,
    ) -> Any:
        url = f"{self.base_url}{path}"
        try:
            response = await self._client.request(
                method, url, params=params, json=json
            )
        except httpx.ConnectError as exc:
            raise RuntimeError(
                f"Cannot connect to Nucleus daemon at {self.base_url}"
            ) from exc
        except httpx.TimeoutException as exc:
            raise RuntimeError(f"Request timed out: {method} {path}") from exc

        if response.status_code >= 400:
            detail: str | None = None
            try:
                body = response.json()
                detail = body.get("detail")
            except Exception:
                detail = response.text
            raise RuntimeError(
                f"API error {response.status_code}: {response.status_text} "
                f"({detail})"
            )

        if response.status_code == 204:
            return None
        return response.json()

    # Sandbox lifecycle
    async def create_sandbox(self, spec: dict[str, Any]) -> dict[str, Any]:
        return await self._request("POST", "/sandboxes", json=spec)

    async def list_sandboxes(self) -> dict[str, Any]:
        return await self._request("GET", "/sandboxes")

    async def get_sandbox(self, sandbox_id: str) -> dict[str, Any]:
        return await self._request("GET", f"/sandboxes/{sandbox_id}")

    async def start_sandbox(self, sandbox_id: str) -> dict[str, Any]:
        return await self._request("POST", f"/sandboxes/{sandbox_id}/start")

    async def stop_sandbox(self, sandbox_id: str) -> dict[str, Any]:
        return await self._request("POST", f"/sandboxes/{sandbox_id}/stop")

    async def destroy_sandbox(self, sandbox_id: str) -> None:
        await self._request("DELETE", f"/sandboxes/{sandbox_id}")

    # Exec
    async def exec_sandbox(
        self,
        sandbox_id: str,
        command: list[str],
        env: list[str] | None = None,
        workdir: str | None = None,
        timeout_seconds: int | None = None,
        output_cap_bytes: int | None = None,
    ) -> dict[str, Any]:
        body: dict[str, Any] = {"command": command}
        if env is not None:
            body["env"] = env
        if workdir is not None:
            body["workdir"] = workdir
        if timeout_seconds is not None:
            body["timeout_seconds"] = timeout_seconds
        if output_cap_bytes is not None:
            body["output_cap_bytes"] = output_cap_bytes
        return await self._request(
            "POST", f"/sandboxes/{sandbox_id}/exec", json=body
        )

    # Files
    async def read_file(self, sandbox_id: str, path: str) -> dict[str, Any]:
        return await self._request(
            "GET", f"/sandboxes/{sandbox_id}/files", params={"path": path}
        )

    async def write_file(
        self,
        sandbox_id: str,
        path: str,
        content: str,
        mode: int | None = None,
    ) -> None:
        params: dict[str, Any] = {}
        if mode is not None:
            params["mode"] = oct(mode)
        await self._request(
            "PUT",
            f"/sandboxes/{sandbox_id}/files",
            params=params,
            json={"path": path, "content": list(content.encode())},
        )

    async def list_files(
        self, sandbox_id: str, path: str = "/"
    ) -> dict[str, Any]:
        return await self._request(
            "GET",
            f"/sandboxes/{sandbox_id}/files",
            params={"path": path, "list": "true"},
        )

    # Snapshots
    async def snapshot_sandbox(
        self, sandbox_id: str, name: str | None = None
    ) -> dict[str, Any]:
        params: dict[str, Any] = {}
        if name is not None:
            params["name"] = name
        return await self._request(
            "POST", f"/sandboxes/{sandbox_id}/snapshot", params=params
        )

    async def reset_sandbox(
        self, sandbox_id: str, snapshot_name: str
    ) -> dict[str, Any]:
        return await self._request(
            "POST",
            f"/sandboxes/{sandbox_id}/reset",
            params={"name": snapshot_name},
        )


# ---------------------------------------------------------------------------
# MCP server (v2 API: on_list_tools / on_call_tool callbacks)
# ---------------------------------------------------------------------------

client = NucleusMCPClient()


# Tool definitions shared between list and call handlers
_TOOL_DEFS: dict[str, Tool] = {
    "sandbox_create": Tool(
        name="sandbox_create",
        description="Create a new Nucleus sandbox.",
        inputSchema={
            "type": "object",
            "properties": {
                "image": {
                    "type": "string",
                    "description": "Container image (e.g. alpine, ubuntu)",
                    "default": "alpine",
                },
                "command": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Default command to run",
                },
                "env": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Environment variables (KEY=VALUE)",
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory inside sandbox",
                },
                "memory": {
                    "type": "string",
                    "description": "Memory limit (e.g. 512M, 1G)",
                },
                "cpus": {
                    "type": "number",
                    "description": "CPU limit (e.g. 1.5)",
                },
                "pids_limit": {
                    "type": "integer",
                    "description": "Maximum PIDs",
                },
                "rootless": {
                    "type": "boolean",
                    "description": "Run in rootless mode",
                    "default": False,
                },
                "readonly": {
                    "type": "boolean",
                    "description": "Mount rootfs read-only",
                    "default": False,
                },
                "network": {
                    "type": "string",
                    "enum": ["none", "allowlist", "open"],
                    "description": "Network policy",
                    "default": "none",
                },
                "network_allowlist": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Allowed outbound destinations",
                },
                "ttl_seconds": {
                    "type": "integer",
                    "description": "Time-to-live in seconds",
                },
                "idle_timeout_seconds": {
                    "type": "integer",
                    "description": "Idle timeout in seconds",
                },
                "labels": {
                    "type": "object",
                    "description": "User labels",
                },
            },
            "required": [],
        },
    ),
    "sandbox_list": Tool(
        name="sandbox_list",
        description="List all Nucleus sandboxes.",
        inputSchema={"type": "object", "properties": {}},
    ),
    "sandbox_get": Tool(
        name="sandbox_get",
        description="Get details of a sandbox.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                }
            },
            "required": ["sandbox_id"],
        },
    ),
    "sandbox_start": Tool(
        name="sandbox_start",
        description="Start a sandbox.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                }
            },
            "required": ["sandbox_id"],
        },
    ),
    "sandbox_stop": Tool(
        name="sandbox_stop",
        description="Stop a sandbox.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                }
            },
            "required": ["sandbox_id"],
        },
    ),
    "sandbox_destroy": Tool(
        name="sandbox_destroy",
        description="Destroy a sandbox and reclaim resources.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                }
            },
            "required": ["sandbox_id"],
        },
    ),
    "sandbox_exec": Tool(
        name="sandbox_exec",
        description="Execute a command in a sandbox.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                },
                "command": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Command and arguments",
                },
                "env": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Environment variables (KEY=VALUE)",
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory",
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Timeout in seconds",
                },
                "output_cap_bytes": {
                    "type": "integer",
                    "description": "Maximum output bytes",
                },
            },
            "required": ["sandbox_id", "command"],
        },
    ),
    "sandbox_read_file": Tool(
        name="sandbox_read_file",
        description="Read a file from a sandbox.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                },
                "path": {
                    "type": "string",
                    "description": "File path inside sandbox",
                },
            },
            "required": ["sandbox_id", "path"],
        },
    ),
    "sandbox_write_file": Tool(
        name="sandbox_write_file",
        description="Write a file to a sandbox.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                },
                "path": {
                    "type": "string",
                    "description": "Destination path inside sandbox",
                },
                "content": {
                    "type": "string",
                    "description": "File content",
                },
                "mode": {
                    "type": "integer",
                    "description": "File mode (e.g. 0o755)",
                },
            },
            "required": ["sandbox_id", "path", "content"],
        },
    ),
    "sandbox_list_files": Tool(
        name="sandbox_list_files",
        description="List files in a directory inside a sandbox.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                },
                "path": {
                    "type": "string",
                    "description": "Directory path (default: /)",
                    "default": "/",
                },
            },
            "required": ["sandbox_id"],
        },
    ),
    "sandbox_snapshot": Tool(
        name="sandbox_snapshot",
        description="Create a snapshot of a sandbox.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                },
                "name": {
                    "type": "string",
                    "description": "Snapshot name (optional)",
                },
            },
            "required": ["sandbox_id"],
        },
    ),
    "sandbox_reset": Tool(
        name="sandbox_reset",
        description="Restore a sandbox from a snapshot.",
        inputSchema={
            "type": "object",
            "properties": {
                "sandbox_id": {
                    "type": "string",
                    "description": "Sandbox ID",
                },
                "snapshot_name": {
                    "type": "string",
                    "description": "Snapshot name to restore",
                },
            },
            "required": ["sandbox_id", "snapshot_name"],
        },
    ),
}


async def _handle_list_tools(
    ctx: ServerRequestContext, params: PaginatedRequestParams | None
) -> ListToolsResult:
    return ListToolsResult(tools=list(_TOOL_DEFS.values()))


async def _handle_call_tool(
    ctx: ServerRequestContext, params: CallToolRequestParams
) -> Any:
    name = params.name
    arguments = params.arguments or {}

    try:
        if name == "sandbox_create":
            result = await client.create_sandbox(arguments)
        elif name == "sandbox_list":
            result = await client.list_sandboxes()
        elif name == "sandbox_get":
            result = await client.get_sandbox(arguments["sandbox_id"])
        elif name == "sandbox_start":
            result = await client.start_sandbox(arguments["sandbox_id"])
        elif name == "sandbox_stop":
            result = await client.stop_sandbox(arguments["sandbox_id"])
        elif name == "sandbox_destroy":
            await client.destroy_sandbox(arguments["sandbox_id"])
            result = {"status": "destroyed"}
        elif name == "sandbox_exec":
            result = await client.exec_sandbox(
                arguments["sandbox_id"],
                arguments["command"],
                env=arguments.get("env"),
                workdir=arguments.get("workdir"),
                timeout_seconds=arguments.get("timeout_seconds"),
                output_cap_bytes=arguments.get("output_cap_bytes"),
            )
        elif name == "sandbox_read_file":
            result = await client.read_file(
                arguments["sandbox_id"], arguments["path"]
            )
        elif name == "sandbox_write_file":
            await client.write_file(
                arguments["sandbox_id"],
                arguments["path"],
                arguments["content"],
                mode=arguments.get("mode"),
            )
            result = {"status": "written"}
        elif name == "sandbox_list_files":
            result = await client.list_files(
                arguments["sandbox_id"], arguments.get("path", "/")
            )
        elif name == "sandbox_snapshot":
            result = await client.snapshot_sandbox(
                arguments["sandbox_id"], arguments.get("name")
            )
        elif name == "sandbox_reset":
            result = await client.reset_sandbox(
                arguments["sandbox_id"], arguments["snapshot_name"]
            )
        else:
            raise ValueError(f"Unknown tool: {name}")

        return [
            TextContent(
                type="text",
                text=json.dumps(result, indent=2, default=str),
            )
        ]
    except Exception as exc:
        return [
            TextContent(
                type="text",
                text=json.dumps({"error": str(exc)}, indent=2),
            )
        ]


server = Server(
    "nucleus-sandbox",
    on_list_tools=_handle_list_tools,
    on_call_tool=_handle_call_tool,
)


async def main() -> None:
    async with stdio_server() as (read_stream, write_stream):
        await server.run(
            read_stream,
            write_stream,
            server.create_initialization_options(),
        )


if __name__ == "__main__":
    asyncio.run(main())

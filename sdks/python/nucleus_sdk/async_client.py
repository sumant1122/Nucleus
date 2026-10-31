"""Async client for the Nucleus sandbox API."""

from __future__ import annotations

from typing import Any, AsyncIterator

import httpx

from .exceptions import (
    NucleusConnectionError,
    NucleusError,
    NucleusTimeoutError,
    map_http_error,
)
from .models import (
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
)


class AsyncNucleusClient:
    """Async client for the Nucleus sandbox daemon.

    Example:

        async with AsyncNucleusClient("http://127.0.0.1:8080") as client:
            sandbox = await client.create_sandbox(SandboxSpec(image="alpine"))
            result = await client.exec_sandbox(sandbox.id, ["ls", "-la"])
            print(result.stdout)
            await client.destroy_sandbox(sandbox.id)
    """

    def __init__(
        self,
        base_url: str = "http://127.0.0.1:8080",
        timeout: float = 30.0,
        headers: dict[str, str] | None = None,
    ):
        self.base_url = base_url.rstrip("/")
        self._client = httpx.AsyncClient(timeout=timeout, headers=headers or {})

    async def __aenter__(self) -> AsyncNucleusClient:
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        """Close the underlying HTTP client."""
        await self._client.aclose()

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

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
            raise NucleusConnectionError(
                f"Cannot connect to Nucleus daemon at {self.base_url}"
            ) from exc
        except httpx.TimeoutException as exc:
            raise NucleusTimeoutError(f"Request timed out: {method} {path}") from exc
        except httpx.RequestError as exc:
            raise NucleusConnectionError(str(exc)) from exc

        if response.status_code >= 400:
            detail: str | None = None
            try:
                body = response.json()
                detail = body.get("detail")
            except Exception:
                detail = response.text
            raise map_http_error(response.status_code, response.text, detail)

        if response.status_code == 204:
            return None

        return response.json()

    # ------------------------------------------------------------------
    # Sandbox lifecycle
    # ------------------------------------------------------------------

    async def create_sandbox(self, spec: SandboxSpec) -> Sandbox:
        """Create a new sandbox."""
        data = await self._request("POST", "/sandboxes", json=spec.to_dict())
        return Sandbox.from_dict(data)

    async def list_sandboxes(self) -> ListSandboxesResponse:
        """List all sandboxes."""
        data = await self._request("GET", "/sandboxes")
        return ListSandboxesResponse.from_dict(data)

    async def get_sandbox(self, sandbox_id: str) -> Sandbox:
        """Get a sandbox by ID."""
        data = await self._request("GET", f"/sandboxes/{sandbox_id}")
        return Sandbox.from_dict(data)

    async def start_sandbox(self, sandbox_id: str) -> Sandbox:
        """Start a sandbox."""
        data = await self._request("POST", f"/sandboxes/{sandbox_id}/start")
        return Sandbox.from_dict(data)

    async def stop_sandbox(self, sandbox_id: str) -> Sandbox:
        """Stop a sandbox."""
        data = await self._request("POST", f"/sandboxes/{sandbox_id}/stop")
        return Sandbox.from_dict(data)

    async def destroy_sandbox(self, sandbox_id: str) -> None:
        """Destroy a sandbox and reclaim all resources."""
        await self._request("DELETE", f"/sandboxes/{sandbox_id}")

    # ------------------------------------------------------------------
    # Exec
    # ------------------------------------------------------------------

    async def exec_sandbox(
        self,
        sandbox_id: str,
        command: list[str],
        *,
        env: list[str] | None = None,
        workdir: str | None = None,
        timeout_seconds: int | None = None,
        output_cap_bytes: int | None = None,
    ) -> ExecResult:
        """Execute a command in a sandbox and wait for completion."""
        request = ExecRequest(
            command=command,
            env=env,
            workdir=workdir,
            timeout_seconds=timeout_seconds,
            output_cap_bytes=output_cap_bytes,
        )
        data = await self._request(
            "POST", f"/sandboxes/{sandbox_id}/exec", json=request.to_dict()
        )
        return ExecResult.from_dict(data)

    async def exec_stream(
        self,
        sandbox_id: str,
        command: list[str],
        *,
        env: list[str] | None = None,
        workdir: str | None = None,
        timeout_seconds: int | None = None,
        output_cap_bytes: int | None = None,
    ) -> AsyncIterator[ExecEvent]:
        """Execute a command and stream output events asynchronously.

        Yields ExecEvent objects for stdout, stderr, and exit.
        """
        request = StreamExecRequest(
            command=command,
            env=env,
            workdir=workdir,
            timeout_seconds=timeout_seconds,
            output_cap_bytes=output_cap_bytes,
        )
        url = f"{self.base_url}/sandboxes/{sandbox_id}/exec/stream"
        try:
            async with self._client.stream(
                "POST", url, json=request.to_dict()
            ) as response:
                if response.status_code >= 400:
                    detail: str | None = None
                    try:
                        body = response.json()
                        detail = body.get("detail")
                    except Exception:
                        detail = response.text
                    raise map_http_error(
                        response.status_code, response.text, detail
                    )
                async for line in response.aiter_lines():
                    if not line:
                        continue
                    if line.startswith("data: "):
                        import json

                        payload = json.loads(line[6:])
                        yield ExecEvent.from_dict(payload)
        except httpx.ConnectError as exc:
            raise NucleusConnectionError(
                f"Cannot connect to Nucleus daemon at {self.base_url}"
            ) from exc
        except httpx.TimeoutException as exc:
            raise NucleusTimeoutError(
                f"Streaming exec timed out: {sandbox_id}"
            ) from exc

    # ------------------------------------------------------------------
    # Files
    # ------------------------------------------------------------------

    async def read_file(self, sandbox_id: str, path: str) -> bytes:
        """Read a file from a sandbox.

        Returns the raw file contents as bytes.
        """
        data = await self._request(
            "GET", f"/sandboxes/{sandbox_id}/files", params={"path": path}
        )
        content = data.get("content", b"")
        if isinstance(content, str):
            return content.encode()
        return content

    async def write_file(
        self,
        sandbox_id: str,
        path: str,
        content: bytes | str,
        *,
        mode: int | None = None,
    ) -> None:
        """Write a file to a sandbox."""
        if isinstance(content, str):
            content_bytes = content.encode()
        else:
            content_bytes = content

        params: dict[str, Any] = {}
        if mode is not None:
            params["mode"] = oct(mode)

        await self._request(
            "PUT",
            f"/sandboxes/{sandbox_id}/files",
            params=params,
            json={"path": path, "content": list(content_bytes)},
        )

    async def list_files(
        self, sandbox_id: str, path: str = "/"
    ) -> ListFilesResponse:
        """List files in a directory inside a sandbox."""
        data = await self._request(
            "GET",
            f"/sandboxes/{sandbox_id}/files",
            params={"path": path, "list": "true"},
        )
        return ListFilesResponse.from_dict(data)

    # ------------------------------------------------------------------
    # Snapshots
    # ------------------------------------------------------------------

    async def snapshot_sandbox(
        self, sandbox_id: str, name: str | None = None
    ) -> SnapshotResponse:
        """Create a snapshot of a sandbox's overlay filesystem."""
        params: dict[str, Any] = {}
        if name is not None:
            params["name"] = name
        data = await self._request(
            "POST", f"/sandboxes/{sandbox_id}/snapshot", params=params
        )
        return SnapshotResponse.from_dict(data)

    async def reset_sandbox(
        self, sandbox_id: str, snapshot_name: str
    ) -> SnapshotResponse:
        """Restore a sandbox from a snapshot."""
        data = await self._request(
            "POST",
            f"/sandboxes/{sandbox_id}/reset",
            params={"name": snapshot_name},
        )
        return SnapshotResponse.from_dict(data)

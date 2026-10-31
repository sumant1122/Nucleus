"""Synchronous client for the Nucleus sandbox API."""

from __future__ import annotations

from typing import Any, Iterator

import httpx

from .exceptions import (
    NucleusConnectionError,
    NucleusError,
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


class NucleusClient:
    """Synchronous client for the Nucleus sandbox daemon.

    Example:

        client = NucleusClient("http://127.0.0.1:8080")
        sandbox = client.create_sandbox(SandboxSpec(image="alpine"))
        result = client.exec_sandbox(sandbox.id, ["ls", "-la"])
        print(result.stdout)
        client.destroy_sandbox(sandbox.id)
    """

    def __init__(
        self,
        base_url: str = "http://127.0.0.1:8080",
        timeout: float = 30.0,
        headers: dict[str, str] | None = None,
    ):
        self.base_url = base_url.rstrip("/")
        self._client = httpx.Client(timeout=timeout, headers=headers or {})

    def __enter__(self) -> NucleusClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()

    def close(self) -> None:
        """Close the underlying HTTP client."""
        self._client.close()

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _request(
        self,
        method: str,
        path: str,
        *,
        params: dict[str, Any] | None = None,
        json: dict[str, Any] | None = None,
    ) -> Any:
        url = f"{self.base_url}{path}"
        try:
            response = self._client.request(method, url, params=params, json=json)
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

    def create_sandbox(self, spec: SandboxSpec) -> Sandbox:
        """Create a new sandbox.

        Args:
            spec: Sandbox specification.

        Returns:
            The created sandbox.

        Raises:
            NucleusConflictError: If a sandbox with the same name already exists.
            NucleusAPIError: On other API errors.
        """
        data = self._request("POST", "/sandboxes", json=spec.to_dict())
        return Sandbox.from_dict(data)

    def list_sandboxes(self) -> ListSandboxesResponse:
        """List all sandboxes."""
        data = self._request("GET", "/sandboxes")
        return ListSandboxesResponse.from_dict(data)

    def get_sandbox(self, sandbox_id: str) -> Sandbox:
        """Get a sandbox by ID.

        Raises:
            NucleusNotFoundError: If the sandbox does not exist.
        """
        data = self._request("GET", f"/sandboxes/{sandbox_id}")
        return Sandbox.from_dict(data)

    def start_sandbox(self, sandbox_id: str) -> Sandbox:
        """Start a sandbox.

        Raises:
            NucleusNotFoundError: If the sandbox does not exist.
        """
        data = self._request("POST", f"/sandboxes/{sandbox_id}/start")
        return Sandbox.from_dict(data)

    def stop_sandbox(self, sandbox_id: str) -> Sandbox:
        """Stop a sandbox.

        Raises:
            NucleusNotFoundError: If the sandbox does not exist.
        """
        data = self._request("POST", f"/sandboxes/{sandbox_id}/stop")
        return Sandbox.from_dict(data)

    def destroy_sandbox(self, sandbox_id: str) -> None:
        """Destroy a sandbox and reclaim all resources.

        Raises:
            NucleusNotFoundError: If the sandbox does not exist.
        """
        self._request("DELETE", f"/sandboxes/{sandbox_id}")

    # ------------------------------------------------------------------
    # Exec
    # ------------------------------------------------------------------

    def exec_sandbox(
        self,
        sandbox_id: str,
        command: list[str],
        *,
        env: list[str] | None = None,
        workdir: str | None = None,
        timeout_seconds: int | None = None,
        output_cap_bytes: int | None = None,
    ) -> ExecResult:
        """Execute a command in a sandbox and wait for completion.

        Args:
            sandbox_id: ID of the sandbox.
            command: Command and arguments to execute.
            env: Environment variables as KEY=VALUE strings.
            workdir: Working directory inside the sandbox.
            timeout_seconds: Per-command timeout in seconds.
            output_cap_bytes: Maximum output bytes before truncation.

        Returns:
            ExecResult with exit_code, stdout, stderr, duration_ms.
        """
        request = ExecRequest(
            command=command,
            env=env,
            workdir=workdir,
            timeout_seconds=timeout_seconds,
            output_cap_bytes=output_cap_bytes,
        )
        data = self._request(
            "POST", f"/sandboxes/{sandbox_id}/exec", json=request.to_dict()
        )
        return ExecResult.from_dict(data)

    def exec_stream(
        self,
        sandbox_id: str,
        command: list[str],
        *,
        env: list[str] | None = None,
        workdir: str | None = None,
        timeout_seconds: int | None = None,
        output_cap_bytes: int | None = None,
    ) -> Iterator[ExecEvent]:
        """Execute a command and stream output events.

        Yields ExecEvent objects for stdout, stderr, and exit.

        Note: This endpoint uses SSE (Server-Sent Events).
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
            with self._client.stream(
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
                for line in response.iter_lines():
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

    def read_file(self, sandbox_id: str, path: str) -> bytes:
        """Read a file from a sandbox.

        Returns the raw file contents as bytes.
        """
        data = self._request(
            "GET", f"/sandboxes/{sandbox_id}/files", params={"path": path}
        )
        return data.get("content", b"").encode() if isinstance(data.get("content"), str) else data.get("content", b"")

    def write_file(
        self,
        sandbox_id: str,
        path: str,
        content: bytes | str,
        *,
        mode: int | None = None,
    ) -> None:
        """Write a file to a sandbox.

        Args:
            sandbox_id: ID of the sandbox.
            path: Destination path inside the sandbox.
            content: File content as bytes or string.
            mode: Optional file mode (e.g. 0o755).
        """
        if isinstance(content, str):
            content_bytes = content.encode()
        else:
            content_bytes = content

        params: dict[str, Any] = {}
        if mode is not None:
            params["mode"] = oct(mode)

        self._request(
            "PUT",
            f"/sandboxes/{sandbox_id}/files",
            params=params,
            json={"path": path, "content": list(content_bytes)},
        )

    def list_files(
        self, sandbox_id: str, path: str = "/"
    ) -> ListFilesResponse:
        """List files in a directory inside a sandbox.

        Args:
            sandbox_id: ID of the sandbox.
            path: Directory path inside the sandbox (default: "/").
        """
        data = self._request(
            "GET",
            f"/sandboxes/{sandbox_id}/files",
            params={"path": path, "list": "true"},
        )
        return ListFilesResponse.from_dict(data)

    # ------------------------------------------------------------------
    # Snapshots
    # ------------------------------------------------------------------

    def snapshot_sandbox(
        self, sandbox_id: str, name: str | None = None
    ) -> SnapshotResponse:
        """Create a snapshot of a sandbox's overlay filesystem.

        Args:
            sandbox_id: ID of the sandbox.
            name: Optional snapshot name (auto-generated if omitted).
        """
        params: dict[str, Any] = {}
        if name is not None:
            params["name"] = name
        data = self._request("POST", f"/sandboxes/{sandbox_id}/snapshot", params=params)
        return SnapshotResponse.from_dict(data)

    def reset_sandbox(self, sandbox_id: str, snapshot_name: str) -> SnapshotResponse:
        """Restore a sandbox from a snapshot.

        Args:
            sandbox_id: ID of the sandbox.
            snapshot_name: Name of the snapshot to restore.
        """
        data = self._request(
            "POST",
            f"/sandboxes/{sandbox_id}/reset",
            params={"name": snapshot_name},
        )
        return SnapshotResponse.from_dict(data)

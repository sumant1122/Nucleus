"""Data models for the Nucleus SDK."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any


@dataclass
class SandboxSpec:
    """Specification for creating a sandbox."""

    image: str = "alpine"
    command: list[str] | None = None
    env: list[str] | None = None
    workdir: str | None = None
    memory: str | None = None
    cpus: float | None = None
    pids_limit: int | None = None
    rootless: bool = False
    readonly: bool = False
    network: str = "none"  # "none", "allowlist", "open"
    network_allowlist: list[str] | None = None
    ttl_seconds: int | None = None
    idle_timeout_seconds: int | None = None
    labels: dict[str, str] | None = None

    def to_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {"image": self.image}
        if self.command is not None:
            data["command"] = self.command
        if self.env is not None:
            data["env"] = self.env
        if self.workdir is not None:
            data["workdir"] = self.workdir
        if self.memory is not None:
            data["memory"] = self.memory
        if self.cpus is not None:
            data["cpus"] = self.cpus
        if self.pids_limit is not None:
            data["pids_limit"] = self.pids_limit
        data["rootless"] = self.rootless
        data["readonly"] = self.readonly
        data["network"] = self.network
        if self.network_allowlist is not None:
            data["network_allowlist"] = self.network_allowlist
        if self.ttl_seconds is not None:
            data["ttl_seconds"] = self.ttl_seconds
        if self.idle_timeout_seconds is not None:
            data["idle_timeout_seconds"] = self.idle_timeout_seconds
        if self.labels is not None:
            data["labels"] = self.labels
        return data


@dataclass
class Sandbox:
    """Represents a sandbox instance."""

    id: str
    image: str
    status: str
    pid: int | None = None
    ip: str | None = None
    memory: str | None = None
    cpus: float | None = None
    pids_limit: int | None = None
    network: str = "none"
    created_at: datetime | None = None
    started_at: datetime | None = None
    stopped_at: datetime | None = None
    exit_code: int | None = None
    ttl_seconds: int | None = None
    idle_timeout_seconds: int | None = None
    labels: dict[str, str] | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Sandbox:
        def _parse_dt(value: Any) -> datetime | None:
            if value is None:
                return None
            if isinstance(value, str):
                return datetime.fromisoformat(value.replace("Z", "+00:00"))
            return None

        return cls(
            id=data["id"],
            image=data["image"],
            status=data["status"],
            pid=data.get("pid"),
            ip=data.get("ip"),
            memory=data.get("memory"),
            cpus=data.get("cpus"),
            pids_limit=data.get("pids_limit"),
            network=data.get("network", "none"),
            created_at=_parse_dt(data.get("created_at")),
            started_at=_parse_dt(data.get("started_at")),
            stopped_at=_parse_dt(data.get("stopped_at")),
            exit_code=data.get("exit_code"),
            ttl_seconds=data.get("ttl_seconds"),
            idle_timeout_seconds=data.get("idle_timeout_seconds"),
            labels=data.get("labels"),
        )


@dataclass
class ExecRequest:
    """Request to execute a command in a sandbox."""

    command: list[str]
    env: list[str] | None = None
    workdir: str | None = None
    timeout_seconds: int | None = None
    output_cap_bytes: int | None = None
    stdin: str | None = None

    def to_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {"command": self.command}
        if self.env is not None:
            data["env"] = self.env
        if self.workdir is not None:
            data["workdir"] = self.workdir
        if self.timeout_seconds is not None:
            data["timeout_seconds"] = self.timeout_seconds
        if self.output_cap_bytes is not None:
            data["output_cap_bytes"] = self.output_cap_bytes
        if self.stdin is not None:
            data["stdin"] = self.stdin
        return data


@dataclass
class ExecResult:
    """Result of executing a command in a sandbox."""

    exit_code: int
    stdout: str
    stderr: str
    duration_ms: int
    truncated: bool

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ExecResult:
        return cls(
            exit_code=data["exit_code"],
            stdout=data.get("stdout", ""),
            stderr=data.get("stderr", ""),
            duration_ms=data.get("duration_ms", 0),
            truncated=data.get("truncated", False),
        )


@dataclass
class StreamExecRequest:
    """Request to stream-execute a command in a sandbox."""

    command: list[str]
    env: list[str] | None = None
    workdir: str | None = None
    timeout_seconds: int | None = None
    output_cap_bytes: int | None = None

    def to_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {"command": self.command}
        if self.env is not None:
            data["env"] = self.env
        if self.workdir is not None:
            data["workdir"] = self.workdir
        if self.timeout_seconds is not None:
            data["timeout_seconds"] = self.timeout_seconds
        if self.output_cap_bytes is not None:
            data["output_cap_bytes"] = self.output_cap_bytes
        return data


@dataclass
class ExecEvent:
    """An event from streaming exec."""

    type: str  # "stdout", "stderr", "exit"
    data: str

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ExecEvent:
        return cls(type=data["type"], data=data.get("data", ""))


@dataclass
class FileEntry:
    """A file or directory entry in a sandbox."""

    name: str
    path: str
    is_dir: bool
    size: int

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> FileEntry:
        return cls(
            name=data["name"],
            path=data["path"],
            is_dir=data.get("is_dir", False),
            size=data.get("size", 0),
        )


@dataclass
class ListFilesResponse:
    """Response for listing files in a sandbox."""

    path: str
    entries: list[FileEntry]

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ListFilesResponse:
        return cls(
            path=data["path"],
            entries=[FileEntry.from_dict(e) for e in data.get("entries", [])],
        )


@dataclass
class SnapshotResponse:
    """Response for snapshot/reset operations."""

    snapshot_id: str
    name: str
    created_at: str

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> SnapshotResponse:
        return cls(
            snapshot_id=data["snapshot_id"],
            name=data["name"],
            created_at=data["created_at"],
        )


@dataclass
class ListSandboxesResponse:
    """Response for listing sandboxes."""

    sandboxes: list[Sandbox]

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ListSandboxesResponse:
        return cls(
            sandboxes=[Sandbox.from_dict(s) for s in data.get("sandboxes", [])],
        )

"""Nucleus SDK exceptions."""


class NucleusError(Exception):
    """Base exception for all Nucleus SDK errors."""


class NucleusAPIError(NucleusError):
    """Raised when the Nucleus API returns an error response."""

    def __init__(self, status_code: int, message: str, detail: str | None = None):
        self.status_code = status_code
        self.message = message
        self.detail = detail
        super().__init__(f"API error {status_code}: {message}")


class NucleusNotFoundError(NucleusError):
    """Raised when a sandbox is not found."""


class NucleusConflictError(NucleusError):
    """Raised when a sandbox already exists."""


class NucleusTimeoutError(NucleusError):
    """Raised when an operation times out."""


class NucleusConnectionError(NucleusError):
    """Raised when the SDK cannot connect to the Nucleus daemon."""


def map_http_error(status_code: int, message: str, detail: str | None = None) -> NucleusError:
    """Map an HTTP status code to the appropriate exception type."""
    if status_code == 404:
        return NucleusNotFoundError(message)
    if status_code == 409:
        return NucleusConflictError(message)
    if status_code == 408:
        return NucleusTimeoutError(message)
    if status_code >= 500:
        return NucleusAPIError(status_code, message, detail)
    return NucleusAPIError(status_code, message, detail)

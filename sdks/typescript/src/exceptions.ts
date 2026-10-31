/** Nucleus SDK exceptions. */

export class NucleusError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "NucleusError";
  }
}

export class NucleusAPIError extends NucleusError {
  statusCode: number;
  detail?: string;

  constructor(statusCode: number, message: string, detail?: string) {
    super(`API error ${statusCode}: ${message}`);
    this.name = "NucleusAPIError";
    this.statusCode = statusCode;
    this.detail = detail;
  }
}

export class NucleusNotFoundError extends NucleusError {
  constructor(message: string) {
    super(message);
    this.name = "NucleusNotFoundError";
  }
}

export class NucleusConflictError extends NucleusError {
  constructor(message: string) {
    super(message);
    this.name = "NucleusConflictError";
  }
}

export class NucleusTimeoutError extends NucleusError {
  constructor(message: string) {
    super(message);
    this.name = "NucleusTimeoutError";
  }
}

export class NucleusConnectionError extends NucleusError {
  constructor(message: string) {
    super(message);
    this.name = "NucleusConnectionError";
  }
}

export function mapHttpError(
  statusCode: number,
  message: string,
  detail?: string
): NucleusError {
  if (statusCode === 404) return new NucleusNotFoundError(message);
  if (statusCode === 409) return new NucleusConflictError(message);
  if (statusCode === 408) return new NucleusTimeoutError(message);
  if (statusCode >= 500)
    return new NucleusAPIError(statusCode, message, detail);
  return new NucleusAPIError(statusCode, message, detail);
}

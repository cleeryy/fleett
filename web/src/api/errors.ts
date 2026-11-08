export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  /** `error.details` from the controller, when the refusal carries structured data. */
  readonly details: unknown;

  constructor(status: number, code: string, message: string, details?: unknown) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.details = details;
  }
}

export function toApiError(error: unknown): ApiError {
  if (error instanceof ApiError) return error;
  if (error instanceof TypeError) {
    return new ApiError(0, "network_error", "Controller unreachable. Check that the backend is running.");
  }
  if (error instanceof Error) {
    return new ApiError(0, "client_error", error.message);
  }
  return new ApiError(0, "client_error", "Unexpected error.");
}

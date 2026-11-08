export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
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

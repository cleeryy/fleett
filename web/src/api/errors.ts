import { hasTranslation } from "@/lib/i18n";

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

type Translate = (key: string, params?: Record<string, string | number>) => string;

/**
 * Human message for an API failure.
 *
 * 1. a known `error.code` gets the translated `errors.<code>` copy — errors are
 *    read under stress, so the wording matters more than the server's;
 * 2. an unknown code keeps the controller's message (English);
 * 3. no code at all falls back to the generic `errors.unknown`.
 */
export function apiErrorMessage(error: unknown, t: Translate): string {
  const apiError = error instanceof ApiError ? error : toApiError(error);
  if (apiError.code && hasTranslation(`errors.${apiError.code}`)) {
    return t(`errors.${apiError.code}`);
  }
  const message = apiError.message?.trim();
  if (message) return message;
  return t("errors.unknown");
}

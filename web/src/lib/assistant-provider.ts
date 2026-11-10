import type { ApiError } from "@/api/errors";
import type {
  AssistantConfig,
  AssistantProvider,
  AssistantProviderReason,
  AssistantProviderSource,
  ProviderSummary,
} from "@/api/types";

/**
 * The assistant has four provider situations, and they need different copy:
 *
 * - `resolved`: a provider answers (`env`, `setting` or `auto`);
 * - `not_selected`: providers exist, none is chosen for the assistant;
 * - `no_provider`: the fleet has no provider at all (real first run);
 * - `unknown`: the configured id no longer exists (`422`), so it has to be
 *   repaired by picking another one.
 *
 * Collapsing the middle two into "no provider" was the bug reported in #63.
 */
export type AssistantProviderState =
  | { kind: "resolved"; provider: AssistantProvider; source: AssistantProviderSource | null }
  | { kind: "not_selected" }
  | { kind: "no_provider" }
  | { kind: "unknown"; valid: string[] };

/** Provider ids carried by `422 assistant_provider_unknown` (`details.valid`). */
export function assistantValidIds(error: ApiError | null): string[] {
  const details = error?.details;
  if (!details || typeof details !== "object") return [];
  const valid = (details as { valid?: unknown }).valid;
  if (!Array.isArray(valid)) return [];
  return valid.filter((id): id is string => typeof id === "string");
}

/**
 * Derive the panel state from the config, the config error and the provider
 * list. The list only matters for controllers older than 0.9, which return
 * `provider: null` without a `reason`: the count then decides between the two
 * empty states instead of repeating the "no provider" message.
 */
export function assistantProviderState(
  config: AssistantConfig | null,
  error: ApiError | null,
  providers: ProviderSummary[],
): AssistantProviderState | null {
  if (error?.code === "assistant_provider_unknown") {
    return { kind: "unknown", valid: assistantValidIds(error) };
  }
  if (!config) return null;
  if (config.provider) {
    return {
      kind: "resolved",
      provider: config.provider,
      source: config.provider_source ?? null,
    };
  }

  const reason: AssistantProviderReason | null | undefined = config.reason;
  if (reason === "provider_deleted") {
    // A controller that reports a stale reference in `reason` still lets the
    // console repair it with the providers that do exist.
    return { kind: "unknown", valid: providers.map((provider) => provider.id) };
  }
  if (reason === "not_selected") return { kind: "not_selected" };
  if (reason === "no_provider") return { kind: "no_provider" };
  return providers.length > 0 ? { kind: "not_selected" } : { kind: "no_provider" };
}

/** Whether the state is still waiting for the provider list to be fetched. */
export function assistantNeedsProviders(config: AssistantConfig | null): boolean {
  return config !== null && config.provider === null && !config.reason;
}

/** Small label next to the provider chip: "env", "selected", "auto". */
export const PROVIDER_SOURCE_KEY: Record<Exclude<AssistantProviderSource, "none">, string> = {
  env: "assistant.sourceEnv",
  setting: "assistant.sourceSetting",
  auto: "assistant.sourceAuto",
};

/** Tooltip explaining how the id was resolved. */
export const PROVIDER_SOURCE_HINT_KEY: Record<
  Exclude<AssistantProviderSource, "none">,
  string
> = {
  env: "assistant.sourceEnvHint",
  setting: "assistant.sourceSettingHint",
  auto: "assistant.sourceAutoHint",
};

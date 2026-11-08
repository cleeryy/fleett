import { useCallback, useEffect, useRef, useState } from "react";
import { toApiError, type ApiError } from "@/api/errors";

export interface Resource<T> {
  data: T | null;
  error: ApiError | null;
  loading: boolean;
  refreshing: boolean;
  reload: () => void;
  setData: (updater: (previous: T | null) => T | null) => void;
}

/**
 * Minimal async resource hook: one in-flight request wins, previous data stays
 * visible while refreshing, errors are normalised to ApiError.
 */
export function useResource<T>(loader: () => Promise<T>, deps: unknown[]): Resource<T> {
  const [state, setState] = useState<{
    data: T | null;
    error: ApiError | null;
    loading: boolean;
    refreshing: boolean;
  }>({ data: null, error: null, loading: true, refreshing: false });

  const sequence = useRef(0);
  const loaderRef = useRef(loader);
  loaderRef.current = loader;

  const reload = useCallback(() => {
    const id = ++sequence.current;
    setState((previous) => ({
      data: previous.data,
      error: null,
      loading: previous.data === null,
      refreshing: previous.data !== null,
    }));
    loaderRef.current().then(
      (data) => {
        if (sequence.current !== id) return;
        setState({ data, error: null, loading: false, refreshing: false });
      },
      (error: unknown) => {
        if (sequence.current !== id) return;
        setState((previous) => ({
          data: previous.data,
          error: toApiError(error),
          loading: false,
          refreshing: false,
        }));
      },
    );
  }, []);

  const setData = useCallback((updater: (previous: T | null) => T | null) => {
    setState((previous) => ({ ...previous, data: updater(previous.data) }));
  }, []);

  useEffect(() => {
    reload();
    return () => {
      sequence.current += 1;
    };
    // The caller owns the dependency list; reload is stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reload, ...deps]);

  return { ...state, reload, setData };
}

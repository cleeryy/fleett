import { useEffect, useRef, useState, type FormEvent } from "react";
import { Boxes, Eye, EyeOff, Loader2 } from "lucide-react";
import { api, DEMO_TOKEN, isMock } from "@/api";
import { ApiError, toApiError } from "@/api/errors";
import { tokenStore } from "@/lib/token";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

export function LoginPage({ onSignedIn }: { onSignedIn: () => void }) {
  const [token, setToken] = useState("");
  const [reveal, setReveal] = useState(false);
  const [pending, setPending] = useState(false);
  const [fieldError, setFieldError] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = token.trim();
    if (!value) {
      setFieldError("Enter the access token.");
      inputRef.current?.focus();
      return;
    }

    setPending(true);
    setFieldError(null);
    setError(null);
    tokenStore.set(value);
    try {
      await api.checkAuth();
      onSignedIn();
    } catch (cause) {
      tokenStore.clear();
      const failure = toApiError(cause);
      setError(
        failure.status === 401
          ? new ApiError(401, failure.code, "Token rejected. Check the controller token.")
          : failure,
      );
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="flex min-h-dvh flex-col items-center justify-center bg-background px-4 py-10">
      <div className="w-full max-w-sm">
        <div className="mb-8 flex items-center gap-2">
          <div className="flex size-8 items-center justify-center rounded-md bg-primary text-primary-foreground">
            <Boxes aria-hidden className="size-4" />
          </div>
          <span className="font-semibold tracking-tight">fleett</span>
        </div>

        <h1 className="text-2xl font-semibold tracking-tight">Sign in</h1>
        <p className="mt-1 text-sm text-muted-foreground">Access token for the control plane.</p>

        <form className="mt-6 space-y-4" onSubmit={onSubmit} noValidate>
          <div className="space-y-2">
            <Label htmlFor="token">Access token</Label>
            <div className="relative">
              <Input
                ref={inputRef}
                id="token"
                name="token"
                type={reveal ? "text" : "password"}
                value={token}
                autoComplete="off"
                spellCheck={false}
                className="pr-11 font-mono"
                placeholder="FLEETT_TOKEN"
                aria-invalid={!!fieldError || !!error}
                aria-describedby={fieldError ? "token-error" : undefined}
                onChange={(event) => setToken(event.target.value)}
              />
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                className="absolute top-1/2 right-1 -translate-y-1/2"
                onClick={() => setReveal((value) => !value)}
                aria-label={reveal ? "Hide token" : "Show token"}
                title={reveal ? "Hide token" : "Show token"}
              >
                {reveal ? <EyeOff aria-hidden /> : <Eye aria-hidden />}
              </Button>
            </div>
            {fieldError ? (
              <p id="token-error" className="text-xs text-destructive">
                {fieldError}
              </p>
            ) : null}
          </div>

          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error.message}
            </p>
          ) : null}

          <Button type="submit" className="w-full" disabled={pending}>
            {pending ? <Loader2 aria-hidden className="animate-spin" /> : null}
            Sign in
          </Button>
        </form>

        <p className="mt-6 text-xs text-muted-foreground">
          Sent as <span className="font-mono">Authorization: Bearer</span>. Kept in sessionStorage for this tab.
        </p>
        {isMock ? (
          <p className="mt-2 text-xs text-muted-foreground">
            Mock mode. Use token <span className="font-mono text-foreground">{DEMO_TOKEN}</span>.
          </p>
        ) : null}
      </div>
    </div>
  );
}

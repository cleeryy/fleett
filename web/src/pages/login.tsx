import { useEffect, useRef, useState, type FormEvent } from "react";
import { Boxes, Eye, EyeOff, KeyRound, Loader2, ShieldCheck } from "lucide-react";
import { api, DEMO_TOKEN, DEMO_VIEWER_TOKEN, isMock } from "@/api";
import { ApiError, apiErrorMessage, toApiError } from "@/api/errors";
import type { AuthConfig, Identity } from "@/api/types";
import { tokenStore } from "@/lib/token";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ErrorAlert } from "@/components/error-alert";
import { LanguageToggle } from "@/components/language-toggle";
import { useI18n } from "@/lib/i18n";

export function LoginPage({
  config,
  bootError,
  onSignedIn,
}: {
  config: AuthConfig;
  /** A failed `/api/auth/me` on boot (network, not 401). */
  bootError: ApiError | null;
  onSignedIn: (identity: Identity) => void;
}) {
  const { t } = useI18n();
  const [token, setToken] = useState("");
  const [reveal, setReveal] = useState(false);
  const [tokenOpen, setTokenOpen] = useState(false);
  const [pending, setPending] = useState<"oidc" | "token" | null>(null);
  const [fieldError, setFieldError] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const oidc = config.oidc.enabled;
  const bearer = config.bearer.enabled;
  // `both` keeps the token form an opt-in, so SSO stays the obvious path.
  const tokenVisible = bearer && (!oidc || tokenOpen);

  useEffect(() => {
    if (tokenVisible) inputRef.current?.focus();
  }, [tokenVisible]);

  async function onOidc() {
    setPending("oidc");
    setError(null);
    tokenStore.clear();
    try {
      await api.signInWithOidc(config.oidc.login_url);
      // Only the mock adapter returns here; the real client navigates away.
      const identity = await api.me();
      onSignedIn(identity);
    } catch (cause) {
      setError(toApiError(cause));
      setPending(null);
    }
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = token.trim();
    if (!value) {
      setFieldError(t("login.enterToken"));
      inputRef.current?.focus();
      return;
    }

    setPending("token");
    setFieldError(null);
    setError(null);
    tokenStore.set(value);
    try {
      await api.checkAuth();
    } catch (cause) {
      tokenStore.clear();
      const failure = toApiError(cause);
      setError(
        failure.status === 401
          ? new ApiError(401, "token_rejected", t("login.tokenRejected"))
          : failure,
      );
      setPending(null);
      return;
    }

    try {
      onSignedIn(await api.me());
    } catch (cause) {
      tokenStore.clear();
      const failure = toApiError(cause);
      setError(
        failure.status === 401
          ? new ApiError(
              401,
              "token_no_session",
              t("login.tokenNoSession"),
            )
          : failure,
      );
    } finally {
      setPending(null);
    }
  }

  const description =
    oidc && bearer
      ? t("login.descriptionBoth")
      : oidc
        ? t("login.descriptionOidc")
        : t("login.descriptionToken");

  return (
    <div className="flex min-h-dvh flex-col items-center justify-center bg-background px-4 py-10">
      <div className="w-full max-w-sm">
        <div className="mb-8 flex items-center gap-2">
          <div className="flex items-center gap-2">
            <div className="flex size-8 items-center justify-center rounded-md bg-primary text-primary-foreground">
              <Boxes aria-hidden className="size-4" />
            </div>
            <span className="font-semibold tracking-tight">fleett</span>
          </div>
          <LanguageToggle className="ml-auto" />
        </div>

        <h1 className="text-2xl font-semibold tracking-tight">{t("login.title")}</h1>
        <p className="mt-1 text-sm text-muted-foreground">{description}</p>

        {bootError ? (
          <ErrorAlert
            error={bootError}
            onRetry={() => window.location.reload()}
            className="mt-6"
          />
        ) : null}

        {oidc ? (
          <Button
            type="button"
            className="mt-6 w-full"
            onClick={onOidc}
            disabled={pending !== null}
          >
            {pending === "oidc" ? (
              <Loader2 aria-hidden className="animate-spin" />
            ) : (
              <ShieldCheck aria-hidden />
            )}
            {t("login.signInWithOidc")}
          </Button>
        ) : null}

        {error && !tokenVisible ? (
          <p role="alert" className="mt-4 text-sm text-destructive">
            {apiErrorMessage(error, t)}
          </p>
        ) : null}

        {oidc && bearer ? (
          <div className="relative my-6" aria-hidden>
            <div className="absolute inset-0 flex items-center">
              <span className="w-full border-t" />
            </div>
            <div className="relative flex justify-center text-xs uppercase">
              <span className="bg-background px-2 text-muted-foreground">{t("login.or")}</span>
            </div>
          </div>
        ) : null}

        {bearer && !tokenVisible ? (
          <Button
            type="button"
            variant="outline"
            className="w-full"
            onClick={() => setTokenOpen(true)}
            disabled={pending !== null}
          >
            <KeyRound aria-hidden />
            {t("login.useToken")}
          </Button>
        ) : null}

        {tokenVisible ? (
          <form className="space-y-4" onSubmit={onSubmit} noValidate>
            <div className="space-y-2">
              <Label htmlFor="token">{t("login.accessToken")}</Label>
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
                  aria-label={reveal ? t("login.hideToken") : t("login.showToken")}
                  title={reveal ? t("login.hideToken") : t("login.showToken")}
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

            {error && tokenVisible ? (
              <p role="alert" className="text-sm text-destructive">
                {apiErrorMessage(error, t)}
              </p>
            ) : null}

            <Button
              type="submit"
              className="w-full"
              variant={oidc ? "outline" : "default"}
              disabled={pending !== null}
            >
              {pending === "token" ? <Loader2 aria-hidden className="animate-spin" /> : null}
              {t("login.title")}
            </Button>
          </form>
        ) : null}

        <p className="mt-6 text-xs text-muted-foreground">
          {oidc && !bearer
            ? t("login.noteRedirect")
            : oidc
              ? t("login.noteCookie")
              : t("login.noteBearer")}
        </p>
        {isMock ? (
          <p className="mt-2 text-xs text-muted-foreground">
            {t("login.mockMode")}
            {bearer ? (
              <>
                {" "}
                {t("login.mockTokens")}{" "}
                <span className="font-mono text-foreground">{DEMO_TOKEN}</span> (
                {t("login.mockAdmin")}) {t("login.mockAnd")}{" "}
                <span className="font-mono text-foreground">{DEMO_VIEWER_TOKEN}</span> (
                {t("login.mockViewer")}).
              </>
            ) : null}
            {oidc ? <> {t("login.mockOidc")}</> : null}
          </p>
        ) : null}
      </div>
    </div>
  );
}

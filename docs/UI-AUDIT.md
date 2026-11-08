# UI audit — Web Interface Guidelines (Vercel)

Audited 2026-11-08 against the official guideline set. `file:line` refers to the state
before the fixes. Priorities: **P0** blocking, **P1** should fix now, **P2** deferred.

**Status in v0.1.0: all P0 and P1 fixed and verified. P2 untouched.**

## P0 — blocking

1. `src/pages/instance-detail.tsx:573` — scrollable logs `<pre>` (`max-h-[60vh] overflow-auto`)
   is not focusable: text past the fold is unreachable by keyboard.
   → `tabIndex={0} role="region" aria-label="Container logs"`.
2. `src/components/code-block.tsx:21` — same for the Compose viewer.
   → `tabIndex={0} role="region" aria-label={title}`.
3. `src/pages/instance-detail.tsx:739` — switching tab unmounts the Configuration draft and
   discards it silently. → keep drafts mounted, or confirm before discarding.
4. `src/pages/instance-new.tsx:158` — the dirty guard only covers the Cancel button; reload,
   Back and hash navigation lose six steps silently. → `beforeunload` + router/hash guard
   when dirty.

## P1 — fix now (cheap, real impact)

5. `src/components/app-shell.tsx:222` — no skip link; `<main>` has no id.
   → skip link + `<main id="main" tabIndex={-1}>`.
6. `src/pages/fleet.tsx:406` — `<tr onClick>` navigation is not keyboard activatable and
   duplicates the name link. → drop the row handler; the name link is the action.
7. `src/pages/templates.tsx:83,111` — repeated link text "Use" / "Use template".
   → `aria-label={`Use ${template.name}`}`.
8. `src/pages/login.tsx:73` — `autoComplete="current-password"` on a bearer token.
   → `autoComplete="off" name="token"`.
9. `index.html:6` — `theme-color` hardcoded `#0a0a0a`, wrong in light mode.
   → set it from the resolved background in the theme effect.
10. `src/pages/fleet.tsx:314` — background refresh is silent.
    → `aria-live="polite"` on the status line, `aria-busy` on the table.
11. `src/components/app-shell.tsx:171` — `viewport-fit=cover` without
    `env(safe-area-inset-*)`. → pad header/nav.
12. `src/index.css:1` — `animate-in`/`slide-in-from-*` used by dialog/sheet/select/dropdown
    but `tw-animate-css` is not imported. → add the dependency and `@import "tw-animate-css"`.
13. `src/index.css:88` — no `touch-action: manipulation`, no `-webkit-tap-highlight-color`.
14. `src/components/error-alert.tsx:25`, `src/pages/instance-detail.tsx:89`,
    `src/pages/shares.tsx:368` — decorative `AlertCircle` svg not `aria-hidden`.
15. `src/pages/instance-detail.tsx:483` — `<Label>Shares</Label>` labels no control.
    → `fieldset`/`legend`.
16. `src/components/share-picker.tsx:74` — checkbox and label separated by padding: dead
    click zone. → wrap the row in the label.
17. `src/pages/instance-detail.tsx:104`, `src/pages/instance-new.tsx:596` — `1536 MB` breaks
    across lines. → non-breaking space.

## P2 — deferred

- `src/pages/fleet.tsx:61,122,302` — filters/search not in the URL; lost on reload/back.
- `src/pages/instance-detail.tsx:617` — active tab not in the URL (no deep link).
- `src/lib/format.ts:22,31,36` — hand-rolled dates, no Intl/locale/timezone.
- `src/pages/fleet.tsx:141` — one `GET /instances/{id}` per row (each carries
  `compose_yaml`). Fix by extending the list response, not by more UI calls.
- `src/pages/shares.tsx:120` — `autoFocus` opens the mobile keyboard; desktop-only.
- `src/pages/instance-new.tsx:407,532` — card `<div onClick>` next to the radio looks
  clickable but is not focusable; wrap in `<label>`.
- `src/components/page-header.tsx:15`, `src/components/data-list.tsx:43` — headings lack
  `text-balance`/`text-pretty`.
- `src/components/ui/button.tsx:7`, `src/components/ui/tabs.tsx:64` — `transition-all`.
- `src/pages/fleet.tsx:269`, `src/pages/shares.tsx:419`, `src/pages/templates.tsx:31` —
  "Loading … ." strings → ellipsis.
- `src/components/app-shell.tsx:89` — driver/docker detail only in `title=""`.
- Registry cleanup: `ui/dialog.tsx:71`, `ui/sheet.tsx:75` `focus:ring-2` →
  `focus-visible`; add `overscroll-behavior: contain` to dialog/sheet content.

## Clean

Icon-only buttons all have accessible names, form fields are labelled, dialogs have
titles, focus traps come from Radix, status is never colour-only, copy/toasts/errors are
announced.

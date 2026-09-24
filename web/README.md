# web

Svelte 5 + Vite + TypeScript single-page app, built into `dist/` and embedded
in the `mistarr` binary. Screens and API contract are in `docs/UI.md` and
`docs/API.md`. Gzipped bundle budget: 200 KiB.

```sh
npm ci
npm run dev              # dev server
VITE_MOCK=1 npm run dev  # dev server against fixture data, no backend needed
npm run check            # svelte-check
npm run lint             # eslint
npm run build            # -> dist/
npm run size             # gzip budget check against dist/
npm run e2e              # playwright screenshots of every screen, two viewports
```

`npm run e2e` uses Playwright's own browser resolution by default. Set
`PLAYWRIGHT_CHROMIUM_PATH` to point at a pre-installed Chromium binary
(e.g. `/opt/pw-browsers/chromium`) when the default download is unavailable.

Routing is a small hash-based router in `src/lib/router.svelte.ts` (under
50 lines, zero dependencies) rather than a router package, since the whole
app is seven flat routes with no nesting or transitions to justify one.

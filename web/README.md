# web

Svelte 5 + Vite + TypeScript single-page app, built into `dist/` and embedded
in the `mistarr` binary. Scaffolded in WP-08; screens and API contract are in
`docs/UI.md` and `docs/API.md`. Gzipped bundle budget: 200 KiB.

The server embeds whatever `dist/` holds when it is compiled. Files under
`dist/assets/` are served as immutable, everything else with `no-cache`, and
unknown paths get `index.html`. A binary built without `dist/` serves a
one-line placeholder page instead.

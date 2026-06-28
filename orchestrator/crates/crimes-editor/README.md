# crimes-editor

A standalone web app for curating the Wet Court crimes list — the Crimes panel
from the operator console, expanded into a focused, full-page editor, hosted so
collaborators can edit crimes **without running the orchestrator**.

It shares the console's stack and styling: SolidJS + Vite + TypeScript, the same
CSS classes (`crime-list`, `crime-cat`, `crime-subject`, `panel-body`, …), and
it reuses the booth's exact crime store (`crimes-core`) on the backend — so
validation, atomic file writes and id bookkeeping are identical. The crimes JSON
file stays the single source of truth: edits are written in place, you commit and
push them when you like, and the booth picks them up on its next restart.

## What it does (and deliberately doesn't)

- **Does**: add / edit / delete / enable-toggle / browse / search crimes,
  including the optional `subject` tag used by `creator` crimes; **group by
  category or subject/creator**; **CSV import/export**.
- **Doesn't**: the live-trial controls from the operator panel — the random
  *draw filter* and the *charge queue* — are intentionally absent. Those only
  mean something with a running booth; the editor is just the curated list.

### Group by

The `group by` selector buckets the (filtered) list under sticky headers — by
`category`, or by `subject / creator` (crimes with no subject collect under
`(no subject)`). Handy given `creator` is the largest category and each charge
is attributed to a specific creator via `subject`.

### CSV import / export

- **Export** downloads the currently-*visible* crimes (so filter/search first to
  export a subset) as `id,category,subject,charge,enabled`.
- **Import** reads that same shape. A row whose `id` matches an existing crime
  **updates** it; a row with no `id` (or an unknown one) is **added**. Only
  `category` and `charge` columns are required; `subject`/`enabled` are optional.
  Rows are validated client-side first, you confirm an "add N / update M /
  skip K" plan, then it applies them and reloads. (Import is not transactional —
  a mid-run server error stops and reloads to show what landed.)

## Layout

```
crates/crimes-editor/
├── src/
│   ├── main.rs        # axum: CRUD API + serves the embedded bundle
│   └── assets.rs      # rust-embed of frontend/dist (mirrors display::assets)
└── frontend/          # SolidJS app — same stack as orchestrator/frontend
    ├── src/App.tsx    # the full-page editor (adapted from CrimesPanel.tsx)
    ├── src/crimes.ts  # API client → /api/crimes
    └── src/app.css    # base theme + crimes styles, copied from the console
```

## Run it locally

The backend embeds the built frontend, so build the bundle once, then run:

```sh
cd orchestrator/crates/crimes-editor/frontend
npm install && npm run build
cd ../../..                       # back to orchestrator/
cargo run -p crimes-editor -- --crimes crimes/wet_court_crimes.json --listen 127.0.0.1:8099
# open http://127.0.0.1:8099
```

For UI work with hot reload, run the Vite dev server against a running binary:

```sh
cargo run -p crimes-editor -- --crimes crimes/wet_court_crimes.json   # :8080
cd crates/crimes-editor/frontend && npm run dev                       # :5174, proxies /api → :8080
```

Flags:

| Flag | Default | Meaning |
|---|---|---|
| `--crimes <path>` | `crimes/wet_court_crimes.json` | The crimes JSON file to edit |
| `--listen <addr>` | `0.0.0.0:8080` | Bind address |

## API

```
GET    /api/crimes          { crimes: [...], categories: [...] }
POST   /api/crimes          { category, charge, subject? } -> created crime
PUT    /api/crimes/{id}     full Crime body -> updated crime
DELETE /api/crimes/{id}     204
GET    /health              "ok"
```

## Deploy (homelab)

A `crimes-editor` service is wired into `deploy/homelab/docker-compose.yml`,
behind your existing host cloudflared. It needs **no tailnet/Spark access** — it
only touches the crimes file. Steps:

1. Point a second cloudflared hostname (e.g. `crimes.example.com`) at
   `http://localhost:${EDITOR_HOST_PORT}` (default `26879`).
2. Put a **Cloudflare Access** policy in front of that hostname with your
   collaborators' emails — that's your login, no app-level auth to build.
3. `docker compose up -d --build crimes-editor`.

The service bind-mounts the repo's `crimes/` dir, so every save lands directly in
the working tree. Commit and push whenever you want to publish; the booth sees
new crimes on its next pull + restart.

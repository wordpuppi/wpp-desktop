# wpp-desktop

The Tauri shell that wraps the built [`@wordpuppi/admin`](https://app.wordpuppi.com)
React SPA into a native macOS app. It adds native chrome around the web
admin — an environment picker, a resizable terminal dock, local file
save/open — without forking or reimplementing any of the admin's ~15
screens.

## This is a source mirror, not a standalone build

**`git clone && tauri build` will not produce a working app from this repo
alone.** `tauri.conf.json`'s `beforeBuildCommand` runs:

```sh
pnpm run build:shell && node scripts/copy-admin.mjs
```

`scripts/copy-admin.mjs` merges `../../wpp-admin/dist` — the built
`@wordpuppi/admin` SPA — into this shell's `dist/` output. `wpp-admin` is
~148 source files depending on `@wordpuppi/shared` (~57 more), and both
live in the private WordPuppi monorepo. Without that build present,
`copy-admin.mjs` warns and produces a shell with an empty iframe target
rather than failing outright. This repo exists so the Tauri/Rust half of
the app — the part that isn't tied to the private backend — is publicly
readable and auditable.

## Releases

Signed builds are published on the [Releases](../../releases) tab.
macOS artifacts are Developer-ID signed, notarized, and stapled. The app
auto-updates from `https://updates.wordpuppi.com/desktop/latest.json`.

## Layout

- `src-tauri/` — the Rust/Tauri app (838 lines: `lib.rs` 588, `terminal.rs`
  244, `main.rs` 6)
- `shell/` — the frame page that hosts the admin iframe (~1,053 lines)
- `scripts/copy-admin.mjs` — the 28-line admin merge step described above

## Architecture & decisions

### 1. Wrap the built `@wordpuppi/admin` bundle (not a dedicated UI)

The admin SPA routes every request through one `apiRequest` honouring
`setApiBase`, and its assets are rooted at `/` (`vite.config.ts base: '/'`),
so it drops into the Tauri webview unmodified. A dedicated native UI would
re-implement ~15 shipped screens for zero user value. The native
requirements this app adds — local file save (#426) and an integrated
terminal dock (#427) — need chrome *around/below* the admin; the shell
provides that chrome while the admin stays untouched.

### 2. Shell + iframe architecture (single stable webview)

`frontendDist` is a thin shell page (`shell/shell.html`, this crate) laid
out as a CSS flex **column**:

- top chrome: env picker + colored env badge
- middle: `<iframe src="/index.html#env=<env>">` — the wrapped admin, same
  origin (`tauri://localhost`)
- bottom: resizable terminal dock (#427)

No `unstable` multiwebview. The shell and admin share one
`tauri://localhost` origin.

### 3. Dist layout (the collision resolution)

Both the shell and the admin want `index.html`. Resolution:

- The shell is emitted by Vite as **`shell.html`**
  (`build.rollupOptions.input`).
- The Tauri window loads `shell.html`.
- `scripts/copy-admin.mjs` copies `wpp-admin/dist/*` into the shell
  `dist/` root, so the admin lands at **`/index.html`** — exactly what the
  iframe requests.
- Vite (`base: './'`) and the admin (`base: '/'`) both hash their
  `assets/` filenames, so the two `assets/` directories merge without
  collision.

Order matters: `build:shell` runs first (it empties `dist/`), then
`copy-admin.mjs` merges the admin in. `copy-admin.mjs` tolerates a missing
`wpp-admin/dist` (warns, produces an empty-but-valid dist) so a debug
build still succeeds before the admin has been built.

**No `/admin/` prefix anywhere** — the admin SPA lives at root (`base:
'/'`).

### 4. Env injection via `#env` hash

The shell writes `#env=local|qa|prod` into the iframe URL and persists the
selection via `tauri-plugin-store` (`settings.json`, key `selectedEnv`).
The admin reads `location.hash`, maps env to an API base from its baked
map (`local: 'http://localhost:5150/api'`,
`qa: 'https://app-qa.wordpuppi.com/api'`,
`prod: 'https://app.wordpuppi.com/api'`), calls `setApiBase`, and
namespaces its persisted state. Switching env reloads the iframe with the
new hash. Works in a plain browser too.

Env badge colors: **local = gray, QA = amber, prod = green**; hover shows
the active host.

### 5. Token storage = web-parity `localStorage` (not Keychain/Stronghold)

The wrapped admin owns its JWT in the webview's `localStorage`
(`wpp-auth`), namespaced to **`wpp-auth:<env>`** for per-env session
isolation. The shell never touches the JWT. The macOS WKWebView data store
is per-app-sandbox, and the token is already a 7-day bearer on web —
introducing Stronghold would fork the admin's storage for no threat-model
gain. `tauri-plugin-store` holds **non-secret shell settings only**
(`selectedEnv`, `dockOpen`, `dockHeight`, `workspaceDir`). Sessions are a
flat 7-day expiry with no refresh (deferred).

### 6. `src-tauri` is its own standalone Cargo project

It is **not** a member of the private backend's Cargo workspace, so Tauri
never compiles against the Loco/axum server. The pnpm `apps/*` glob picks
up `apps/wpp-desktop` automatically.

### 7. Updater signing

The updater endpoint is `https://updates.wordpuppi.com/desktop/latest.json`.
The verification key is public and embedded in `tauri.conf.json >
plugins.updater.pubkey`. The matching private signing key is held only as
a CI secret and is used to sign release artifacts at build time.

### 8. Terminal dock IPC (#427/#549)

`src-tauri/src/terminal.rs` exposes exactly four commands —
`terminal_spawn` / `terminal_write` / `terminal_resize` / `terminal_kill`
— over `portable-pty`. `tauri-plugin-shell` arbitrary-exec is **not**
enabled anywhere in this app.

`terminal_spawn` opens a real PTY and spawns the **user's own login
shell** (`$SHELL -l`, falling back to `/bin/zsh`) with cwd pinned to the
workspace directory (default `~/WordPuppi`, expanded and created if
missing). This is deliberate (#549): the dock makes no assumption that any
particular AI CLI is installed — a login shell sources the user's profile,
so whatever tools are on their real PATH (a coding-agent CLI, plain
`bash`, anything) come along for free, even under the minimal GUI-launchd
environment. A reader thread streams PTY output over `terminal://data`; a
wait thread emits `terminal://exit {code}` once the shell terminates,
guarded by a monotonic generation counter so a deliberate kill/respawn
never emits a stale exit event. `terminal_kill` kills the direct child
process and drops the PTY; dock chrome and the xterm theme follow
`prefers-color-scheme`.

### 9. Filesystem capability scope (#426)

`tauri-plugin-fs` + `tauri-plugin-dialog` are registered. The fs
capability is scoped to the **workspace directory only**
(`$HOME/WordPuppi` + `$HOME/WordPuppi/**`) — a write outside it is
rejected by the static scope. If the user changes `workspaceDir` away
from the default, runtime scope extension is a follow-up rather than a
day-one feature. The admin builds the Save/Open UI and the front-matter
serializer; both halves agree on the front-matter field set:

```
{ site_slug, content_type, id, title, slug, status, excerpt, tags,
  seo_title, meta_description, featured_image } + `---` + body
```

Filename is `<slug || id>.md`; the default directory is the
`workspaceDir` store value — the same directory the terminal dock opens
in.

### 10. External links open in the default browser (#488)

wry does not send `window.open` / `target="_blank"` / off-origin
navigations to the OS browser — they dead-end inside the wrap (preview
buttons, billing portal links, social/media links). Fixed at the webview
level, not per-link, so un-instrumented links are caught too:

- **Plugin**: [`tauri-plugin-opener`](https://v2.tauri.app/plugin/opener/)
  (Rust) + `@tauri-apps/plugin-opener` (JS). Chosen over
  `tauri-plugin-shell` deliberately — opener only opens a URL/path in the
  default app, preserving the posture that only the four `terminal_*` PTY
  commands can execute anything.
- **The belt (interception)**: the main window is built in
  `src-tauri/src/lib.rs`'s `setup()` via `WebviewWindowBuilder` so it can
  carry two handlers on the single webview:
  - `on_navigation` cancels in-webview navigation and routes external
    URLs to the OS opener.
  - `on_new_window` denies the new window and routes external URLs to the
    opener, else allows it.

  On macOS, wry's navigation and UI delegates fire for **iframe
  sub-frames too**, so this single pair also catches external links
  *inside* the wrapped admin iframe — no admin-side changes needed for
  the fallback path. `is_external` is `http`/`https` with a host other
  than `tauri.localhost`, plus `mailto`/`tel`; `tauri://`, `about:`,
  `data:`, and `blob:` stay internal.
- **The buckle (config)**: `opener:default` is granted in
  `capabilities/default.json` so the admin's own `openUrl(...)` calls are
  authorized too (scoped to http/https/mailto/tel). The Rust-side
  interception above catches everything else regardless of whether a
  link went through that authorized path.

### 11. ZIP save path capability (#491)

The admin does the JS side (`blob → arrayBuffer → dialog.save() →
fs.writeFile(path, bytes)`). The desktop half only needs the capability to
let a **dialog-chosen** path — which may be anywhere under `$HOME`,
outside the workspace-only static scope — accept the write, without
opening arbitrary read access to the rest of the disk:

- `dialog:default` (already granted) plus **`fs:allow-write-file`**
  (enables the binary `write_file`/`write`/`open` fs commands; the
  pre-existing `fs:allow-write-text-file` only covered the markdown save
  path from §9).
- **No static scope broadening.** `tauri-plugin-dialog`'s `save` command
  calls `window.try_fs_scope().allow_file(&chosen_path)` at runtime, so
  the fs scope is extended to *exactly* the file the user picked, for
  that session only. The static `fs:scope` stays pinned to
  `$HOME/WordPuppi` — read access to the rest of the disk is never
  granted.

### 12. API transport = native HTTP via `tauri-plugin-http`

The wrapped admin runs on `tauri://localhost`, so every API call to
`localhost:5150` / `app-qa.wordpuppi.com` / `app.wordpuppi.com` is
cross-origin *inside the webview* and would depend on the server's CORS
headers forever. Decision: the desktop app never depends on server CORS.

- `apiRequest` (the shared single fetch seam) routes through
  `@tauri-apps/plugin-http`'s WHATWG-compatible `fetch` when running
  inside Tauri. The request is made by the Rust process, so webview CORS
  never applies. `FormData` uploads and `AbortSignal` pass through
  unchanged. A dynamic import keeps the plugin out of the web bundle's
  main entry, so browser behavior stays byte-identical.
- **Scope**: `http:default` in `capabilities/default.json` is
  allow-listed to exactly the three API bases
  (`http://localhost:5150/*`, `https://app-qa.wordpuppi.com/*`,
  `https://app.wordpuppi.com/*`) — the webview cannot use native HTTP to
  reach anything else.
- The server-side CORS allowlist for `tauri://localhost` origins is
  retained as belt-and-braces — it costs nothing and covers any request
  that still goes through the webview's own fetch.

### 13. Social login round-trip = https redirect + web-login bounce

The desktop OAuth round-trip does **not** redirect the auth provider to
the app's `wordpuppi://` custom scheme. That scheme isn't in the auth
provider's URI allow-list, so a redirect to it silently falls back to the
configured site URl (`app.wordpuppi.com`) after provider consent and
strands the system browser — the deep link never fires.

Instead:

- The desktop login flow redirects to the **already allow-listed** https
  app origin at `/login?wpp_desktop=1`, derived from the active env's API
  base with the trailing `/api` stripped (prod →
  `https://app.wordpuppi.com/login?wpp_desktop=1`). The provider's
  authorize URL is opened in the real system browser.
- The web `/login` page, when loaded in a **plain browser** (not inside
  the app), recognizes the `wpp_desktop=1` marker plus a session/error
  fragment and, instead of running the normal web session exchange, hands
  the fragment back to the app via
  `wordpuppi://auth/social#<fragment>` — attempted automatically, with a
  visible "Return to the WordPuppi app" fallback button (custom-scheme
  navigation often needs a user gesture). The shell's existing deep-link
  handler consumes it unchanged.

Net effect: the `wordpuppi://` scheme is **app-internal only**
(shell ⇄ iframe) and never needs to be in the auth provider's URI
allow-list — only the https app origins do, and they already are. The
provider-denied error case, carrying the same marker, is rendered on the
web page rather than bounced into the app.

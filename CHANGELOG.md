# WordPuppi Desktop — Changelog

## 0.1.49

- Baxter, your AI co-editor (AB#645, lean v1): a right-rail chat on every site
  page (toggle in the sidebar footer) that can write and edit posts and pages,
  tweak the theme, schedule barks, queue posts to connected platforms, connect
  platforms, and build/deploy the site — through the same tools the MCP
  server exposes. Deletes, deploys and anything that leaves WordPuppi pause
  for an Approve/Reject card. Chat history lives in the site's content DB.
  Streams over the desktop's native HTTP (no shell changes needed).

## 0.1.46

- API keys moved to Account → API Keys (`/account/api-keys`); the old per-site
  path redirects there. Keys are per-user, not per-site (AB#654).
- Copy now actually reports the truth: one shared clipboard helper (awaited,
  execCommand fallback, error toast) and a "Copied ✓" pill only after the write
  resolves. View reveals the full key with Copy (AB#655).
- Rotate: mints a replacement key and revokes the old one in one step; Revoke
  now confirms and revoked keys read as terminal (AB#656, AB#654).

## 0.1.45

- No functional change. This release exists to exercise the GitHub update path
  end to end: 0.1.44 was the first build that checks GitHub first, so 0.1.45 is
  the first update it can actually pull from there.

## 0.1.44

- Updates are now checked and downloaded from the GitHub Releases page
  (github.com/wordpuppi/wpp-desktop) as the primary source, with
  updates.wordpuppi.com kept as an automatic fallback. Builds 0.1.43 and
  earlier keep using updates.wordpuppi.com — the endpoint list is baked in at
  build time and cannot be changed retroactively.

## 0.1.43

- Releases are now published to GitHub as well as the update endpoint
  (github.com/wordpuppi/wpp-desktop). The auto-updater is unchanged: every
  build still polls updates.wordpuppi.com, and GitHub is a second copy.
- Source for this package is mirrored to that same public repo.

## 0.1.0 — first signed release

- First codesigned + notarized macOS build published to updates.wordpuppi.com.
- Tauri v2 shell wrapping the wpp-admin webview, env picker, Claude PTY dock,
  local-save plumbing, external-link opener.
- Auto-update via the Tauri updater against the staging updates endpoint.

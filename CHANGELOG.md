# WordPuppi Desktop — Changelog

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

# WordPuppi Desktop — Changelog

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

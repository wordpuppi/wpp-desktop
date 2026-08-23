import './styles.css';
import '@xterm/xterm/css/xterm.css';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { load, type Store } from '@tauri-apps/plugin-store';
import { confirm, open } from '@tauri-apps/plugin-dialog';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';

type Env = 'local' | 'qa' | 'prod';

// Env → API host, for the badge tooltip only. The admin (#425) owns the
// baked env→base('/api') map; the shell just passes `#env=<env>` and labels it.
const ENV_HOST: Record<Env, string> = {
  local: 'localhost:5150',
  qa: 'app-qa.wordpuppi.com',
  prod: 'app.wordpuppi.com',
};

const DEFAULT_WORKSPACE = '~/WordPuppi';

// Build-time default env (e.g. the IDE "dev desktop (QA)" run config sets
// VITE_DEFAULT_ENV=qa). Only used when no env was previously picked/persisted.
const DEFAULT_ENV: Env = (['local', 'qa', 'prod'] as const).includes(
  import.meta.env.VITE_DEFAULT_ENV as Env,
)
  ? (import.meta.env.VITE_DEFAULT_ENV as Env)
  : 'local';

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

// Set by an initialization_script the Rust side injects in DEBUG BUILDS ONLY
// (lib.rs). Release builds never define it: the env picker (Local/QA) is an
// IDE affordance, customers get a prod-pinned app. A Vite flag can't carry
// this — the shell is `vite build` output in dev and release alike.
const DEBUG = Boolean(
  (window as unknown as { __WPP_DEBUG__?: boolean }).__WPP_DEBUG__,
);

const iframe = $<HTMLIFrameElement>('admin');
const envSelect = $<HTMLSelectElement>('env-select');
const envBadge = $<HTMLSpanElement>('env-badge');
const dock = $<HTMLElement>('dock');
const resizer = $<HTMLElement>('dock-resizer');
const dockCwd = $<HTMLSpanElement>('dock-cwd');
const mount = $<HTMLElement>('terminal-mount');
const overlay = $<HTMLElement>('dock-overlay');

// ---- persisted settings (tauri-plugin-store) ----
let store: Store;
let state = {
  selectedEnv: DEFAULT_ENV,
  dockOpen: false,
  dockHeight: 320,
  workspaceDir: DEFAULT_WORKSPACE,
};

async function loadState() {
  // `defaults` is required by plugin-store's StoreOptions type; empty is a no-op
  // here — every read below already falls back via `?? DEFAULT_*`.
  store = await load('settings.json', { autoSave: true, defaults: {} });
  state.selectedEnv = ((await store.get<Env>('selectedEnv')) ?? DEFAULT_ENV) as Env;
  state.dockOpen = (await store.get<boolean>('dockOpen')) ?? false;
  state.dockHeight = (await store.get<number>('dockHeight')) ?? 320;
  state.workspaceDir = (await store.get<string>('workspaceDir')) ?? DEFAULT_WORKSPACE;
}

const save = (k: keyof typeof state, v: unknown) => store?.set(k, v);

// ---- env picker ----
function applyEnv(env: Env) {
  state.selectedEnv = env;
  envSelect.value = env;
  envBadge.textContent = env;
  envBadge.dataset.env = env;
  envBadge.title = ENV_HOST[env];
  // Reload the admin with the new env hash → per-env session isolation (#425).
  iframe.src = `/index.html#env=${env}`;
  // Release pins prod without persisting it — a dev's stored env choice must
  // survive in settings.json, it's just never honored outside debug builds.
  if (DEBUG) save('selectedEnv', env);
}

envSelect.addEventListener('change', () => applyEnv(envSelect.value as Env));

// #623 v4 (macOS): the window is TitleBarStyle::Overlay (true full-size content
// view — Transparent never was; see lib.rs), so the webview owns the titlebar
// band and the shell paints it: 28px #drag band with the site badge + pill in
// it, admin iframe reserved below (styles.css). Tag <html> as `.mac` so
// styles.css shows the band; other OSes keep native decorations.
//
// #623 v4: the shell band now RESERVES its 28px (styles.css padding-top), so
// the admin never sits under the traffic lights and the old injected-CSS inset
// hack (selector-coupled to admin class names) is gone.
//
// Band theming: the band should read as part of the admin it wraps, so mirror
// the admin's `wpp-theme` localStorage key (same origin — same passive bridge
// as the #623 site badge, load-then-listen). Explicit light/dark set a data
// attr; 'system'/absent falls back to prefers-color-scheme in CSS.
function applyAdminTheme() {
  const t = localStorage.getItem('wpp-theme');
  if (t === 'dark' || t === 'light') document.documentElement.dataset.adminTheme = t;
  else delete document.documentElement.dataset.adminTheme;
}
if (navigator.userAgent.includes('Macintosh')) {
  document.documentElement.classList.add('mac');
  applyAdminTheme();
  window.addEventListener('storage', (e) => {
    if (e.key === 'wpp-theme') applyAdminTheme();
  });
}

// #622: the admin writes 'wpp-login-ping' on every successful login (same
// passive same-origin bridge as the badge/theme). A login is a fresh-session
// moment — re-run the silent update check so a release published AFTER the
// launch check still shows the pill promptly instead of waiting for the +1h
// tick (Rick hit this hole on a rapid-release morning). All OSes.
window.addEventListener('storage', (e) => {
  if (e.key === 'wpp-login-ping') void invoke('check_updates_silent').catch(() => {});
});

// ---- dock height / collapse ----
const clampHeight = (h: number) => {
  const max = window.innerHeight - 200; // admin always keeps >=200px
  return Math.max(120, Math.min(h, Math.max(120, max)));
};

function applyDockHeight() {
  dock.style.height = `${clampHeight(state.dockHeight)}px`;
}

function setDockOpen(open: boolean, respawn = true) {
  state.dockOpen = open;
  dock.hidden = !open;
  resizer.hidden = !open;
  save('dockOpen', open);
  if (open) {
    applyDockHeight();
    if (respawn) void startSession();
    setTimeout(() => term?.focus(), 0);
  } else {
    void invoke('terminal_kill').catch(() => {});
  }
}

function toggleDock() {
  setDockOpen(!state.dockOpen);
}

$('dock-close').addEventListener('click', () => setDockOpen(false));
// Top-right Restart kills a LIVE session → confirm first (native dialog; bare
// confirm() is a no-op under Tauri's WKWebView). The overlay's "New Terminal"
// (dock-restart-2) and the terminal://new menu item are explicit new-session
// intents with nothing to lose, so they stay confirm-free.
$('dock-restart').addEventListener('click', async () => {
  const ok = await confirm('Restart the terminal session? The current session will end.', {
    title: 'Restart Terminal',
  });
  if (ok) void startSession();
});

// drag-resize. Pointer events + setPointerCapture: with mouse events the drag
// died whenever the cursor crossed into the admin IFRAME (its document eats
// mousemove/mouseup, the shell never sees the release → handle stuck to the
// cursor). Capture routes every subsequent pointer event to the handle no
// matter what's underneath.
let dragging = false;
resizer.addEventListener('pointerdown', (e) => {
  dragging = true;
  resizer.setPointerCapture(e.pointerId);
  e.preventDefault();
});
resizer.addEventListener('pointermove', (e) => {
  if (!dragging) return;
  state.dockHeight = clampHeight(window.innerHeight - e.clientY);
  dock.style.height = `${state.dockHeight}px`;
  fit?.fit();
});
const endDrag = (e: PointerEvent) => {
  if (!dragging) return;
  dragging = false;
  resizer.releasePointerCapture(e.pointerId);
  save('dockHeight', state.dockHeight);
  sendResize();
};
resizer.addEventListener('pointerup', endDrag);
resizer.addEventListener('pointercancel', endDrag);
// double-click the handle collapses to header / restores
resizer.addEventListener('dblclick', () => {
  dock.classList.toggle('collapsed');
  if (!dock.classList.contains('collapsed')) fit?.fit();
});
window.addEventListener('resize', () => {
  applyDockHeight();
  fit?.fit();
  sendResize();
});

// Cmd+` toggles the dock from anywhere. Cmd+W / Cmd+Q are never intercepted.
// Also attached inside the admin iframe (same-origin) — keyboard focus lives
// there almost always, and the shell window never hears those keydowns
// (Rick's "dock doesn't show up", AB#549).
const dockKeyHandler = (e: KeyboardEvent) => {
  if (e.metaKey && e.key === '`') {
    e.preventDefault();
    toggleDock();
  }
};
window.addEventListener('keydown', dockKeyHandler);
iframe.addEventListener('load', () => {
  try {
    iframe.contentWindow?.addEventListener('keydown', dockKeyHandler);
  } catch {
    // cross-origin env (never in production builds) — menu item still works
  }
});

// Native "Terminal → New Terminal" menu item (AB#549): open the dock with a
// fresh session (startSession kills any previous one).
void listen('terminal://new', () => {
  if (state.dockOpen) void startSession();
  else setDockOpen(true);
});

// Native "Settings…" menu item (#607) → pick the default terminal directory.
// Rust expands ~ and mkdirs on spawn, and the picker returns an absolute path,
// so no munging here. Only pass defaultPath when already absolute (the stored
// default may still be the '~/WordPuppi' tilde form the native dialog can't seed).
async function pickWorkspace() {
  const path = await open({
    directory: true,
    title: 'Choose default terminal directory',
    defaultPath: state.workspaceDir.startsWith('/') ? state.workspaceDir : undefined,
  });
  if (typeof path !== 'string') return; // cancelled (null)
  state.workspaceDir = path;
  save('workspaceDir', path);
  dockCwd.textContent = path;
  dockCwd.title = path;
  // Live session? Offer to respawn in the new dir; otherwise next New Terminal picks it up.
  if (state.dockOpen) {
    const now = await confirm('Restart terminal in the new directory now?', {
      title: 'Directory Changed',
    });
    if (now) void startSession();
  }
}
void listen('settings://pick-workspace', () => void pickWorkspace());

// ---- terminal (xterm + PTY IPC) ----
let term: Terminal | null = null;
let fit: FitAddon | null = null;

function themeIsDark() {
  return window.matchMedia('(prefers-color-scheme: dark)').matches;
}

function ensureTerm() {
  if (term) return;
  term = new Terminal({
    fontFamily: 'SFMono-Regular, Menlo, monospace',
    fontSize: 12,
    cursorBlink: true,
    theme: themeIsDark()
      ? { background: '#12151c', foreground: '#e6e8ee' }
      : { background: '#ffffff', foreground: '#1f2430' },
  });
  fit = new FitAddon();
  term.loadAddon(fit);
  term.open(mount);
  term.onData((d) => void invoke('terminal_write', { data: d }));
  fit.fit();
}

function sendResize() {
  if (!term || !fit || dock.hidden) return;
  fit.fit();
  void invoke('terminal_resize', { rows: term.rows, cols: term.cols }).catch(() => {});
}

function showOverlay(html: string) {
  overlay.innerHTML = html;
  overlay.hidden = false;
}
function hideOverlay() {
  overlay.hidden = true;
}

async function startSession() {
  ensureTerm();
  hideOverlay();
  term!.clear();
  dockCwd.textContent = state.workspaceDir;
  dockCwd.title = state.workspaceDir;
  try {
    await invoke('terminal_kill').catch(() => {});
    await invoke('terminal_spawn', { cwd: state.workspaceDir });
    setTimeout(sendResize, 30);
  } catch (err) {
    showOverlay(`<div>Failed to start terminal: ${String(err)}</div>`);
  }
}

// PTY output → xterm; exit → dimmed banner + Restart.
void listen<string>('terminal://data', (e) => term?.write(e.payload));
void listen<{ code: number }>('terminal://exit', (e) => {
  showOverlay(
    `<div>shell exited (code ${e.payload.code})</div>` +
      `<button id="dock-restart-2">New Terminal</button>`,
  );
  $('dock-restart-2').addEventListener('click', () => void startSession());
});

// ---- update pill (#622): Zed-style "Restart to Update" ----
// The Rust side polls silently (1h after launch, then every 24h), downloads
// in the background, and emits update://ready once the on-disk app is the new
// version — so the pill's only job is offering the relaunch (update_restart).
// ✕ hides it for now; the next poll tick re-emits and it reappears.
const updatePill = $<HTMLElement>('update-pill');
function showUpdatePill(version: string) {
  $('update-restart').title = `Restart to update WordPuppi to ${version}`;
  updatePill.hidden = false;
}
void listen<string>('update://ready', (e) => showUpdatePill(e.payload));
// #622 race fix (Rick's 0.1.21 no-pill): the silent check fires IMMEDIATELY
// at login, so download + emit can finish BEFORE this module's listener is
// attached — the event is lost until the next 24h tick. Read the pending
// state once at load (#623 badge's load-then-listen); the listener handles
// later ticks. showUpdatePill is idempotent, so overlap with an emit is fine.
void invoke<string | null>('get_pending_update').then((v) => {
  if (v) showUpdatePill(v);
});
$('update-restart').addEventListener('click', () => void invoke('update_restart'));
$('update-dismiss').addEventListener('click', () => {
  updatePill.hidden = true;
});
// Dev-only styling preview (real polling is release-only): run
// __wppUpdatePillPreview() in the shell devtools console.
if (DEBUG) {
  (window as unknown as Record<string, unknown>).__wppUpdatePillPreview = () =>
    showUpdatePill('0.0.0-preview');
}

// ---- site badge (#623): Discord-style current-site favicon + name ----
// The ADMIN publishes the selected site to localStorage 'wpp-current-site'
// (sites.$siteSlug layout writes {slug, name, favicon}; cleared when no site
// is selected). Shell and iframe are same-origin (the #605 wpp-theme
// pattern), so the iframe's writes fire 'storage' events in this document.
// The key literal is the contract with the admin bundle — keep in sync.
const SITE_BADGE_KEY = 'wpp-current-site';
const siteBadge = $<HTMLElement>('site-badge');
const siteBadgeIcon = $<HTMLImageElement>('site-badge-icon');
const siteBadgeName = $<HTMLSpanElement>('site-badge-name');
// Favicon 404s (or any load failure) → name-only badge, never a broken image.
siteBadgeIcon.addEventListener('error', () => {
  siteBadgeIcon.hidden = true;
});
function renderSiteBadge(raw: string | null) {
  let site: { name?: string; favicon?: string | null } | null = null;
  try {
    site = raw ? JSON.parse(raw) : null;
  } catch {
    // malformed → treat as no site
  }
  if (!site?.name) {
    siteBadge.hidden = true;
    return;
  }
  siteBadgeName.textContent = site.name;
  siteBadgeIcon.hidden = !site.favicon;
  if (site.favicon && siteBadgeIcon.src !== site.favicon) siteBadgeIcon.src = site.favicon;
  siteBadge.hidden = false;
}
// Initial read: the iframe (src is set in the HTML) may have written before
// this module ran; after that, live via storage events.
renderSiteBadge(localStorage.getItem(SITE_BADGE_KEY));
window.addEventListener('storage', (e) => {
  if (e.key === SITE_BADGE_KEY) renderSiteBadge(e.newValue);
});

// ---- deep link (#362 / AB#333): complete the social-login round-trip ----
// Envelope literals are the contract with the admin's login.tsx listener; keep
// them in sync. Bare strings (not a shared import) because shell and admin are
// separate bundles.
const DEEPLINK_MSG_SOURCE = 'wpp-desktop-deeplink';
const DEEPLINK_MSG_KIND = 'social-auth';

async function handleDeepLinks(urls: string[]) {
  for (const url of urls) {
    if (!url.startsWith('wordpuppi://auth/social')) continue;
    const hashIndex = url.indexOf('#');
    if (hashIndex === -1) continue; // no fragment → nothing to exchange
    const fragment = url.slice(hashIndex + 1);
    // Forward the GoTrue fragment (carries the access_token) to the same-origin
    // admin iframe, then raise the app. NEVER log the fragment or url — tokens.
    iframe.contentWindow?.postMessage(
      { source: DEEPLINK_MSG_SOURCE, kind: DEEPLINK_MSG_KIND, fragment },
      window.location.origin,
    );
    const { getCurrentWindow } = await import('@tauri-apps/api/window');
    await getCurrentWindow().setFocus();
  }
}

async function wireDeepLink() {
  try {
    const { onOpenUrl } = await import('@tauri-apps/plugin-deep-link');
    await onOpenUrl((urls) => void handleDeepLinks(urls));
  } catch (err) {
    console.warn('[deep-link] not available:', err);
  }
}

// ---- boot ----
async function boot() {
  await loadState();
  // Release build → keep the switcher hidden (default-hidden in shell.html)
  // and pin prod, whatever was persisted or baked via VITE_DEFAULT_ENV
  // (a dev's settings.json must not steer a customer install at Local/QA).
  if (DEBUG) {
    $('topbar').hidden = false;
    $('env-controls').hidden = false;
    // Push the revealed topbar below the fixed #drag overlay (mac) so the
    // overlay doesn't eat clicks on the env picker. Release never gets here.
    document.documentElement.classList.add('has-topbar');
  } else {
    state.selectedEnv = 'prod';
  }
  applyEnv(state.selectedEnv);
  dock.style.height = `${clampHeight(state.dockHeight)}px`;
  await wireDeepLink();
  // Restore an open dock (decision: restoring an open dock respawns claude).
  if (state.dockOpen) setDockOpen(true, true);
}

void boot();

// ponytail: startup transport probe, remove when charter native pass done
void import('@tauri-apps/plugin-http').then(({ fetch: nativeFetch }) =>
  nativeFetch('https://app-qa.wordpuppi.com/_health').then(
    (r) => console.log('[transport-probe]', r.ok ? 'ok' : `fail ${r.status}`),
    (e) => console.log('[transport-probe] fail', e),
  ),
);

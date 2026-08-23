mod terminal;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::webview::NewWindowResponse;
use tauri::{Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
use terminal::TerminalState;

/// #488: any off-origin http/https (and mailto/tel) target belongs in the
/// user's default browser, never inside the wrap. On macOS/Linux the wrapped
/// admin lives on `tauri://localhost`; on Windows it's `http://tauri.localhost`
/// — both stay internal. Everything else (about:, data:, blob:, javascript:)
/// is left to the webview.
fn is_external(url: &Url) -> bool {
    match url.scheme() {
        "http" | "https" => {
            let host = url.host_str();
            // `tauri dev` (no devUrl) serves frontendDist from
            // http://localhost:1430 — that's the app itself, not an external
            // link. Debug-only: packaged builds load tauri://localhost and
            // must keep sending every http(s) host to the default browser.
            if cfg!(debug_assertions) && matches!(host, Some("localhost") | Some("127.0.0.1")) {
                return false;
            }
            host != Some("tauri.localhost")
        }
        "mailto" | "tel" => true,
        _ => false,
    }
}

/// Debug-only local backend lifecycle: `tauri dev` spawns the wpp-core server
/// (env "local" needs :5150) and app exit kills it — no orphaned `cargo run`
/// processes after closing the window. If something already listens on :5150
/// (backend started by hand / IDE), nothing is spawned and nothing is killed:
/// we only ever manage a backend we started. Packaged builds compile none of
/// this — a real desktop install never runs a local server.
#[cfg(debug_assertions)]
mod dev_backend {
    use std::process::{Child, Command};
    use std::sync::Mutex;

    /// The cargo-run backend we spawned (None = external one, not ours to kill).
    pub struct DevBackend(pub Mutex<Option<Child>>);

    pub fn spawn() -> Option<Child> {
        if std::net::TcpStream::connect(("127.0.0.1", 5150)).is_ok() {
            eprintln!("[wpp-desktop] backend already on :5150 — leaving its lifecycle alone");
            return None;
        }
        // The repo's source/ dir, resolved at compile time — debug builds only
        // ever run from the checkout, so this path always exists.
        let source_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
        let mut cmd = Command::new("cargo");
        cmd.args(["run", "--package", "wpp-core", "--bin", "wpp_core-cli", "--", "start"])
            .current_dir(source_dir);
        #[cfg(unix)]
        {
            // Own process group: `cargo run` execs the server as a child, and
            // killing cargo alone would orphan it still holding :5150.
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        match cmd.spawn() {
            Ok(child) => {
                eprintln!("[wpp-desktop] spawned local backend (pgid {})", child.id());
                Some(child)
            }
            Err(e) => {
                eprintln!("[wpp-desktop] failed to spawn local backend: {e}");
                None
            }
        }
    }

    pub fn kill(mut child: Child) {
        #[cfg(unix)]
        // Negative pid = the whole group: cargo AND the server it spawned.
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{}", child.id())])
            .status();
        #[cfg(not(unix))]
        let _ = child.kill();
        let _ = child.wait();
        eprintln!("[wpp-desktop] local backend stopped");
    }
}

/// Dev-only (macOS): a binary run straight out of `target/debug` — `cargo run`
/// or `tauri dev` — has NO .app bundle (LaunchServices reports bundleID=NULL),
/// so the Dock and ⌘-Tab fall back to the generic-executable icon; the icons
/// in `bundle.icon` only ever reach a packaged .app. Hand NSApp the icon
/// ourselves. Path resolved at compile time — debug builds only run from the
/// checkout (same assumption as dev_backend). Release builds get their icon
/// from the bundle and never compile this.
#[cfg(all(debug_assertions, target_os = "macos"))]
fn set_dev_dock_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSString;

    let Some(mtm) = MainThreadMarker::new() else { return };
    let path = NSString::from_str(concat!(env!("CARGO_MANIFEST_DIR"), "/icons/icon.icns"));
    if let Some(icon) = NSImage::initWithContentsOfFile(NSImage::alloc(), &path) {
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon)) };
    }
}

/// AB#523: update flow modeled on Zed's auto_update crate (checked 2026-07-16):
/// silent background checks download in the background and gate on a restart
/// instead of interrupting with a modal; manual checks are loud (explicit
/// up-to-date / error / already-running feedback). tauri-plugin-updater
/// replaces the .app on disk during download_and_install, so "restart
/// whenever" is safe — the running app keeps working until relaunch.
static CHECK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
/// Version already downloaded and waiting for a relaunch, if any.
static PENDING_UPDATE: Mutex<Option<String>> = Mutex::new(None);
const UPDATE_MENU_ID: &str = "check-for-updates";
const NEW_TERMINAL_MENU_ID: &str = "new-terminal";
const SETTINGS_MENU_ID: &str = "settings";

/// Menu items are main-thread-only; find ours by id and swap its text.
fn set_update_menu_text(handle: &tauri::AppHandle, text: String) {
    use tauri::menu::MenuItemKind;
    fn find(items: Vec<MenuItemKind<tauri::Wry>>) -> Option<tauri::menu::MenuItem<tauri::Wry>> {
        for item in items {
            match item {
                MenuItemKind::MenuItem(mi) if mi.id() == UPDATE_MENU_ID => return Some(mi),
                MenuItemKind::Submenu(sm) => {
                    if let Ok(children) = sm.items() {
                        if let Some(found) = find(children) {
                            return Some(found);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }
    let handle2 = handle.clone();
    let _ = handle.run_on_main_thread(move || {
        if let Some(item) = handle2.menu().and_then(|m| m.items().ok()).and_then(find) {
            let _ = item.set_text(text);
        }
    });
}

/// One shared update path for the hourly background check and the
/// "Check for Updates…" menu item. `interactive` = the user asked.
fn spawn_update_check(handle: tauri::AppHandle, interactive: bool) {
    use tauri::Emitter;
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
    use tauri_plugin_updater::UpdaterExt;

    if CHECK_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        if interactive {
            handle
                .dialog()
                .message("An update check is already in progress.")
                .title("Check for Updates")
                .show(|_| {});
        }
        return;
    }
    tauri::async_runtime::spawn(async move {
        struct ClearInFlight;
        impl Drop for ClearInFlight {
            fn drop(&mut self) {
                CHECK_IN_FLIGHT.store(false, Ordering::SeqCst);
            }
        }
        let _clear = ClearInFlight;

        let updater = match handle.updater() {
            Ok(u) => u,
            Err(e) => {
                if interactive {
                    handle
                        .dialog()
                        .message(format!("Update check failed: {e}"))
                        .title("Check for Updates")
                        .blocking_show();
                } else {
                    eprintln!("[wpp-desktop] updater init failed: {e}");
                }
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                let version = update.version.clone();
                // Already downloaded (the on-disk app is new, we're the old
                // binary until relaunch) — never re-download the same version.
                if PENDING_UPDATE.lock().unwrap().as_deref() == Some(version.as_str()) {
                    // #622: re-offer the pill whenever a downloaded update is
                    // still pending — a dismissed pill returns on the next poll
                    // tick, and an interactive "Later" leaves it visible too.
                    let _ = handle.emit("update://ready", &version);
                    if interactive {
                        let restart = handle
                            .dialog()
                            .message(format!(
                                "WordPuppi {version} is downloaded and ready. Restart now?"
                            ))
                            .title("Update ready")
                            .buttons(MessageDialogButtons::OkCancelCustom(
                                "Restart".into(),
                                "Later".into(),
                            ))
                            .blocking_show();
                        if restart {
                            handle.restart();
                        }
                    }
                    return;
                }
                if interactive {
                    // The user asked: keep explicit consent before downloading.
                    let install = handle
                        .dialog()
                        .message(format!(
                            "WordPuppi {} is available (you have {}). Install and restart?",
                            version, update.current_version
                        ))
                        .title("Update available")
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            "Install".into(),
                            "Later".into(),
                        ))
                        .blocking_show();
                    if install {
                        match update.download_and_install(|_, _| {}, || {}).await {
                            Ok(()) => handle.restart(),
                            Err(e) => {
                                handle
                                    .dialog()
                                    .message(format!("Update install failed: {e}"))
                                    .title("Update")
                                    .blocking_show();
                            }
                        }
                        return;
                    }
                    // #622 (Rick): "Later" must still leave the pill + menu item
                    // behind — fall through to the silent path below, exactly what
                    // the next hourly tick would have done anyway.
                }
                // Zed pattern: download silently, apply on restart. The
                // menu item becomes the passive "restart to update" pill.
                match update.download_and_install(|_, _| {}, || {}).await {
                    Ok(()) => {
                        *PENDING_UPDATE.lock().unwrap() = Some(version.clone());
                        set_update_menu_text(
                            &handle,
                            format!("Restart to Update WordPuppi to {version}"),
                        );
                        // #622: shell shows the "Restart to Update" pill.
                        let _ = handle.emit("update://ready", &version);
                    }
                    Err(e) => eprintln!("[wpp-desktop] update install failed: {e}"),
                }
            }
            Ok(None) => {
                if interactive {
                    handle
                        .dialog()
                        .message(format!(
                            // The REAL bundle version (tauri.conf.json), not
                            // CARGO_PKG_VERSION — Cargo.toml lags the releases
                            // (was 0.1.12 while shipping 0.1.17), which made an
                            // up-to-date app read as "0.1.12" (Rick's regress
                            // scare). package_info() is the single source.
                            "WordPuppi {} is up to date.",
                            tauri::Manager::package_info(&handle).version
                        ))
                        .title("Check for Updates")
                        .blocking_show();
                }
            }
            Err(e) => {
                // Zed policy: automatic failures stay quiet (offline is
                // normal); only a user-initiated check surfaces the error.
                if interactive {
                    handle
                        .dialog()
                        .message(format!("Update check failed: {e}"))
                        .title("Check for Updates")
                        .blocking_show();
                } else {
                    eprintln!("[wpp-desktop] update check failed: {e}");
                }
            }
        }
    });
}

/// #622 race fix (0.1.21 no-pill, Rick): the silent login-time check can
/// download and emit update://ready BEFORE the shell webview's listener is
/// attached — the event is lost until the next 24h tick. The shell reads this
/// once at load (#623 badge's load-then-listen pattern) so a pending update
/// shows the pill regardless of emit timing.
#[tauri::command]
fn get_pending_update() -> Option<String> {
    PENDING_UPDATE.lock().unwrap().clone()
}

/// #622: the shell's "Restart to Update" pill click. Guarded so a stray
/// invoke without a finished background download is a no-op — the pill only
/// ever shows after update://ready, i.e. the on-disk app is already replaced.
#[tauri::command]
fn update_restart(app: tauri::AppHandle) {
    if PENDING_UPDATE.lock().unwrap().is_some() {
        app.restart();
    }
}

/// #622: on-demand SILENT re-check. The shell calls this when the admin
/// signals a fresh login via the 'wpp-login-ping' localStorage bridge (the
/// admin itself stays 100% Tauri-free). Closes the update-published-after-
/// launch-check hole: the startup cadence is immediate/+1h/24h, so a release
/// published minutes AFTER launch showed no pill for an hour (Rick hit it on
/// a rapid-release morning). Safe anytime: an already-downloaded update just
/// re-emits update://ready (PENDING_UPDATE short-circuit), up-to-date is a
/// no-op. Release builds only — dev runs are unsigned and fail verification.
#[tauri::command]
fn check_updates_silent(app: tauri::AppHandle) {
    #[cfg(not(debug_assertions))]
    spawn_update_check(app, false);
    #[cfg(debug_assertions)]
    drop(app);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_deep_link::init())
        .manage(TerminalState::default())
        .invoke_handler(tauri::generate_handler![
            terminal::terminal_spawn,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_kill,
            update_restart,
            get_pending_update,
            check_updates_silent,
        ])
        .setup(|app| {
            #[cfg(debug_assertions)]
            {
                use tauri::Manager;
                app.manage(dev_backend::DevBackend(std::sync::Mutex::new(
                    dev_backend::spawn(),
                )));
            }
            #[cfg(all(debug_assertions, target_os = "macos"))]
            set_dev_dock_icon();

            // The main window is built here (not in tauri.conf.json) so it can
            // carry the #488 navigation / new-window handlers. wry's macOS
            // navigation + UI delegates fire for iframe sub-frames too, so a
            // single pair of handlers on this webview also catches every
            // external link inside the wrapped admin iframe.
            let opener = app.handle().clone();
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("shell.html".into()))
                .title("WordPuppi")
                .inner_size(1400.0, 900.0)
                .min_inner_size(1100.0, 700.0)
                .on_navigation(move |url| {
                    if is_external(url) {
                        // route to default browser, cancel the in-webview nav
                        let _ = opener.opener().open_url(url.as_str(), None::<&str>);
                        return false;
                    }
                    true
                })
                .on_new_window({
                    let opener = app.handle().clone();
                    move |url, _features| {
                        if is_external(&url) {
                            let _ = opener.opener().open_url(url.as_str(), None::<&str>);
                            return NewWindowResponse::Deny;
                        }
                        NewWindowResponse::Allow
                    }
                });

            // #623 v4 (macOS): Overlay, NOT Transparent. Transparent only makes
            // the titlebar see-through — tao sets fullsize_content_view=false, so
            // the webview still starts BELOW the titlebar band and no shell DOM
            // can ever render next to the traffic lights (why #623 v1–v3 CSS
            // fixes all landed under the band; the old comment here claimed
            // full-size content wrongly). Overlay sets fullsize_content_view=true:
            // the webview owns the whole window, the traffic lights float, and
            // the shell paints the 28px titlebar band itself (theme-matched, with
            // the site badge + update pill in it) and reserves its height so the
            // admin iframe starts below (shell/styles.css). Dark window
            // background kills the first-paint white flash before the CSS lands.
            let win = win.background_color(tauri::window::Color(0x09, 0x09, 0x0b, 0xff));
            #[cfg(target_os = "macos")]
            let win = win
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true)
                // Rick: 50%-thicker band (28 -> 42px). The lights don't follow the
                // DOM band, so re-center them. tao's inset math (view.rs
                // inset_traffic_lights): container height = button_h(12) + y, and
                // the buttons keep their default ~8px BOTTOM inset inside it, so
                // the visual TOP offset = y - 8. Centering in a 42px band needs
                // top offset (42-12)/2 = 15 -> y = 23 (y=15 rendered them 7px from
                // the top — Rick's "too high" screenshot). x=12 = close-button
                // left edge. Eyeball knobs — keep in sync with --titlebar-h in
                // shell/styles.css.
                .traffic_light_position(tauri::LogicalPosition::new(12.0, 23.0));

            // The env picker (Local/QA/Prod) is an IDE/debug affordance. Only
            // debug builds inject this marker; without it the shell hides the
            // switcher and pins prod (shell.ts boot()). Release builds never
            // define it, so customers can't point the app at staging/localhost.
            #[cfg(debug_assertions)]
            let win = win.initialization_script("window.__WPP_DEBUG__ = true;");

            win.build()?;

            // AB#523: "Check for Updates…" lives where each OS expects it —
            // macOS: app menu, right under About; elsewhere: a Help menu.
            // AB#549: "Terminal" menu (VS Code-style) before Window/Help — the
            // native item reaches the dock even when keyboard focus is inside
            // the admin iframe (where the shell's Cmd+` listener can't hear).
            {
                use tauri::menu::{Menu, MenuItem, SubmenuBuilder};
                let check_updates = MenuItem::with_id(
                    app,
                    UPDATE_MENU_ID,
                    "Check for Updates…",
                    true,
                    None::<&str>,
                )?;
                // #607: "Settings" is a submenu (Zed-style) holding "Set Default
                // Folder…", which opens a folder picker (frontend) for the dock
                // terminal cwd. Cmd+, (macOS Settings key) lives on the child.
                let set_default_folder = MenuItem::with_id(
                    app,
                    SETTINGS_MENU_ID,
                    "Set Default Folder…",
                    true,
                    Some("CmdOrCtrl+,"),
                )?;
                let settings = SubmenuBuilder::new(app, "Settings")
                    .item(&set_default_folder)
                    .build()?;
                let menu = Menu::default(app.handle())?;
                #[cfg(target_os = "macos")]
                if let Some(app_menu) = menu.items()?.first().and_then(|i| i.as_submenu().cloned())
                {
                    app_menu.insert(&check_updates, 1)?;
                    app_menu.insert(&settings, 2)?;
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let help = SubmenuBuilder::new(app, "Help").item(&check_updates).build()?;
                    menu.append(&help)?;
                    let file = SubmenuBuilder::new(app, "File").item(&settings).build()?;
                    menu.insert(&file, 0)?;
                }

                let new_terminal = MenuItem::with_id(
                    app,
                    NEW_TERMINAL_MENU_ID,
                    "New Terminal",
                    true,
                    Some("CmdOrCtrl+Shift+`"),
                )?;
                let terminal_menu =
                    SubmenuBuilder::new(app, "Terminal").item(&new_terminal).build()?;
                // Before the "Window" submenu (falls back to last place).
                let items = menu.items()?;
                let pos = items
                    .iter()
                    .position(|i| {
                        i.as_submenu()
                            .and_then(|s| s.text().ok())
                            .is_some_and(|t| t == "Window")
                    })
                    .unwrap_or(items.len());
                menu.insert(&terminal_menu, pos)?;

                app.set_menu(menu)?;
                app.on_menu_event(|handle, event| {
                    if event.id() == UPDATE_MENU_ID {
                        // After a background download the item reads "Restart
                        // to Update…" — clicking it applies, Zed-pill style.
                        if PENDING_UPDATE.lock().unwrap().is_some() {
                            handle.restart();
                        }
                        spawn_update_check(handle.clone(), true);
                    } else if event.id() == NEW_TERMINAL_MENU_ID {
                        use tauri::Emitter;
                        // Shell opens the dock and starts a fresh session.
                        let _ = handle.emit("terminal://new", ());
                    } else if event.id() == SETTINGS_MENU_ID {
                        use tauri::Emitter;
                        // Shell runs the workspace-dir folder picker + persists.
                        let _ = handle.emit("settings://pick-workspace", ());
                    }
                });
            }

            // AB#523/#622: nothing called the updater before — the plugin was
            // registered but never checked, so installed apps could never
            // self-update. Rick's cadence (#622, refined 2026-07-20): check
            // IMMEDIATELY after login, again 1 hour later, then once every
            // 24 hours (hardcoded, no setting). Release builds only: dev runs
            // aren't signed and would fail verification.
            #[cfg(not(debug_assertions))]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    spawn_update_check(handle.clone(), false);
                    std::thread::sleep(std::time::Duration::from_secs(60 * 60));
                    loop {
                        spawn_update_check(handle.clone(), false);
                        std::thread::sleep(std::time::Duration::from_secs(24 * 60 * 60));
                    }
                });
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building WordPuppi desktop")
        .run(|app, event| {
            // #622 follow-up (Rick): after an update relaunch the fresh
            // instance came up WITHOUT foreground activation on macOS — he had
            // to click the dock icon. set_focus at Ready (setup is too early
            // on macOS) activates the app (tao's macOS set_focus does
            // activateIgnoringOtherApps + makeKeyAndOrderFront). Normal
            // launches are already active, so this is a harmless no-op there —
            // same move the deep-link handler makes from JS.
            if let tauri::RunEvent::Ready = event {
                use tauri::Manager;
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.set_focus();
                }
            }
            #[cfg(debug_assertions)]
            if let tauri::RunEvent::Exit = event {
                use tauri::Manager;
                if let Some(state) = app.try_state::<dev_backend::DevBackend>() {
                    if let Some(child) = state.0.lock().unwrap().take() {
                        dev_backend::kill(child);
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::is_external;
    use tauri::Url;

    #[test]
    fn external_vs_internal() {
        let ext = |s: &str| is_external(&Url::parse(s).unwrap());
        // external → default browser
        assert!(ext("https://app.wordpuppi.com/preview"));
        assert!(ext("http://example.com"));
        assert!(ext("https://bsky.app/profile/x"));
        assert!(ext("mailto:hi@wordpuppi.com"));
        // internal → stay in webview
        assert!(!ext("tauri://localhost/index.html#env=qa")); // macOS/Linux origin
        assert!(!ext("http://tauri.localhost/index.html")); // Windows origin
        assert!(!ext("http://localhost:1430/shell.html")); // tauri dev server (debug builds)
        assert!(!ext("about:blank"));
        assert!(!ext("blob:tauri://localhost/abc"));
    }
}

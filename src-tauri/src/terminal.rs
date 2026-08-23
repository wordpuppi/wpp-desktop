//! Bottom-dock terminal PTY (#427/#549). Exposes exactly four commands —
//! `terminal_spawn/write/resize/kill`. The dock is an integrated terminal in
//! the VS Code sense: it spawns the USER'S OWN login shell in the workspace
//! dir (no assumption that `claude` — or any particular AI CLI — is installed
//! or wanted; Rick: "user might not have a claude subscription yet or they may
//! want a different AI harness"). We only open a PTY and pipe bytes; the login
//! shell sources the user's profile, so their real PATH comes along for free
//! even under the minimal GUI launchd env (the old #542 layer-1 problem).

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter, State};

/// Managed state holding the single live PTY session (v1 = one terminal).
#[derive(Default)]
pub struct TerminalState {
    inner: Mutex<Option<Session>>,
    /// #607 monotonic session id. Bumped on every spawn AND kill; a wait thread
    /// only emits `terminal://exit` while its captured id is still current.
    /// Invariant: a superseded session (kill/respawn) never emits — only a
    /// genuine shell exit (generation unchanged) does.
    generation: Arc<AtomicU64>,
}

struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

#[derive(Clone, serde::Serialize)]
struct ExitPayload {
    code: u32,
}

/// Expand a leading `~` and materialize the workspace dir (#542 second layer:
/// the shell's DEFAULT_WORKSPACE is the literal string "~/WordPuppi" — Rust
/// never expands `~`, and the dir doesn't exist on a fresh machine, so the
/// PTY spawn failed on cwd even once `claude` resolved).
fn workspace_dir(raw: &str) -> Result<std::path::PathBuf, String> {
    let path = match raw.strip_prefix("~/") {
        Some(rest) => home_dir()?.join(rest),
        None if raw == "~" => home_dir()?,
        None => std::path::PathBuf::from(raw),
    };
    std::fs::create_dir_all(&path)
        .map_err(|e| format!("cannot create workspace dir {}: {e}", path.display()))?;
    Ok(path)
}

fn home_dir() -> Result<std::path::PathBuf, String> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .map_err(|_| "cannot expand ~: HOME is not set".to_string())
}

/// Spawn the user's login shell in `cwd` on a real PTY (#549: no `claude`
/// presumption — the user runs whatever harness they like; a login shell
/// sources their profile so their real PATH is present under GUI launchd).
#[tauri::command]
pub fn terminal_spawn(
    app: AppHandle,
    state: State<'_, TerminalState>,
    cwd: String,
) -> Result<(), String> {
    let cwd = workspace_dir(&cwd)?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());

    // #607: claim a new generation FIRST — this both silences the old session's
    // wait thread (its captured id is now stale) and is the id our own wait
    // thread checks before emitting. On a Restart the frontend kills then
    // spawns; without this the dying old child's wait thread fired a spurious
    // `terminal://exit` over the fresh terminal ("shell exited" overlay).
    let generation = state.generation.clone();
    let my_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;

    // Replace any previous session (kills its child first).
    if let Some(mut old) = state.inner.lock().unwrap().take() {
        let _ = old.killer.kill();
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| e.to_string())?;

    let mut cmd = CommandBuilder::new(shell);
    cmd.arg("-l"); // login shell: sources the user's profile (PATH etc.)
    cmd.cwd(&cwd);
    // GUI launchd env has no TERM and portable_pty doesn't add one — TUI apps
    // expect it on a PTY.
    if std::env::var_os("TERM").is_none() {
        cmd.env("TERM", "xterm-256color");
    }

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    // Drop the slave handle so the master read yields EOF when the shell exits.
    drop(pair.slave);

    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    {
        let mut guard = state.inner.lock().unwrap();
        *guard = Some(Session { master: pair.master, writer, killer });
    }

    // Reader thread → pipe PTY output to the webview.
    let app_data = app.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                // ponytail: from_utf8_lossy can mangle a multibyte char split
                // across a read boundary; fine for a v1 ANSI stream. Upgrade to
                // an incremental UTF-8 decoder if garbled glyphs show up.
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let _ = app_data.emit("terminal://data", chunk);
                }
            }
        }
    });

    // Wait thread → report the real exit code once the shell terminates, but
    // only if this session is still current. A kill/respawn bumps `generation`
    // past `my_gen`, so a deliberately-killed child stays silent (#607); a
    // genuine `exit` (generation untouched) still emits.
    let app_exit = app.clone();
    std::thread::spawn(move || {
        let code = child.wait().map(|s| s.exit_code()).unwrap_or(0);
        if generation.load(Ordering::SeqCst) == my_gen {
            let _ = app_exit.emit("terminal://exit", ExitPayload { code });
        }
    });

    Ok(())
}

/// Forward webview keystrokes to the PTY master.
#[tauri::command]
pub fn terminal_write(state: State<'_, TerminalState>, data: String) -> Result<(), String> {
    let mut guard = state.inner.lock().unwrap();
    if let Some(session) = guard.as_mut() {
        session.writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        session.writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Resize the PTY when the dock/FitAddon reflows.
#[tauri::command]
pub fn terminal_resize(
    state: State<'_, TerminalState>,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    let guard = state.inner.lock().unwrap();
    if let Some(session) = guard.as_ref() {
        session
            .master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::workspace_dir;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// #607: the wait-thread guard. A session captures `my_gen = fetch_add+1`;
    /// it emits iff the shared generation still equals it. A kill/respawn bumps
    /// the generation, so a superseded session is silent while a current one
    /// (nothing bumped after it) still emits.
    #[test]
    fn superseded_generation_is_suppressed() {
        let gen = AtomicU64::new(0);
        // Session A claims gen 1 (mirrors terminal_spawn's fetch_add+1).
        let a_gen = gen.fetch_add(1, Ordering::SeqCst) + 1;
        // A never superseded → still current → would emit.
        assert_eq!(gen.load(Ordering::SeqCst), a_gen);
        // A Restart: kill bumps, then a respawn claims gen 3.
        gen.fetch_add(1, Ordering::SeqCst); // kill
        let b_gen = gen.fetch_add(1, Ordering::SeqCst) + 1; // respawn
        // A's captured id is now stale → suppressed.
        assert_ne!(gen.load(Ordering::SeqCst), a_gen);
        // B is current → would emit.
        assert_eq!(gen.load(Ordering::SeqCst), b_gen);
    }

    /// #542 end-to-end: the REAL production default string "~/WordPuppi" must
    /// expand to $HOME/WordPuppi and be materialized on disk. Only cleans up the
    /// dir it created — never deletes Rick's real ~/WordPuppi if it predates us.
    #[test]
    fn workspace_dir_materializes_production_default() {
        let home = std::env::var("HOME").unwrap();
        let expected = std::path::Path::new(&home).join("WordPuppi");
        let existed_before = expected.exists();
        let dir = workspace_dir("~/WordPuppi").unwrap();
        assert_eq!(dir, expected);
        assert!(dir.is_dir(), "production default workspace must exist on disk");
        if !existed_before {
            let _ = std::fs::remove_dir(&dir);
        }
    }


    #[test]
    fn workspace_dir_expands_tilde_and_creates() {
        let home = std::env::var("HOME").unwrap();
        // Literal default the shell sends on a fresh install.
        let dir = workspace_dir("~/.wpp-desktop-test-542").unwrap();
        assert_eq!(dir, std::path::Path::new(&home).join(".wpp-desktop-test-542"));
        assert!(dir.is_dir(), "workspace dir must be materialized");
        std::fs::remove_dir(&dir).unwrap();
        // Absolute paths pass through untouched.
        assert_eq!(workspace_dir("/tmp").unwrap(), std::path::PathBuf::from("/tmp"));
    }

}

/// Kill the child on dock close. Dropping the session also closes the master,
/// so the reader thread EOFs and the wait thread reports the exit.
///
/// ponytail: kills the direct shell process. If the shell's children (claude
/// or any other harness the user launched) ever orphan, move to a
/// process-group kill (setsid + killpg).
#[tauri::command]
pub fn terminal_kill(state: State<'_, TerminalState>) {
    // #607: a deliberate kill supersedes its own wait thread — bump so the
    // resulting `terminal://exit` is suppressed.
    state.generation.fetch_add(1, Ordering::SeqCst);
    let mut guard = state.inner.lock().unwrap();
    if let Some(mut session) = guard.take() {
        let _ = session.killer.kill();
    }
}

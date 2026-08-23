//! Headless probe for the dock's PTY spawn path (#542/#549 diagnosis harness):
//! spawn the user's login shell exactly as terminal_spawn does and report
//! whether it survives. Run under a launchd-like env:
//!   env -i HOME=$HOME USER=$USER SHELL=/bin/zsh PATH=/usr/bin:/bin:/usr/sbin:/sbin \
//!     cargo run --example spawn_probe

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;

fn main() {
    let home = std::env::var("HOME").unwrap();
    let cwd = std::path::Path::new(&home).join("WordPuppi");
    std::fs::create_dir_all(&cwd).unwrap();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    println!("spawning login shell: {shell} in {}", cwd.display());

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .unwrap();
    let mut cmd = CommandBuilder::new(shell);
    cmd.arg("-l");
    cmd.cwd(&cwd);
    if std::env::var_os("TERM").is_none() {
        cmd.env("TERM", "xterm-256color");
    }
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut bytes = 0usize;
    let mut exited: Option<u32> = None;
    while std::time::Instant::now() < deadline {
        if let Ok(chunk) = rx.recv_timeout(std::time::Duration::from_millis(300)) {
            bytes += chunk.len();
        }
        if let Some(status) = child.try_wait().unwrap() {
            exited = Some(status.exit_code());
            break;
        }
    }
    match exited {
        Some(code) => println!("RESULT: shell EXITED code={code} after {bytes} output bytes"),
        None => {
            println!("RESULT: shell ALIVE after 5s, {bytes} output bytes — killing");
            let _ = child.kill();
        }
    }
}

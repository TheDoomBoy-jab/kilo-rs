mod editor;
mod input;
mod row;
mod terminal;

use std::env;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use editor::Editor;
use terminal::RawModeGuard;

static WINCH_FLAG: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigwinch(_: libc::c_int) {
    WINCH_FLAG.store(true, Ordering::Relaxed);
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: kilo-rs <filename>");
        std::process::exit(1);
    }

    let _raw_guard = RawModeGuard::enable()?;

    unsafe {
        if libc::signal(
            libc::SIGWINCH,
            handle_sigwinch as *const () as libc::sighandler_t,
        ) == libc::SIG_ERR
        {
            return Err(io::Error::last_os_error());
        }
    }

    let mut editor = Editor::new()?;
    editor.open(&args[1])?;
    editor.set_status_message("HELP: Ctrl-S = save | Ctrl-Q = quit | Ctrl-F = find");

    editor.refresh_screen()?;
    loop {
        if WINCH_FLAG.swap(false, Ordering::Relaxed) {
            editor.update_window_size()?;
            editor.refresh_screen()?;
        }
        if let Some(key) = input::read_key()? {
            if !editor.process_keypress(key)? {
                break;
            }
            editor.refresh_screen()?;
        }
    }

    Ok(())
}

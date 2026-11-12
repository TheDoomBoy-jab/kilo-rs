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
        libc::signal(libc::SIGWINCH, handle_sigwinch as *const () as libc::sighandler_t);
    }

    let mut editor = Editor::new()?;
    editor.open(&args[1])?;
    editor.set_status_message("HELP: Ctrl-S = save | Ctrl-Q = quit | Ctrl-F = find");

    loop {
        if WINCH_FLAG.swap(false, Ordering::Relaxed) {
            let _ = editor.update_window_size();
        }
        editor.refresh_screen()?;
        if !editor.process_keypress()? {
            break;
        }
    }

    Ok(())
}

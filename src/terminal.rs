use std::io;
use std::mem::MaybeUninit;

/// RAII Guard: enables POSIX raw terminal mode on creation and
/// guarantees restoration of original terminal attributes on Drop (even on panic).
pub struct RawModeGuard {
    orig_termios: libc::termios,
}

impl RawModeGuard {
    pub fn enable() -> io::Result<Self> {
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "stdin is not a terminal",
                ));
            }

            let mut orig = MaybeUninit::<libc::termios>::uninit();
            if libc::tcgetattr(libc::STDIN_FILENO, orig.as_mut_ptr()) == -1 {
                return Err(io::Error::last_os_error());
            }
            let orig_termios = orig.assume_init();

            let mut raw = orig_termios;
            raw.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
            raw.c_oflag &= !(libc::OPOST);
            raw.c_cflag |= libc::CS8;
            raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);

            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 1;

            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw) == -1 {
                return Err(io::Error::last_os_error());
            }

            Ok(Self { orig_termios })
        }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.orig_termios);
        }
    }
}

/// Queries terminal (rows, cols) via ioctl TIOCGWINSZ.
pub fn get_window_size() -> io::Result<(usize, usize)> {
    unsafe {
        let mut ws = MaybeUninit::<libc::winsize>::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, ws.as_mut_ptr()) == -1 {
            return Err(io::Error::last_os_error());
        }
        let ws = ws.assume_init();
        if ws.ws_col == 0 || ws.ws_row == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "invalid terminal window size",
            ));
        }
        Ok((ws.ws_row as usize, ws.ws_col as usize))
    }
}

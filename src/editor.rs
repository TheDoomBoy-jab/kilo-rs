use std::fs;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use crate::input::{self, Key};
use crate::row::{Highlight, Row, SyntaxSpec, select_syntax};
use crate::terminal;

const KILO_VERSION: &str = "0.1.0-rs";
const KILO_QUIT_TIMES: u8 = 3;
type PromptCallback<'a> = dyn FnMut(&mut Editor, &[u8], Key) + 'a;

// Metadata is rendered in one terminal cell per byte, just like file contents.
// Never allow filenames or I/O errors to inject terminal control sequences.
fn display_metadata(bytes: &[u8], width: usize) -> Vec<u8> {
    bytes
        .iter()
        .take(width)
        .map(|&b| if (32..127).contains(&b) { b } else { b'?' })
        .collect()
}

// Write beside the destination and rename only after a complete, synced write.
// Existing symlinks continue to refer to their original target.
fn write_file(filename: &str, contents: &[u8]) -> io::Result<()> {
    use std::fs::OpenOptions;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let path = Path::new(filename);
    let target = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => fs::canonicalize(path)?,
        Ok(_) => path.to_path_buf(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    let metadata = match fs::metadata(&target) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "destination is not a regular file",
                ));
            }
            if metadata.permissions().readonly() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "destination is read-only",
                ));
            }
            Some(metadata)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (temp, mut file) = loop {
        let temp = parent.join(format!(
            ".kilo-rs-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        if let Some(metadata) = metadata {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub struct Editor {
    cx: usize,
    cy: usize,
    rx: usize,
    rowoff: usize,
    coloff: usize,
    screenrows: usize,
    screenheight: usize,
    screencols: usize,
    rows: Vec<Row>,
    dirty: bool,
    filename: Option<String>,
    statusmsg: String,
    statusmsg_time: Instant,
    syntax: Option<&'static SyntaxSpec>,
    quit_times: u8,
    /// Pre-allocated frame buffer reused across renders to avoid per-char realloc()
    frame_buf: Vec<u8>,
}

impl Editor {
    pub fn new() -> io::Result<Self> {
        let (rows, cols) = terminal::get_window_size()?;
        Ok(Self {
            cx: 0,
            cy: 0,
            rx: 0,
            rowoff: 0,
            coloff: 0,
            screenrows: rows.saturating_sub(2).max(1),
            screencols: cols,
            screenheight: rows,
            rows: Vec::new(),
            dirty: false,
            filename: None,
            statusmsg: String::new(),
            statusmsg_time: Instant::now(),
            syntax: None,
            quit_times: KILO_QUIT_TIMES,
            frame_buf: Vec::with_capacity(8192),
        })
    }

    pub fn update_window_size(&mut self) -> io::Result<()> {
        let (rows, cols) = terminal::get_window_size()?;
        self.screenrows = rows.saturating_sub(2).max(1);
        self.screencols = cols;
        self.screenheight = rows;
        // Resizing changes the viewport, never the document position.
        self.scroll();
        Ok(())
    }

    pub fn set_status_message(&mut self, msg: impl Into<String>) {
        self.statusmsg = msg.into();
        self.statusmsg_time = Instant::now();
    }

    pub fn open(&mut self, filename: &str) -> io::Result<()> {
        let contents = match fs::read(filename) {
            Ok(contents) => contents,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        self.filename = Some(filename.to_string());
        self.syntax = select_syntax(filename);
        self.rows.clear();
        for line in contents.split_inclusive(|&b| b == b'\n') {
            let (chars, ending) = if let Some(body) = line.strip_suffix(b"\r\n") {
                (body, b"\r\n".as_slice())
            } else if let Some(body) = line.strip_suffix(b"\n") {
                (body, b"\n".as_slice())
            } else {
                (line, b"".as_slice())
            };
            let mut row = Row::new(chars.to_vec());
            row.ending = ending.to_vec();
            self.rows.push(row);
        }
        self.cx = 0;
        self.cy = 0;
        self.rowoff = 0;
        self.coloff = 0;
        self.recompute_all_rows();
        self.dirty = false;
        Ok(())
    }

    fn recompute_all_rows(&mut self) {
        let mut prev_open = false;
        for row in &mut self.rows {
            row.update(self.syntax, prev_open);
            prev_open = row.hl_open_comment;
        }
    }

    fn update_row_from(&mut self, start_idx: usize) {
        // Structural edits can change the predecessor of an existing row even
        // when the newly inserted row's outgoing comment state is unchanged.
        for idx in start_idx..self.rows.len() {
            let prev_open = if idx > 0 {
                self.rows[idx - 1].hl_open_comment
            } else {
                false
            };
            let changed = self.rows[idx].update(self.syntax, prev_open);
            if idx > start_idx && !changed {
                break;
            }
        }
    }

    fn save(&mut self) -> io::Result<()> {
        if self.filename.is_none() {
            if let Some(name) = self.prompt("Save as: {} (ESC to cancel)", None)? {
                let Ok(name) = String::from_utf8(name) else {
                    self.set_status_message("Can't save: filename must be valid UTF-8");
                    return Ok(());
                };
                self.syntax = select_syntax(&name);
                self.filename = Some(name);
                self.recompute_all_rows();
            } else {
                self.set_status_message("Save aborted");
                return Ok(());
            }
        }

        let Some(ref path) = self.filename else {
            return Ok(());
        };
        let total_bytes: usize = self
            .rows
            .iter()
            .map(|r| r.chars.len() + r.ending.len())
            .sum();
        let mut out = Vec::with_capacity(total_bytes);
        for row in &self.rows {
            out.extend_from_slice(&row.chars);
            out.extend_from_slice(&row.ending);
        }

        match write_file(path, &out) {
            Ok(()) => {
                self.dirty = false;
                self.set_status_message(format!("{} bytes written on disk", out.len()));
            }
            Err(e) => {
                self.set_status_message(format!("Can't save! I/O error: {}", e));
            }
        }
        Ok(())
    }

    fn insert_char(&mut self, c: u8) {
        if self.cy == self.rows.len() {
            self.ensure_previous_newline();
            let mut row = Row::new(Vec::new());
            row.ending.clear();
            self.rows.push(row);
        }
        let cy = self.cy;
        self.rows[cy].insert_char(self.cx, c);
        self.update_row_from(cy);
        self.cx += 1;
        self.dirty = true;
    }

    fn ensure_previous_newline(&mut self) {
        if let Some(previous) = self.rows.last_mut().filter(|r| r.ending.is_empty()) {
            previous.ending = b"\n".to_vec();
        }
    }

    fn insert_newline(&mut self) {
        if self.cy == self.rows.len() {
            self.ensure_previous_newline();
        }
        if self.cx == 0 {
            self.rows.insert(self.cy, Row::new(Vec::new()));
        } else {
            let tail = self.rows[self.cy].chars.split_off(self.cx);
            let ending = std::mem::replace(&mut self.rows[self.cy].ending, b"\n".to_vec());
            let mut new_row = Row::new(tail);
            new_row.ending = ending;
            self.rows.insert(self.cy + 1, new_row);
        }
        self.update_row_from(self.cy);
        self.cy += 1;
        self.cx = 0;
        self.dirty = true;
    }

    fn del_char(&mut self) {
        if self.cy == self.rows.len() {
            if self.cy > 0 {
                self.cy -= 1;
                self.cx = self.rows[self.cy].chars.len();
                self.dirty |= !self.rows[self.cy].ending.is_empty();
                self.rows[self.cy].ending.clear();
            }
            return;
        }
        if self.cx == 0 && self.cy == 0 {
            return;
        }
        if self.cx > 0 {
            let cy = self.cy;
            self.rows[cy].delete_char(self.cx - 1);
            self.update_row_from(cy);
            self.cx -= 1;
        } else {
            let removed = self.rows.remove(self.cy);
            self.cy -= 1;
            self.cx = self.rows[self.cy].chars.len();
            self.rows[self.cy].chars.extend_from_slice(&removed.chars);
            self.rows[self.cy].ending = removed.ending;
            self.update_row_from(self.cy);
        }
        self.dirty = true;
    }

    fn scroll(&mut self) {
        self.rx = if self.cy < self.rows.len() {
            self.rows[self.cy].cx_to_rx(self.cx)
        } else {
            0
        };

        if self.cy < self.rowoff {
            self.rowoff = self.cy;
        }
        if self.cy >= self.rowoff + self.screenrows {
            self.rowoff = self.cy - self.screenrows + 1;
        }
        if self.rx < self.coloff {
            self.coloff = self.rx;
        }
        if self.rx >= self.coloff + self.screencols {
            self.coloff = self.rx - self.screencols + 1;
        }
    }

    pub fn refresh_screen(&mut self) -> io::Result<()> {
        self.render_frame();
        let mut stdout = io::stdout().lock();
        stdout.write_all(&self.frame_buf)?;
        stdout.flush()
    }

    fn render_frame(&mut self) {
        self.scroll();

        self.frame_buf.clear();
        // Hide cursor & move to top-left
        self.frame_buf.extend_from_slice(b"\x1b[?25l\x1b[H");

        for y in 0..self.screenrows {
            let filerow = self.rowoff + y;
            if filerow >= self.rows.len() {
                if self.rows.is_empty() && y == self.screenrows / 3 {
                    let welcome = format!("Kilo editor -- version {}", KILO_VERSION);
                    let wlen = welcome.len().min(self.screencols);
                    let mut padding = self.screencols.saturating_sub(wlen) / 2;
                    if padding > 0 {
                        self.frame_buf.push(b'~');
                        padding -= 1;
                    }
                    for _ in 0..padding {
                        self.frame_buf.push(b' ');
                    }
                    self.frame_buf
                        .extend_from_slice(&welcome.as_bytes()[..wlen]);
                } else {
                    self.frame_buf.push(b'~');
                }
            } else {
                let row = &self.rows[filerow];
                let len = row
                    .render
                    .len()
                    .saturating_sub(self.coloff)
                    .min(self.screencols);
                if len > 0 {
                    let mut current_color: Option<u8> = None;
                    for i in 0..len {
                        let idx = self.coloff + i;
                        let b = row.render[idx];
                        let hl = row.hl[idx];

                        if !(32..127).contains(&b) {
                            let sym = if b <= 26 { b'@' + b } else { b'?' };
                            self.frame_buf.extend_from_slice(b"\x1b[7m");
                            self.frame_buf.push(sym);
                            self.frame_buf.extend_from_slice(b"\x1b[0m");
                            if let Some(col) = current_color {
                                let _ = write!(self.frame_buf, "\x1b[{}m", col);
                            }
                        } else if hl == Highlight::Normal {
                            if current_color.is_some() {
                                self.frame_buf.extend_from_slice(b"\x1b[39m");
                                current_color = None;
                            }
                            self.frame_buf.push(b);
                        } else {
                            let color = hl.ansi_color();
                            if current_color != Some(color) {
                                current_color = Some(color);
                                let _ = write!(self.frame_buf, "\x1b[{}m", color);
                            }
                            self.frame_buf.push(b);
                        }
                    }
                    self.frame_buf.extend_from_slice(b"\x1b[39m");
                }
            }

            self.frame_buf.extend_from_slice(b"\x1b[K");
            if y + 1 < self.screenheight {
                self.frame_buf.extend_from_slice(b"\r\n");
            }
        }

        if self.screenheight > 1 {
            // Status bar (inverted colors)
            self.frame_buf.extend_from_slice(b"\x1b[7m");
            let name = self.filename.as_deref().unwrap_or("[No Name]");
            let name =
                String::from_utf8(display_metadata(name.as_bytes(), 20)).expect("ASCII metadata");
            let dirty_str = if self.dirty { "(modified)" } else { "" };
            let left = format!("{:.20} - {} lines {}", name, self.rows.len(), dirty_str);
            let right = format!("{}/{}", self.cy + 1, self.rows.len());

            let mut len = left.len().min(self.screencols);
            self.frame_buf.extend_from_slice(&left.as_bytes()[..len]);
            while len < self.screencols {
                if self.screencols - len == right.len() {
                    self.frame_buf.extend_from_slice(right.as_bytes());
                    break;
                } else {
                    self.frame_buf.push(b' ');
                    len += 1;
                }
            }
            self.frame_buf.extend_from_slice(b"\x1b[0m");
        }
        if self.screenheight > 2 {
            self.frame_buf.extend_from_slice(b"\r\n");
            self.frame_buf.extend_from_slice(b"\x1b[K");
            if !self.statusmsg.is_empty() && self.statusmsg_time.elapsed() < Duration::from_secs(5)
            {
                let mlen = self.statusmsg.len().min(self.screencols);
                self.frame_buf
                    .extend_from_slice(&display_metadata(self.statusmsg.as_bytes(), mlen));
            }
        }

        // Reposition cursor and show it
        let cursor_y = (self.cy - self.rowoff) + 1;
        let cursor_x = (self.rx - self.coloff) + 1;
        let _ = write!(self.frame_buf, "\x1b[{};{}H\x1b[?25h", cursor_y, cursor_x);
    }

    fn move_cursor(&mut self, key: Key) {
        let current_row_len = self.rows.get(self.cy).map_or(0, |r| r.chars.len());
        match key {
            Key::ArrowLeft => {
                if self.cx > 0 {
                    self.cx -= 1;
                } else if self.cy > 0 {
                    self.cy -= 1;
                    self.cx = self.rows[self.cy].chars.len();
                }
            }
            Key::ArrowRight => {
                if self.cy < self.rows.len() {
                    if self.cx < current_row_len {
                        self.cx += 1;
                    } else if self.cx == current_row_len {
                        self.cy += 1;
                        self.cx = 0;
                    }
                }
            }
            Key::ArrowUp => {
                if self.cy > 0 {
                    self.cy -= 1;
                }
            }
            Key::ArrowDown if self.cy < self.rows.len() => self.cy += 1,
            _ => {}
        }

        let new_row_len = self.rows.get(self.cy).map_or(0, |r| r.chars.len());
        if self.cx > new_row_len {
            self.cx = new_row_len;
        }
    }

    fn prompt(
        &mut self,
        template: &str,
        mut callback: Option<&mut PromptCallback<'_>>,
    ) -> io::Result<Option<Vec<u8>>> {
        let mut buf = Vec::new();
        loop {
            if crate::WINCH_FLAG.swap(false, std::sync::atomic::Ordering::Relaxed) {
                self.update_window_size()?;
            }
            self.set_status_message(template.replace("{}", &String::from_utf8_lossy(&buf)));
            self.refresh_screen()?;

            let Some(key) = input::read_key()? else {
                continue;
            };
            match key {
                Key::Del | Key::Backspace | Key::Ctrl(b'h') => {
                    buf.pop();
                }
                Key::Esc => {
                    self.set_status_message("");
                    if let Some(ref mut cb) = callback {
                        cb(self, &buf, key);
                    }
                    return Ok(None);
                }
                Key::Enter => {
                    if !buf.is_empty() {
                        self.set_status_message("");
                        if let Some(ref mut cb) = callback {
                            cb(self, &buf, key);
                        }
                        return Ok(Some(buf));
                    }
                }
                Key::Char(c) if !c.is_ascii_control() => {
                    buf.push(c);
                }
                _ => {}
            }

            if let Some(ref mut cb) = callback {
                cb(self, &buf, key);
            }
        }
    }

    fn find(&mut self) -> io::Result<()> {
        let saved_cx = self.cx;
        let saved_cy = self.cy;
        let saved_coloff = self.coloff;
        let saved_rowoff = self.rowoff;

        let mut last_match: Option<usize> = None;
        let mut direction: isize = 1;
        let mut saved_hl: Option<(usize, Vec<Highlight>)> = None;

        let mut on_find = |ed: &mut Self, query: &[u8], key: Key| {
            if let Some((row_idx, orig_hl)) = saved_hl.take() {
                ed.rows[row_idx].hl = orig_hl;
            }

            if key == Key::Enter || key == Key::Esc {
                last_match = None;
                direction = 1;
                return;
            } else if key == Key::ArrowRight || key == Key::ArrowDown {
                direction = 1;
            } else if key == Key::ArrowLeft || key == Key::ArrowUp {
                direction = -1;
            } else {
                last_match = None;
                direction = 1;
            }

            if query.is_empty() || ed.rows.is_empty() {
                return;
            }
            if last_match.is_none() {
                direction = 1;
            }

            let mut current = last_match.map(|m| m as isize).unwrap_or(-1);
            let num_rows = ed.rows.len() as isize;

            for _ in 0..ed.rows.len() {
                current += direction;
                if current == -1 {
                    current = num_rows - 1;
                } else if current == num_rows {
                    current = 0;
                }

                let idx = current as usize;
                if let Some(match_pos) = ed.rows[idx]
                    .render
                    .windows(query.len())
                    .position(|w| w == query)
                {
                    last_match = Some(idx);
                    ed.cy = idx;
                    ed.cx = ed.rows[idx].rx_to_cx(match_pos);
                    ed.rowoff = ed.rows.len();

                    saved_hl = Some((idx, ed.rows[idx].hl.clone()));
                    let end = (match_pos + query.len()).min(ed.rows[idx].hl.len());
                    for slot in &mut ed.rows[idx].hl[match_pos..end] {
                        *slot = Highlight::Match;
                    }
                    break;
                }
            }
        };

        let result = self.prompt("Search: {} (Use ESC/Arrows/Enter)", Some(&mut on_find));
        // Restore temporary highlighting even if terminal I/O failed.
        if let Some((idx, hl)) = saved_hl.take() {
            self.rows[idx].hl = hl;
        }
        if !matches!(result, Ok(Some(_))) {
            self.cx = saved_cx;
            self.cy = saved_cy;
            self.coloff = saved_coloff;
            self.rowoff = saved_rowoff;
        }
        result.map(|_| ())
    }

    /// Returns `Ok(true)` if the editor should keep running, `Ok(false)` to exit.
    pub fn process_keypress(&mut self, key: Key) -> io::Result<bool> {
        match key {
            Key::Enter => self.insert_newline(),
            Key::Ctrl(b'q') => {
                if self.dirty && self.quit_times > 0 {
                    self.set_status_message(format!(
                        "WARNING!!! File has unsaved changes. Press Ctrl-Q {} more times to quit.",
                        self.quit_times
                    ));
                    self.quit_times -= 1;
                    return Ok(true);
                }
                // Clear screen before exiting
                let mut stdout = io::stdout().lock();
                stdout.write_all(b"\x1b[2J\x1b[H")?;
                stdout.flush()?;
                return Ok(false);
            }
            Key::Ctrl(b's') => self.save()?,
            Key::Ctrl(b'f') => self.find()?,
            Key::Home => self.cx = 0,
            Key::End => {
                if self.cy < self.rows.len() {
                    self.cx = self.rows[self.cy].chars.len();
                }
            }
            Key::Backspace | Key::Ctrl(b'h') => self.del_char(),
            Key::Del => {
                if self.cy < self.rows.len()
                    && (self.cx < self.rows[self.cy].chars.len() || self.cy + 1 < self.rows.len())
                {
                    self.move_cursor(Key::ArrowRight);
                    self.del_char();
                } else if self.cy < self.rows.len() && !self.rows[self.cy].ending.is_empty() {
                    self.rows[self.cy].ending.clear();
                    self.dirty = true;
                }
            }
            Key::PageUp | Key::PageDown => {
                if key == Key::PageUp {
                    self.cy = self.rowoff.min(self.rows.len());
                } else {
                    self.cy = (self.rowoff + self.screenrows - 1).min(self.rows.len());
                }
                let dir = if key == Key::PageUp {
                    Key::ArrowUp
                } else {
                    Key::ArrowDown
                };
                for _ in 0..self.screenrows {
                    self.move_cursor(dir);
                }
            }
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight => {
                self.move_cursor(key);
            }
            Key::Tab => self.insert_char(b'\t'),
            Key::Ctrl(b'l') | Key::Esc => {}
            Key::Char(c) => self.insert_char(c),
            _ => {}
        }

        self.quit_times = KILO_QUIT_TIMES;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn editor() -> Editor {
        Editor {
            cx: 0,
            cy: 0,
            rx: 0,
            rowoff: 0,
            coloff: 0,
            screenrows: 22,
            screencols: 80,
            screenheight: 24,
            rows: Vec::new(),
            dirty: false,
            filename: None,
            statusmsg: String::new(),
            statusmsg_time: Instant::now(),
            syntax: None,
            quit_times: KILO_QUIT_TIMES,
            frame_buf: Vec::new(),
        }
    }

    struct TempFile(std::path::PathBuf);
    impl TempFile {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            Self(std::env::temp_dir().join(format!(
                "kilo-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn file_round_trip_preserves_all_bytes() {
        for contents in [
            b"".as_slice(),
            b"abc",
            b"abc\n",
            b"a\r\nb\n\r",
            b"\n\n",
            b"\xff\0\x1b\n",
        ] {
            let temp = TempFile::new();
            fs::write(&temp.0, contents).unwrap();
            let mut ed = editor();
            ed.open(temp.path()).unwrap();
            ed.save().unwrap();
            assert_eq!(fs::read(&temp.0).unwrap(), contents);
        }
    }

    #[test]
    fn open_errors_preserve_existing_document() {
        let mut ed = editor();
        ed.insert_char(b'x');
        assert!(ed.open(std::env::temp_dir().to_str().unwrap()).is_err());
        assert_eq!(ed.rows[0].chars, b"x");
        assert!(ed.dirty);
        let temp = TempFile::new();
        ed.open(temp.path()).unwrap();
        assert!(ed.rows.is_empty());
        assert!(!ed.dirty);
    }

    #[test]
    fn splitting_and_joining_updates_both_rows() {
        let mut ed = editor();
        ed.syntax = Some(&crate::row::C_SYNTAX);
        ed.rows.push(Row::new(b"/* comment */ int x;".to_vec()));
        ed.recompute_all_rows();
        ed.cx = 3;
        ed.insert_newline();
        assert_eq!(ed.rows[1].render, b"comment */ int x;");
        assert_eq!(ed.rows[1].hl[0], Highlight::MlComment);
        ed.del_char();
        assert_eq!(ed.rows.len(), 1);
        assert_eq!(ed.rows[0].render, b"/* comment */ int x;");
    }

    #[test]
    fn deleting_at_eof_never_deletes_the_last_character() {
        let mut ed = editor();
        ed.insert_char(b'x');
        ed.process_keypress(Key::Del).unwrap();
        assert_eq!(ed.rows[0].chars, b"x");
        ed.rows[0].ending = b"\n".to_vec();
        ed.process_keypress(Key::Del).unwrap();
        assert_eq!(ed.rows[0].chars, b"x");
        assert!(ed.rows[0].ending.is_empty());
    }

    #[test]
    fn backspace_at_virtual_eof_removes_the_newline() {
        let mut ed = editor();
        ed.rows.push(Row::new(b"x".to_vec()));
        ed.cy = 1;
        ed.del_char();
        assert_eq!((ed.cx, ed.cy), (1, 0));
        assert_eq!(ed.rows[0].chars, b"x");
        assert!(ed.rows[0].ending.is_empty());
    }

    #[test]
    fn tiny_terminal_and_control_bytes_render_safely() {
        let mut ed = editor();
        ed.syntax = Some(&crate::row::C_SYNTAX);
        ed.rows.push(Row::new(b"//\x1b[2J\xff".to_vec()));
        ed.filename = Some("\x1b[2J".into());
        ed.set_status_message("\x1b[2J");
        ed.recompute_all_rows();
        for height in 1..=3 {
            ed.screenheight = height;
            ed.screenrows = height.saturating_sub(2).max(1);
            ed.screencols = 1;
            ed.process_keypress(Key::PageDown).unwrap();
            ed.render_frame();
            assert!(!ed.frame_buf.windows(4).any(|w| w == b"\x1b[2J"));
            assert!(ed.cy <= ed.rows.len());
        }
        ed.screencols = 80;
        ed.cy = 0;
        ed.render_frame();
        assert!(!ed.frame_buf.windows(4).any(|w| w == b"\x1b[2J"));
        assert!(!ed.frame_buf.contains(&255));
    }

    #[test]
    fn editing_sequences_keep_cursor_and_highlights_valid() {
        let keys = [
            Key::Char(b'x'),
            Key::Tab,
            Key::Enter,
            Key::Backspace,
            Key::Del,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::ArrowUp,
            Key::ArrowDown,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
        ];
        let mut ed = editor();
        let mut seed = 7_u64;
        for _ in 0..10_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ed.process_keypress(keys[(seed >> 32) as usize % keys.len()])
                .unwrap();
            assert!(ed.cy <= ed.rows.len());
            assert!(ed.cx <= ed.rows.get(ed.cy).map_or(0, |r| r.chars.len()));
            for row in &ed.rows {
                assert_eq!(row.render.len(), row.hl.len());
                let mut expected = Row::new(row.chars.clone());
                expected.update(None, false);
                assert_eq!(row.render, expected.render);
            }
            ed.render_frame();
        }
    }
    #[test]
    fn failed_save_keeps_dirty_buffer_and_original_file() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempFile::new();
        fs::write(&temp.0, b"original").unwrap();
        fs::set_permissions(&temp.0, fs::Permissions::from_mode(0o444)).unwrap();
        let mut ed = editor();
        ed.open(temp.path()).unwrap();
        ed.insert_char(b'x');
        ed.save().unwrap();
        assert!(ed.dirty);
        assert!(ed.statusmsg.contains("Can't save"));
        assert_eq!(fs::read(&temp.0).unwrap(), b"original");
        fs::set_permissions(&temp.0, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn saving_through_symlink_preserves_link_and_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = TempFile::new();
        let link = TempFile::new();
        fs::write(&temp.0, b"original").unwrap();
        fs::set_permissions(&temp.0, fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&temp.0, &link.0).unwrap();
        write_file(link.path(), b"changed").unwrap();
        assert!(
            fs::symlink_metadata(&link.0)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&temp.0).unwrap(), b"changed");
        assert_eq!(
            fs::metadata(&temp.0).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

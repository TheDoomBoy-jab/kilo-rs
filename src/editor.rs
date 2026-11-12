use std::fs;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use crate::input::{self, Key};
use crate::row::{Highlight, Row, SyntaxSpec, select_syntax};
use crate::terminal;

const KILO_VERSION: &str = "0.1.0-rs";
const KILO_QUIT_TIMES: u8 = 3;

pub struct Editor {
    cx: usize,
    cy: usize,
    rx: usize,
    rowoff: usize,
    coloff: usize,
    screenrows: usize,
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
            screenrows: rows.saturating_sub(2),
            screencols: cols,
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
        self.screenrows = rows.saturating_sub(2);
        self.screencols = cols;
        if self.cy >= self.screenrows + self.rowoff {
            self.cy = self.screenrows.saturating_add(self.rowoff).saturating_sub(1);
        }
        if self.cx >= self.screencols + self.coloff {
            self.cx = self.screencols.saturating_add(self.coloff).saturating_sub(1);
        }
        Ok(())
    }

    pub fn set_status_message(&mut self, msg: impl Into<String>) {
        self.statusmsg = msg.into();
        self.statusmsg_time = Instant::now();
    }

    pub fn open(&mut self, filename: &str) -> io::Result<()> {
        self.filename = Some(filename.to_string());
        self.syntax = select_syntax(filename);
        self.rows.clear();

        if let Ok(contents) = fs::read(filename) {
            for line in contents.split(|&b| b == b'\n') {
                let trimmed = line.strip_suffix(b"\r").unwrap_or(line);
                self.rows.push(Row::new(trimmed.to_vec()));
            }
            // If file ends with trailing newline, split() adds a trailing empty slice; keep behavior clean
            if contents.ends_with(b"\n") && self.rows.len() > 1 {
                self.rows.pop();
            }
            self.recompute_all_rows();
        }
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
        let mut idx = start_idx;
        while idx < self.rows.len() {
            let prev_open = if idx > 0 {
                self.rows[idx - 1].hl_open_comment
            } else {
                false
            };
            let changed = self.rows[idx].update(self.syntax, prev_open);
            if !changed {
                break;
            }
            idx += 1;
        }
    }

    fn save(&mut self) {
        if self.filename.is_none() {
            if let Some(name) = self.prompt("Save as: {} (ESC to cancel)", None) {
                self.syntax = select_syntax(&name);
                self.filename = Some(name);
                self.recompute_all_rows();
            } else {
                self.set_status_message("Save aborted");
                return;
            }
        }

        let Some(ref path) = self.filename else { return };
        let total_bytes: usize = self.rows.iter().map(|r| r.chars.len() + 1).sum();
        let mut out = Vec::with_capacity(total_bytes);
        for row in &self.rows {
            out.extend_from_slice(&row.chars);
            out.push(b'\n');
        }

        match fs::write(path, &out) {
            Ok(()) => {
                self.dirty = false;
                self.set_status_message(format!("{} bytes written on disk", out.len()));
            }
            Err(e) => {
                self.set_status_message(format!("Can't save! I/O error: {}", e));
            }
        }
    }

    fn insert_char(&mut self, c: u8) {
        if self.cy == self.rows.len() {
            self.rows.push(Row::new(Vec::new()));
        }
        let cy = self.cy;
        self.rows[cy].insert_char(self.cx, c);
        self.update_row_from(cy);
        self.cx += 1;
        self.dirty = true;
    }

    fn insert_newline(&mut self) {
        if self.cx == 0 {
            self.rows.insert(self.cy, Row::new(Vec::new()));
        } else {
            let tail = self.rows[self.cy].chars.split_off(self.cx);
            self.rows.insert(self.cy + 1, Row::new(tail));
        }
        self.update_row_from(self.cy);
        self.cy += 1;
        self.cx = 0;
        self.dirty = true;
    }

    fn del_char(&mut self) {
        if self.cy == self.rows.len() || (self.cx == 0 && self.cy == 0) {
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
                    self.frame_buf.extend_from_slice(&welcome.as_bytes()[..wlen]);
                } else {
                    self.frame_buf.push(b'~');
                }
            } else {
                let row = &self.rows[filerow];
                let len = row.render.len().saturating_sub(self.coloff).min(self.screencols);
                if len > 0 {
                    let mut current_color: Option<u8> = None;
                    for i in 0..len {
                        let idx = self.coloff + i;
                        let b = row.render[idx];
                        let hl = row.hl[idx];

                        if hl == Highlight::NonPrint {
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

            self.frame_buf.extend_from_slice(b"\x1b[K\r\n");
        }

        // Status bar (inverted colors)
        self.frame_buf.extend_from_slice(b"\x1b[7m");
        let name = self.filename.as_deref().unwrap_or("[No Name]");
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
        self.frame_buf.extend_from_slice(b"\x1b[0m\r\n");

        // Message bar
        self.frame_buf.extend_from_slice(b"\x1b[K");
        if !self.statusmsg.is_empty() && self.statusmsg_time.elapsed() < Duration::from_secs(5) {
            let mlen = self.statusmsg.len().min(self.screencols);
            self.frame_buf.extend_from_slice(&self.statusmsg.as_bytes()[..mlen]);
        }

        // Reposition cursor and show it
        let cursor_y = (self.cy - self.rowoff) + 1;
        let cursor_x = (self.rx - self.coloff) + 1;
        let _ = write!(self.frame_buf, "\x1b[{};{}H\x1b[?25h", cursor_y, cursor_x);

        let mut stdout = io::stdout().lock();
        stdout.write_all(&self.frame_buf)?;
        stdout.flush()
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
            Key::ArrowDown => {
                if self.cy < self.rows.len() {
                    self.cy += 1;
                }
            }
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
        mut callback: Option<&mut dyn FnMut(&mut Self, &str, Key)>,
    ) -> Option<String> {
        let mut buf = String::new();
        loop {
            self.set_status_message(template.replace("{}", &buf));
            let _ = self.refresh_screen();

            let Ok(key) = input::read_key() else { return None };
            match key {
                Key::Del | Key::Backspace | Key::Ctrl(b'h') => {
                    buf.pop();
                }
                Key::Esc => {
                    self.set_status_message("");
                    if let Some(ref mut cb) = callback {
                        cb(self, &buf, key);
                    }
                    return None;
                }
                Key::Enter => {
                    if !buf.is_empty() {
                        self.set_status_message("");
                        if let Some(ref mut cb) = callback {
                            cb(self, &buf, key);
                        }
                        return Some(buf);
                    }
                }
                Key::Char(c) if !c.is_ascii_control() => {
                    buf.push(c as char);
                }
                _ => {}
            }

            if let Some(ref mut cb) = callback {
                cb(self, &buf, key);
            }
        }
    }

    fn find(&mut self) {
        let saved_cx = self.cx;
        let saved_cy = self.cy;
        let saved_coloff = self.coloff;
        let saved_rowoff = self.rowoff;

        let mut last_match: Option<usize> = None;
        let mut direction: isize = 1;
        let mut saved_hl: Option<(usize, Vec<Highlight>)> = None;

        let mut on_find = |ed: &mut Self, query: &str, key: Key| {
            if let Some((row_idx, orig_hl)) = saved_hl.take() {
                if row_idx < ed.rows.len() {
                    ed.rows[row_idx].hl = orig_hl;
                }
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
                let row_render = String::from_utf8_lossy(&ed.rows[idx].render);
                if let Some(match_pos) = row_render.find(query) {
                    last_match = Some(idx);
                    ed.cy = idx;
                    ed.cx = match_pos;
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

        if self.prompt("Search: {} (Use ESC/Arrows/Enter)", Some(&mut on_find)).is_none() {
            self.cx = saved_cx;
            self.cy = saved_cy;
            self.coloff = saved_coloff;
            self.rowoff = saved_rowoff;
        }
    }

    /// Returns `Ok(true)` if the editor should keep running, `Ok(false)` to exit.
    pub fn process_keypress(&mut self) -> io::Result<bool> {
        let key = input::read_key()?;

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
            Key::Ctrl(b's') => self.save(),
            Key::Ctrl(b'f') => self.find(),
            Key::Home => self.cx = 0,
            Key::End => {
                if self.cy < self.rows.len() {
                    self.cx = self.rows[self.cy].chars.len();
                }
            }
            Key::Backspace | Key::Ctrl(b'h') => self.del_char(),
            Key::Del => {
                self.move_cursor(Key::ArrowRight);
                self.del_char();
            }
            Key::PageUp | Key::PageDown => {
                if key == Key::PageUp {
                    self.cy = self.rowoff;
                } else {
                    self.cy = (self.rowoff + self.screenrows - 1).min(self.rows.len());
                }
                let dir = if key == Key::PageUp { Key::ArrowUp } else { Key::ArrowDown };
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

pub const TAB_STOP: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Highlight {
    Normal = 0,
    NonPrint = 1,
    Comment = 2,
    MlComment = 3,
    Keyword1 = 4,
    Keyword2 = 5,
    String = 6,
    Number = 7,
    Match = 8,
}

impl Highlight {
    pub fn ansi_color(self) -> u8 {
        match self {
            Highlight::Comment | Highlight::MlComment => 36, // cyan
            Highlight::Keyword1 => 33,                       // yellow
            Highlight::Keyword2 => 32,                       // green
            Highlight::String => 35,                         // magenta
            Highlight::Number => 31,                         // red
            Highlight::Match => 34,                          // blue
            Highlight::Normal | Highlight::NonPrint => 37,   // white/default
        }
    }
}

pub struct SyntaxSpec {
    pub extensions: &'static [&'static str],
    pub keywords: &'static [&'static str],
    pub singleline_comment_start: &'static str,
    pub multiline_comment_start: &'static str,
    pub multiline_comment_end: &'static str,
    pub highlight_numbers: bool,
    pub highlight_strings: bool,
}

pub const C_SYNTAX: SyntaxSpec = SyntaxSpec {
    extensions: &[".c", ".h", ".cpp", ".hpp", ".cc", ".rs"],
    keywords: &[
        // Primary keywords
        "switch",
        "if",
        "while",
        "for",
        "break",
        "continue",
        "return",
        "else",
        "struct",
        "union",
        "typedef",
        "static",
        "enum",
        "class",
        "fn",
        "let",
        "mut",
        "pub",
        "impl",
        "match",
        "loop",
        "use",
        "mod",
        "crate",
        // Secondary type keywords (marked with trailing '|')
        "int|",
        "long|",
        "double|",
        "float|",
        "char|",
        "unsigned|",
        "signed|",
        "void|",
        "usize|",
        "isize|",
        "u8|",
        "i32|",
        "u32|",
        "bool|",
        "String|",
    ],
    singleline_comment_start: "//",
    multiline_comment_start: "/*",
    multiline_comment_end: "*/",
    highlight_numbers: true,
    highlight_strings: true,
};

pub fn select_syntax(filename: &str) -> Option<&'static SyntaxSpec> {
    for ext in C_SYNTAX.extensions {
        if filename.ends_with(ext) {
            return Some(&C_SYNTAX);
        }
    }
    None
}

fn is_separator(b: u8) -> bool {
    b.is_ascii_whitespace() || b == b'\0' || b",.()+-/*=~%<>[];{}:!&|^?".contains(&b)
}

pub struct Row {
    pub chars: Vec<u8>,
    pub ending: Vec<u8>,
    pub render: Vec<u8>,
    pub hl: Vec<Highlight>,
    pub hl_open_comment: bool,
}

impl Row {
    pub fn new(chars: Vec<u8>) -> Self {
        Self {
            chars,
            ending: b"\n".to_vec(),
            render: Vec::new(),
            hl: Vec::new(),
            hl_open_comment: false,
        }
    }

    pub fn update(&mut self, syntax: Option<&SyntaxSpec>, prev_open_comment: bool) -> bool {
        self.render.clear();
        for &b in &self.chars {
            if b == b'\t' {
                let spaces = TAB_STOP - self.render.len() % TAB_STOP;
                self.render.resize(self.render.len() + spaces, b' ');
            } else {
                self.render.push(b);
            }
        }
        self.update_syntax(syntax, prev_open_comment)
    }

    pub fn cx_to_rx(&self, cx: usize) -> usize {
        let mut rx = 0;
        for &b in self.chars.iter().take(cx) {
            if b == b'\t' {
                rx += (TAB_STOP - 1) - (rx % TAB_STOP);
            }
            rx += 1;
        }
        rx
    }

    pub fn rx_to_cx(&self, rx: usize) -> usize {
        let mut rendered = 0;
        for (cx, &b) in self.chars.iter().enumerate() {
            rendered += if b == b'\t' {
                TAB_STOP - rendered % TAB_STOP
            } else {
                1
            };
            if rendered > rx {
                return cx;
            }
        }
        self.chars.len()
    }

    pub fn insert_char(&mut self, at: usize, c: u8) {
        let idx = at.min(self.chars.len());
        self.chars.insert(idx, c);
    }

    pub fn delete_char(&mut self, at: usize) {
        if at < self.chars.len() {
            self.chars.remove(at);
        }
    }

    fn update_syntax(&mut self, syntax: Option<&SyntaxSpec>, prev_open_comment: bool) -> bool {
        self.hl.clear();
        self.hl.resize(self.render.len(), Highlight::Normal);

        for (i, &b) in self.render.iter().enumerate() {
            if !(32..127).contains(&b) {
                self.hl[i] = Highlight::NonPrint;
            }
        }

        let Some(syn) = syntax else {
            let changed = self.hl_open_comment;
            self.hl_open_comment = false;
            return changed;
        };

        let scs = syn.singleline_comment_start.as_bytes();
        let mcs = syn.multiline_comment_start.as_bytes();
        let mce = syn.multiline_comment_end.as_bytes();

        let mut prev_sep = true;
        let mut in_string: Option<u8> = None;
        let mut in_comment = prev_open_comment;

        let mut i = 0;
        let rlen = self.render.len();
        while i < rlen {
            let c = self.render[i];

            if in_string.is_none()
                && !scs.is_empty()
                && !in_comment
                && self.render[i..].starts_with(scs)
            {
                for slot in &mut self.hl[i..] {
                    *slot = Highlight::Comment;
                }
                break;
            }

            if !mcs.is_empty() && !mce.is_empty() && in_string.is_none() {
                if in_comment {
                    self.hl[i] = Highlight::MlComment;
                    if self.render[i..].starts_with(mce) {
                        for k in 0..mce.len() {
                            self.hl[i + k] = Highlight::MlComment;
                        }
                        i += mce.len();
                        in_comment = false;
                        prev_sep = true;
                        continue;
                    } else {
                        i += 1;
                        continue;
                    }
                } else if self.render[i..].starts_with(mcs) {
                    for k in 0..mcs.len() {
                        self.hl[i + k] = Highlight::MlComment;
                    }
                    i += mcs.len();
                    in_comment = true;
                    continue;
                }
            }

            if syn.highlight_strings {
                if let Some(quote) = in_string {
                    self.hl[i] = Highlight::String;
                    if c == b'\\' && i + 1 < rlen {
                        self.hl[i + 1] = Highlight::String;
                        i += 2;
                        continue;
                    }
                    if c == quote {
                        in_string = None;
                    }
                    i += 1;
                    prev_sep = true;
                    continue;
                } else if c == b'"' || c == b'\'' {
                    in_string = Some(c);
                    self.hl[i] = Highlight::String;
                    i += 1;
                    continue;
                }
            }

            if syn.highlight_numbers {
                let prev_hl = if i > 0 {
                    self.hl[i - 1]
                } else {
                    Highlight::Normal
                };
                if (c.is_ascii_digit() && (prev_sep || prev_hl == Highlight::Number))
                    || (c == b'.' && prev_hl == Highlight::Number)
                {
                    self.hl[i] = Highlight::Number;
                    i += 1;
                    prev_sep = false;
                    continue;
                }
            }

            if prev_sep {
                let mut matched_kw = false;
                for &kw in syn.keywords {
                    let is_kw2 = kw.ends_with('|');
                    let kw_bytes = if is_kw2 {
                        &kw.as_bytes()[..kw.len() - 1]
                    } else {
                        kw.as_bytes()
                    };
                    if self.render[i..].starts_with(kw_bytes) {
                        let after = i + kw_bytes.len();
                        if after == rlen || is_separator(self.render[after]) {
                            let hl_type = if is_kw2 {
                                Highlight::Keyword2
                            } else {
                                Highlight::Keyword1
                            };
                            for slot in &mut self.hl[i..after] {
                                *slot = hl_type;
                            }
                            i = after;
                            matched_kw = true;
                            break;
                        }
                    }
                }
                if matched_kw {
                    prev_sep = false;
                    continue;
                }
            }

            prev_sep = is_separator(c);
            i += 1;
        }

        let changed = self.hl_open_comment != in_comment;
        self.hl_open_comment = in_comment;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_coordinates_round_trip() {
        let mut row = Row::new(b"\ta\tb".to_vec());
        row.update(None, false);
        assert_eq!(row.render, b"        a       b");
        for cx in 0..=row.chars.len() {
            assert_eq!(row.rx_to_cx(row.cx_to_rx(cx)), cx);
        }
        assert_eq!(row.rx_to_cx(7), 0);
        assert_eq!(row.rx_to_cx(16), 3);
    }

    #[test]
    fn removing_syntax_clears_multiline_state() {
        let mut row = Row::new(b"/* open".to_vec());
        assert!(row.update(Some(&C_SYNTAX), false));
        assert!(row.hl_open_comment);
        assert!(row.update(None, false));
        assert!(!row.hl_open_comment);
        assert!(row.hl.iter().all(|&hl| hl == Highlight::Normal));
    }

    #[test]
    fn comments_strings_and_keyword_boundaries() {
        let mut row = Row::new(b"{return} \"// string\" // comment".to_vec());
        row.update(Some(&C_SYNTAX), false);
        assert!(row.hl[1..7].iter().all(|&h| h == Highlight::Keyword1));
        assert_eq!(row.hl[10], Highlight::String);
        assert_eq!(row.hl.last(), Some(&Highlight::Comment));
        let mut next = Row::new(b"still comment */ let x = 42;".to_vec());
        next.update(Some(&C_SYNTAX), true);
        assert_eq!(next.hl[0], Highlight::MlComment);
        assert!(!next.hl_open_comment);
        assert_eq!(next.hl[17], Highlight::Keyword1);
        assert_eq!(next.hl[25], Highlight::Number);
    }
}

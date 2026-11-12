use std::io::{self, Read};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(u8),
    Ctrl(u8),
    Enter,
    Tab,
    Backspace,
    Esc,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Del,
    Home,
    End,
    PageUp,
    PageDown,
}

fn read_byte() -> io::Result<Option<u8>> {
    let mut buf = [0u8; 1];
    loop {
        match io::stdin().read(&mut buf) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(buf[0])),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Reads a single logical key event from stdin, decoding multi-byte VT100 escape sequences.
pub fn read_key() -> io::Result<Key> {
    let first = loop {
        if let Some(b) = read_byte()? {
            break b;
        }
    };

    if first == b'\x1b' {
        let Some(seq0) = read_byte()? else {
            return Ok(Key::Esc);
        };
        let Some(seq1) = read_byte()? else {
            return Ok(Key::Esc);
        };

        if seq0 == b'[' {
            if seq1.is_ascii_digit() {
                let Some(seq2) = read_byte()? else {
                    return Ok(Key::Esc);
                };
                if seq2 == b'~' {
                    return Ok(match seq1 {
                        b'1' | b'7' => Key::Home,
                        b'3' => Key::Del,
                        b'4' | b'8' => Key::End,
                        b'5' => Key::PageUp,
                        b'6' => Key::PageDown,
                        _ => Key::Esc,
                    });
                }
            } else {
                return Ok(match seq1 {
                    b'A' => Key::ArrowUp,
                    b'B' => Key::ArrowDown,
                    b'C' => Key::ArrowRight,
                    b'D' => Key::ArrowLeft,
                    b'H' => Key::Home,
                    b'F' => Key::End,
                    _ => Key::Esc,
                });
            }
        } else if seq0 == b'O' {
            return Ok(match seq1 {
                b'H' => Key::Home,
                b'F' => Key::End,
                _ => Key::Esc,
            });
        }
        return Ok(Key::Esc);
    }

    Ok(match first {
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        127 | 8 => Key::Backspace,
        1..=26 => Key::Ctrl(first + b'a' - 1),
        c => Key::Char(c),
    })
}

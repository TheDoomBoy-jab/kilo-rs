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
    match io::stdin().read(&mut buf) {
        // VMIN=0 / VTIME=1: return to the event loop on timeout or SIGWINCH.
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(buf[0])),
        Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(None),
        Err(e) => Err(e),
    }
}

/// Returns None on the raw terminal's read timeout so the UI can handle resize events.
pub fn read_key() -> io::Result<Option<Key>> {
    decode_key(read_byte)
}

fn decode_key(mut read: impl FnMut() -> io::Result<Option<u8>>) -> io::Result<Option<Key>> {
    let Some(first) = read()? else {
        return Ok(None);
    };
    if first == b'\x1b' {
        let Some(prefix) = read()? else {
            return Ok(Some(Key::Esc));
        };
        if prefix != b'[' && prefix != b'O' {
            return Ok(Some(Key::Esc));
        }
        // Consume an entire CSI/SS3 sequence, including modifier parameters.
        // Otherwise a sequence such as ESC [ 1 ; 5 C inserts "5C" into the file.
        let mut parameters = Vec::new();
        for _ in 0..32 {
            let Some(byte) = read()? else {
                return Ok(Some(Key::Esc));
            };
            if (0x40..=0x7e).contains(&byte) {
                let key = match byte {
                    b'A' => Key::ArrowUp,
                    b'B' => Key::ArrowDown,
                    b'C' => Key::ArrowRight,
                    b'D' => Key::ArrowLeft,
                    b'H' => Key::Home,
                    b'F' => Key::End,
                    b'~' => match parameters.split(|&b| b == b';').next().unwrap_or_default() {
                        b"1" | b"7" => Key::Home,
                        b"3" => Key::Del,
                        b"4" | b"8" => Key::End,
                        b"5" => Key::PageUp,
                        b"6" => Key::PageDown,
                        _ => Key::Esc,
                    },
                    _ => Key::Esc,
                };
                return Ok(Some(key));
            }
            if !(0x20..=0x3f).contains(&byte) {
                break;
            }
            parameters.push(byte);
        }
        return Ok(Some(Key::Esc));
    }
    Ok(Some(match first {
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        127 | 8 => Key::Backspace,
        1..=26 => Key::Ctrl(first + b'a' - 1),
        c => Key::Char(c),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Option<Key> {
        let mut iter = bytes.iter().copied();
        decode_key(|| Ok(iter.next())).unwrap()
    }

    #[test]
    fn decodes_keys_and_timeouts() {
        for (bytes, key) in [
            (b"a".as_slice(), Key::Char(b'a')),
            (b"\x13", Key::Ctrl(b's')),
            (b"\t", Key::Tab),
            (b"\r", Key::Enter),
            (b"\x7f", Key::Backspace),
            (b"\x1b", Key::Esc),
            (b"\x1b[A", Key::ArrowUp),
            (b"\x1bOD", Key::ArrowLeft),
            (b"\x1b[6~", Key::PageDown),
            (b"\x1b[1;5C", Key::ArrowRight),
            (b"\x1b[3;2~", Key::Del),
        ] {
            assert_eq!(decode(bytes), Some(key));
        }
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"\x1b["), Some(Key::Esc));
    }

    #[test]
    fn consumes_unknown_sequences_without_inserting_their_suffix() {
        let mut bytes = b"\x1b[15~z".iter().copied();
        assert_eq!(decode_key(|| Ok(bytes.next())).unwrap(), Some(Key::Esc));
        assert_eq!(
            decode_key(|| Ok(bytes.next())).unwrap(),
            Some(Key::Char(b'z'))
        );
    }
}

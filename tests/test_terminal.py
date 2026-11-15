"""Run after cargo build; exercises the actual editor through POSIX terminals."""

from pathlib import Path
import subprocess
import tempfile
import termios
import unittest

from benchmark import PtySession, benchmark_binary

BINARY = Path(__file__).resolve().parents[1] / "target/debug/kilo-rs"


class TerminalTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "sample.c"

    def session(self, contents=b"", **kwargs):
        self.path.write_bytes(contents)
        session = PtySession(BINARY, self.path, **kwargs)
        self.addCleanup(session.close)
        session.frame()
        return session

    def key(self, session, key):
        session.send(key)
        return session.frame()

    def quit(self, session):
        session.send(b"\x11" * 4)
        self.assertEqual(session.wait_exit(), 0)
        self.assertEqual(termios.tcgetattr(session.slave), session.original_attributes)

    def test_search_after_tabs_then_edit(self):
        session = self.session(b"\tneedle\n")
        self.key(session, b"\x06")
        for byte in b"needle":
            self.key(session, bytes([byte]))
        self.key(session, b"\r")
        self.key(session, b"X")
        self.key(session, b"\x13")
        self.assertEqual(self.path.read_bytes(), b"\tXneedle\n")
        self.quit(session)

    def test_search_with_invalid_utf8_prefix(self):
        session = self.session(b"\xffneedle")
        self.key(session, b"\x06")
        for byte in b"needle":
            self.key(session, bytes([byte]))
        self.key(session, b"\r")
        self.key(session, b"X")
        self.key(session, b"\x13")
        self.assertEqual(self.path.read_bytes(), b"\xffXneedle")
        self.quit(session)

    def test_delete_at_end_and_modified_navigation(self):
        session = self.session(b"abc")
        self.key(session, b"\x1b[F")
        self.key(session, b"\x1b[3~")
        self.key(session, b"\x1b[1;5D")
        self.key(session, b"X")
        self.key(session, b"\x13")
        self.assertEqual(self.path.read_bytes(), b"abXc")
        self.quit(session)

    def test_idle_resize_preserves_document_position(self):
        session = self.session(b"abcdef\nsecond\n")
        self.key(session, b"\x1b[F")
        session.resize(1, 1)
        session.frame()  # No keypress: SIGWINCH must redraw the idle editor.
        self.key(session, b"X")
        session.resize(24, 80)
        session.frame()
        self.key(session, b"\x13")
        self.assertEqual(self.path.read_bytes(), b"abcdefX\nsecond\n")
        self.quit(session)

    def test_control_bytes_in_comments_are_not_executed(self):
        self.path.write_bytes(b"// \x1b[2J\x1b]0;BAD\x07\n")
        with PtySession(BINARY, self.path) as session:
            frame = session.frame()
            self.assertNotIn(b"\x1b[2J", frame)
            self.assertNotIn(b"\x1b]0;BAD", frame)
            self.quit(session)

    def test_save_preserves_empty_mixed_and_missing_line_endings(self):
        for content in (b"", b"abc", b"a\r\nb\nlast\r"):
            with self.subTest(content=content):
                session = self.session(content)
                self.key(session, b"\x13")
                self.assertEqual(self.path.read_bytes(), content)
                self.quit(session)

    def test_directory_open_fails_and_restores_terminal(self):
        with PtySession(BINARY, self.directory.name) as session:
            self.assertNotEqual(session.wait_exit(), 0)
            self.assertEqual(termios.tcgetattr(session.slave), session.original_attributes)

    def test_nonterminal_input_fails_without_hanging(self):
        result = subprocess.run([BINARY, self.path], input=b"", capture_output=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"stdin is not a terminal", result.stderr)

    def test_benchmark_waits_for_complete_frames(self):
        self.path.write_bytes(b"line\n" * 50)
        self.assertGreater(benchmark_binary(BINARY, self.path, 10), 0)

    def test_benchmark_reports_failed_child(self):
        with self.assertRaises(RuntimeError):
            benchmark_binary("/usr/bin/false", self.path, 1)


if __name__ == "__main__":
    unittest.main()

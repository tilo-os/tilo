"""Exercise the production flight recorder's atomic publication under overlap."""
import importlib.util
import os
from pathlib import Path
import tempfile
import errno
import subprocess
import sys
import time
import unittest
from unittest.mock import patch

RECORDER = Path(__file__).resolve().parents[2] / "dist/session/tilo-flight-recorder.py"
SPEC = importlib.util.spec_from_file_location(
    "flight_recorder", Path(__file__).resolve().parents[2] / "dist/session/tilo-flight-recorder.py"
)
recorder = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(recorder)


class FlightRecorderPublication(unittest.TestCase):
    def test_overlapping_publications_keep_their_own_temporary_inode(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "latest.log"
            replace = os.replace
            overlapped = False

            def overlapping_replace(source, destination):
                nonlocal overlapped
                if not overlapped:
                    overlapped = True
                    recorder.persist(path, bytearray(b"second recorder"))
                    self.assertEqual(path.read_bytes(), b"second recorder")
                replace(source, destination)

            with patch.object(recorder.os, "replace", side_effect=overlapping_replace):
                recorder.persist(path, bytearray(b"first recorder"))
            self.assertEqual(path.read_bytes(), b"first recorder")
            self.assertEqual(list(Path(directory).iterdir()), [path])

    def test_published_record_is_private_even_with_permissive_umask(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "latest.log"
            previous = os.umask(0)
            try:
                recorder.persist(path, bytearray(b"private diagnostics"))
            finally:
                os.umask(previous)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_failed_publication_preserves_previous_record_and_cleans_staging(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "latest.log"
            path.write_bytes(b"previous record")
            with patch.object(recorder.os, "replace", side_effect=OSError("publication refused")):
                self.assertFalse(recorder.persist(path, bytearray(b"new record")))
            self.assertEqual(path.read_bytes(), b"previous record")
            self.assertEqual(list(Path(directory).iterdir()), [path])


    def test_a_full_disk_never_raises_out_of_persist(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "latest.log"
            full = OSError(errno.ENOSPC, "No space left on device")
            with patch.object(recorder.os, "fsync", side_effect=full):
                self.assertFalse(recorder.persist(path, bytearray(b"record")))
            self.assertFalse(path.exists())
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_the_relay_keeps_running_while_storage_refuses_every_publication(self):
        # The compositor's stderr is this relay. If storage failures ended it,
        # the compositor's next log line would hit a closed pipe and abort it.
        with tempfile.TemporaryDirectory() as directory:
            gone = Path(directory) / "removed"
            gone.mkdir()
            path = gone / "latest.log"
            process = subprocess.Popen(
                [sys.executable, str(RECORDER), str(path), "now", "1", "/usr/bin/shell"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
            )
            assert process.stdin is not None and process.stdout is not None
            deadline = time.monotonic() + 60
            while not path.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            path.unlink()
            gone.rmdir()  # every later publication now fails inside mkstemp
            sent = bytearray()
            for line in range(200):
                chunk = f"log line {line} after storage went away\n".encode()
                process.stdin.write(chunk)
                process.stdin.flush()
                sent.extend(chunk)
                time.sleep(0.002)
            process.stdin.close()
            relayed = process.stdout.read()
            self.assertEqual(process.wait(timeout=60), 0)
            self.assertEqual(relayed, bytes(sent))


if __name__ == "__main__":
    unittest.main()

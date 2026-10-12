"""Exercise the production flight recorder's atomic publication under overlap."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

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
                with self.assertRaisesRegex(OSError, "publication refused"):
                    recorder.persist(path, bytearray(b"new record"))
            self.assertEqual(path.read_bytes(), b"previous record")
            self.assertEqual(list(Path(directory).iterdir()), [path])


if __name__ == "__main__":
    unittest.main()

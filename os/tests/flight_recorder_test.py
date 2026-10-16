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
            for record in gone.iterdir():  # latest.log and events.log
                record.unlink()
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


# What the compositor writes: `tracing` records among the periodic measurement
# stream and relayed app output.
LAUNCH = (
    b"\x1b[2m2026-10-11T02:22:31.204118Z\x1b[0m \x1b[32m INFO\x1b[0m "
    b"\x1b[2mtilo::launch_latency\x1b[0m\x1b[2m:\x1b[0m application launch requested "
    b"package_id=com.onepassword.OnePassword\n"
)
XWAYLAND = (
    b"2026-10-11T02:22:31.981004Z  INFO tilo::xwayland: starting Xwayland bridge for its "
    b"first client display=:1\n"
)
PAINT_STATS = (
    b"paint-stats: 3/14 painted | events=0 revision=0 anim=0 session=0 keepalive=0 | "
    b"locked=false blanked=true | anim_sources host=0x0000 host_labels=\"\" | 60.000 Hz target\n"
)
FRAME_LIFECYCLE = (
    b"FRAME-LIFECYCLE serial=183875 event=render-retired queue_wait_us=9 render_us=36917\n"
)
APP_STDERR = b"[app com.onepassword.OnePassword stderr] INFO  2026-10-11T02:22:33.1+00:00 started\n"


class EventSelection(unittest.TestCase):
    def test_tracing_records_are_events_and_stream_output_is_not(self):
        for line in (LAUNCH, XWAYLAND, b"2026-10-11T02:22:31Z ERROR tilo::app_launch: stopped\n"):
            self.assertTrue(recorder.is_event(line), line)
        for line in (PAINT_STATS, FRAME_LIFECYCLE, APP_STDERR, b"\n", b"INFO no timestamp\n"):
            self.assertFalse(recorder.is_event(line), line)

    def test_a_record_split_across_reads_is_kept_whole_once(self):
        stream = PAINT_STATS + LAUNCH + FRAME_LIFECYCLE + XWAYLAND
        for cut in range(1, len(stream)):
            selection = recorder.EventFilter()
            picked = selection.feed(stream[:cut]) + selection.feed(stream[cut:])
            self.assertEqual(picked + selection.finish(), LAUNCH + XWAYLAND, cut)

    def test_an_unterminated_last_record_is_kept_at_end_of_stream(self):
        selection = recorder.EventFilter()
        self.assertEqual(selection.feed(XWAYLAND[:-1]), b"")
        self.assertEqual(selection.finish(), XWAYLAND)

    def test_an_endless_line_holds_a_bounded_prefix(self):
        selection = recorder.EventFilter(limit=1024)
        for _ in range(64):
            self.assertEqual(selection.feed(b"x" * 4096), b"")
        self.assertLessEqual(len(selection.partial), 1024)


class EventRetention(unittest.TestCase):
    def test_launch_and_xwayland_lines_outlive_hours_of_measurement_lines(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            events = Path(directory) / "events.log"
            measurement = (PAINT_STATS * 2 + FRAME_LIFECYCLE + APP_STDERR) * 64
            # More than the full record's bound after the events, as a session
            # that has been up for an afternoon writes.
            filler = measurement * (recorder.LOG_LIMIT // len(measurement) + 2)
            stream = LAUNCH + XWAYLAND + filler
            result = subprocess.run(
                [sys.executable, "-I", SPEC.origin, str(log), "now", "42", "/usr/bin/shell"],
                input=stream,
                stdout=subprocess.PIPE,
                check=True,
                timeout=120,
            )
            self.assertEqual(result.stdout, stream, "every byte is still relayed")
            self.assertEqual(log.stat().st_size, recorder.LOG_LIMIT)
            self.assertNotIn(b"launch_latency", log.read_bytes(), "the full record lost it")
            header = b"tilo-flight-record-v1 start=now pid=42 shell=/usr/bin/shell\n"
            self.assertEqual(events.read_bytes(), header + LAUNCH + XWAYLAND)
            self.assertEqual(events.stat().st_mode & 0o777, 0o600)

    def test_the_event_record_keeps_its_own_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            events = Path(directory) / "events.log"
            count = recorder.LOG_LIMIT // len(XWAYLAND) + 100
            stream = (XWAYLAND + PAINT_STATS) * count
            subprocess.run(
                [sys.executable, "-I", SPEC.origin, str(log)],
                input=stream,
                stdout=subprocess.DEVNULL,
                check=True,
                timeout=120,
            )
            kept = events.read_bytes()
            self.assertEqual(len(kept), recorder.LOG_LIMIT)
            self.assertTrue(kept.endswith(XWAYLAND))
            self.assertEqual(log.stat().st_size, recorder.LOG_LIMIT)


if __name__ == "__main__":
    unittest.main()

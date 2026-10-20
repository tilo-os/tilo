"""Owned listing processes never replace the production Cargo test verdict."""

import json
import os
import signal
import socket
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

from tools.ci.prewarm_test_binaries import Harness, harnesses, listing_environment, prewarm
from hang import HANG_BOUND


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
import t_matrix  # noqa: E402


class PrewarmTests(unittest.TestCase):
    def fixture(self, root, name="entry", body="pass", harness=True):
        source, target = root / "source", root / "target"
        source.mkdir(exist_ok=True)
        target.mkdir(exist_ok=True)
        manifest = source / "Cargo.toml"
        manifest.write_text('[package]\nname="fixture"\nversion="0.1.0"\n'
                            f'[lib]\nharness={str(harness).lower()}\n')
        executable = target / name
        executable.write_text(f"#!{sys.executable}\nimport sys, os, time, json\n{body}\n")
        executable.chmod(0o700)
        row = {"reason": "compiler-artifact", "executable": str(executable),
               "manifest_path": str(manifest), "profile": {"test": True},
               "target": {"kind": ["lib"], "name": "fixture", "src_path": str(source / "lib.rs")}}
        return source, target, row

    def test_only_standard_compiled_test_harnesses_are_selected_once(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, target, row = self.fixture(root)
            artifacts = root / "artifacts.jsonl"
            ordinary = {**row, "profile": {"test": False}}
            artifacts.write_text("\n".join(json.dumps(x) for x in [row, ordinary, row]) + "\n")
            self.assertEqual(harnesses(artifacts, source, target),
                             [Harness(Path(row["executable"]), source)])
            self.fixture(root, harness=False)
            self.assertEqual(harnesses(artifacts, source, target), [])

    def test_manifest_named_custom_integration_harness_is_not_launched(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, target, row = self.fixture(root)
            (source / "Cargo.toml").write_text('[[test]]\nname="fixture"\nharness=false\n')
            row["target"]["kind"] = ["test"]
            artifacts = root / "artifacts.jsonl"
            artifacts.write_text(json.dumps(row) + "\n")
            self.assertEqual(harnesses(artifacts, source, target), [])

    def test_artifact_escape_or_unavailable_metadata_fails_observation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, target, row = self.fixture(root)
            artifacts = root / "artifacts.jsonl"
            row["executable"] = sys.executable
            artifacts.write_text(json.dumps(row) + "\n")
            with self.assertRaises(ValueError):
                harnesses(artifacts, source, target)
            artifacts.write_text("not Cargo JSON\n")
            with self.assertRaises(ValueError):
                harnesses(artifacts, source, target)

    def test_list_arguments_and_package_directory_preserve_test_nonexecution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            record = root / "observed.json"
            body = f"open({str(record)!r}, 'w').write(json.dumps({{'args':sys.argv[1:], 'cwd':os.getcwd()}}))"
            source, _, row = self.fixture(root, body=body)
            item = Harness(Path(row["executable"]), source)
            result = prewarm([item], 1, 5, 5, os.environ.copy())
            self.assertEqual(result["successful"], 1)
            self.assertFalse(result["meaningful_tests_run"])
            self.assertEqual(json.loads(record.read_text()), {"args": ["--list"], "cwd": str(source)})

    def test_timeout_is_bounded_and_does_not_signal_an_unrelated_owned_process(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, _, row = self.fixture(root, body="time.sleep(60)")
            neighbour = subprocess.Popen([sys.executable, "-c", "import time;time.sleep(60)"])
            try:
                start = time.monotonic()
                result = prewarm([Harness(Path(row["executable"]), source)], 1, 1, 2, os.environ.copy())
                self.assertEqual(result["timed_out"], 1)
                self.assertLess(time.monotonic() - start, 6)
                self.assertIsNone(neighbour.poll())
            finally:
                neighbour.terminate()
                neighbour.wait(timeout=HANG_BOUND)

    def test_timeout_kills_descendants_even_when_the_listing_leader_exits_on_term(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            address = str(root / "child.sock")
            marker = root / "child.pid"
            child_code = (
                "import os, signal, socket, time\n"
                "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                "listener = socket.socket(socket.AF_UNIX)\n"
                f"listener.bind({address!r})\nlistener.listen()\n"
                f"open({str(marker)!r}, 'w').write(str(os.getpid()))\n"
                "time.sleep(60)\n")
            body = ("import subprocess\n"
                    f"subprocess.Popen([sys.executable, '-c', {child_code!r}])\n"
                    "time.sleep(60)")
            source, _, row = self.fixture(root, body=body)
            child = None
            try:
                result = prewarm([Harness(Path(row["executable"]), source)],
                                 1, 2, 4, os.environ.copy())
                self.assertTrue(marker.exists(), "the descendant must actually start")
                child = int(marker.read_text())
                self.assertEqual(result["timed_out"], 1)
                # A zombie may await the host's reaper, but a killed process
                # cannot retain its listening socket.
                deadline = time.monotonic() + 2
                while True:
                    with socket.socket(socket.AF_UNIX) as connection:
                        connection.settimeout(1)
                        try:
                            connection.connect(address)
                        except ConnectionRefusedError:
                            break
                    if time.monotonic() >= deadline:
                        self.fail("the listing descendant retained its socket after timeout")
                    time.sleep(0.02)
            finally:
                if child is None and marker.exists():
                    child = int(marker.read_text())
                if child is not None:
                    try:
                        os.kill(child, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_budget_leaves_pending_work_unstarted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, _, row = self.fixture(root, body="time.sleep(60)")
            item = Harness(Path(row["executable"]), source)
            result = prewarm([item, item, item], 1, 5, 1, os.environ.copy())
            self.assertEqual(result["timed_out"], 1)
            self.assertEqual(result["budget_exhausted"], 2)

    def test_child_library_environment_does_not_mutate_parent(self):
        before = os.environ.copy()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = listing_environment([Harness(root / "debug/deps/a", root)], root)
            key = "DYLD_FALLBACK_LIBRARY_PATH" if sys.platform == "darwin" else "LD_LIBRARY_PATH"
            self.assertIn(str(root / "debug/deps"), environment[key])
        self.assertEqual(dict(os.environ), before)

    def test_cli_cancellation_reaps_only_its_listing_even_when_term_is_ignored(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "listing.pid"
            pending_marker = root / "listing.pid.pending"
            # The marker's existence means its PID is ready to read.
            body = ("import signal\nsignal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                    f"open({str(pending_marker)!r}, 'w').write(str(os.getpid()))\n"
                    f"os.replace({str(pending_marker)!r}, {str(marker)!r})\ntime.sleep(60)")
            source, target, row = self.fixture(root, body=body)
            artifacts = root / "artifacts.jsonl"
            artifacts.write_text(json.dumps(row) + "\n")
            command = [sys.executable, str(ROOT / "tools/ci/prewarm_test_binaries.py"),
                       "--artifacts", str(artifacts), "--source", str(source), "--target", str(target)]
            process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                deadline = time.monotonic() + 5
                while not marker.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(marker.exists(), "owned listing must actually start")
                child = int(marker.read_text())
                process.terminate()
                stdout, _ = process.communicate(timeout=HANG_BOUND)
                self.assertEqual(process.returncode, 143)
                self.assertEqual(json.loads(stdout)["cancelled"], 1)
                with self.assertRaises(ProcessLookupError):
                    os.kill(child, 0)
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=HANG_BOUND)

    def test_cli_failure_is_reported_as_optimization_failure_not_a_test_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, target, row = self.fixture(root, body="sys.exit(9)")
            artifacts = root / "artifacts.jsonl"
            artifacts.write_text(json.dumps(row) + "\n")
            command = [sys.executable, str(ROOT / "tools/ci/prewarm_test_binaries.py"),
                       "--artifacts", str(artifacts), "--source", str(source), "--target", str(target)]
            output = subprocess.run(command, capture_output=True, text=True, check=True)
            evidence = json.loads(output.stdout)
            self.assertEqual(evidence["failed"], 1)
            self.assertEqual(evidence["successful"], 0)
            self.assertFalse(evidence["meaningful_tests_run"])
            artifacts.write_text("not JSON\n")
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)

    def test_nextest_check_owns_discovery_and_keeps_full_tests_and_doctests(self):
        checks = {check["name"]: check for check in t_matrix.load(ROOT)["check"]}
        apps = checks["apps-macos"]["run"]
        self.assertNotIn("prewarm_test_binaries.py", apps)
        self.assertNotIn("--no-run", apps)
        nextest = "cargo nextest run --locked --profile ci --no-fail-fast $packages || status=$?"
        doctests = "cargo test --locked --no-fail-fast --doc $packages || status=$?"
        self.assertEqual(apps.count(nextest), 1)
        self.assertEqual(apps.count(doctests), 1)
        self.assertLess(apps.index(nextest), apps.index(doctests))


if __name__ == "__main__":
    unittest.main()

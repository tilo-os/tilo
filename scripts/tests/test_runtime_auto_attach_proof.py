import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).parents[1] / "runtime-auto-attach-proof.py"
SPEC = importlib.util.spec_from_file_location("runtime_auto_attach_proof", SCRIPT)
PROOF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROOF)


class RuntimeAutoAttachProofTests(unittest.TestCase):
    def test_supported_cli_captures_host_source_snapshot(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "host-sources.json"
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--capture-host-source-snapshot",
                    str(output),
                ],
                cwd=SCRIPT.parents[1],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            snapshot = json.loads(output.read_text())
        self.assertEqual(snapshot, PROOF.source_snapshot(SCRIPT.parents[1]))

    def test_launch_commands_are_ordinary_cargo_run(self):
        fixture = pathlib.Path("/tmp/proof/source.png")
        self.assertEqual(
            PROOF.launch_commands(fixture),
            [
                ("word-processor", ["cargo", "run", "-p", "word-processor"]),
                ("file-explorer", ["cargo", "run", "-p", "file-explorer"]),
                ("photos", ["cargo", "run", "-p", "photos", "--", "--open", str(fixture)]),
                ("calculator", ["cargo", "run", "-p", "calculator"]),
            ],
        )

    def test_native_status_requires_exact_valid_shape(self):
        self.assertEqual(
            PROOF.decode_native_status(b'{"version":1,"pid":321,"room":"room-a"}\n'),
            {"version": 1, "pid": 321, "room": "room-a"},
        )
        for frame in (
            b'{"version":2,"pid":321,"room":"room-a"}\n',
            b'{"version":1,"pid":0,"room":"room-a"}\n',
            b'{"version":1,"pid":321,"room":""}\n',
            b'{"version":1,"pid":321,"room":"room-a","extra":true}\n',
            b'not-json\n',
        ):
            with self.subTest(frame=frame), self.assertRaises(PROOF.ProofError):
                PROOF.decode_native_status(frame)

    def test_expected_apps_rejects_missing_or_duplicate_peers(self):
        expected = {
            "dev.qos.word-processor",
            "dev.qos.file-explorer",
            "dev.qos.photos",
            "dev.qos.calculator",
        }
        apps = [
            {"app_id": app_id, "peer_id": peer_id}
            for peer_id, app_id in enumerate(sorted(expected), start=11)
        ]
        self.assertEqual(set(PROOF.index_expected_apps(apps, expected)), expected)
        with self.assertRaises(PROOF.ProofError):
            PROOF.index_expected_apps(apps[:-1], expected)
        apps[-1]["peer_id"] = apps[0]["peer_id"]
        with self.assertRaises(PROOF.ProofError):
            PROOF.index_expected_apps(apps, expected)

    def test_changed_source_snapshot_is_reported(self):
        before = {"a.rs": "one", "b.rs": "two"}
        self.assertEqual(PROOF.changed_sources(before, before.copy()), [])
        self.assertEqual(
            PROOF.changed_sources(before, {"a.rs": "changed", "c.rs": "three"}),
            ["a.rs", "b.rs", "c.rs"],
        )

    def test_cleanup_signals_only_exact_owned_pids(self):
        alive = {17, 23}

        def send(pid, _signal):
            alive.discard(pid)

        with mock.patch.object(PROOF, "pid_alive", side_effect=lambda pid: pid in alive), mock.patch.object(
            PROOF.os, "kill", side_effect=send
        ) as kill:
            PROOF.terminate_owned_pids([17, 23])
        self.assertEqual({call.args[0] for call in kill.call_args_list}, {17, 23})

    def test_app_launch_cleanup_reaps_terminated_direct_child(self):
        with tempfile.TemporaryDirectory() as directory:
            launch = PROOF.AppLaunch(
                "sleeper",
                [sys.executable, "-c", "import time; time.sleep(60)"],
                pathlib.Path.cwd(),
                os.environ.copy(),
                pathlib.Path(directory),
            )
            pid = launch.process.pid
            known, status = launch.terminate()
        self.assertIn(pid, known)
        self.assertEqual(status, -PROOF.signal.SIGTERM)
        self.assertEqual(PROOF.processes_for_pids({pid}), [])

    def test_profile_cleanup_rejects_a_remaining_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = pathlib.Path(directory) / "profile"
            profile.mkdir()
            with mock.patch.object(PROOF.shutil, "rmtree", return_value=None):
                with self.assertRaisesRegex(PROOF.ProofError, "runtime profile remains"):
                    PROOF.remove_profile(profile)


if __name__ == "__main__":
    unittest.main()

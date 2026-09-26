import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


MODULE_PATH = pathlib.Path(__file__).resolve().parents[1] / "app-launch-sweep.py"
SPEC = importlib.util.spec_from_file_location("app_launch_sweep", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
SWEEP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SWEEP)


class SweepCheckpointTest(unittest.TestCase):
    def setUp(self):
        # Also called at the start of every merged phase: each phase gets its own
        # temporary tree so no checkpoint or fixture leaks into the next phase.
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.out = pathlib.Path(self.tempdir.name) / "out"
        self.config = {
            "packages": ["org.tilo.calculator", "org.tilo.notes"],
            "worktree": "/worktree",
            "data_dir": "/data",
            "pin": False,
            "empty_workspace": False,
            "shots": False,
            "settle": 60.0,
            "hold": 60.0,
            "min_hold": 45.0,
            "startup_timeout": 45.0,
        }
        self.result = {
            "adopted": ["org.tilo.calculator#1"],
            "errors": [],
            "compositor_started": True,
            "launched": True,
            "png": None,
            "frames": [],
            "gui": True,
        }

    def test_checkpoint_persists_for_resume_and_fresh_run_replaces_it(self):
        with self.subTest(phase='checkpoint persists each completed result for resume'):
            self.setUp()
            checkpoint, completed = SWEEP.prepare_checkpoint(self.out, self.config, False)
            completed["org.tilo.calculator"] = self.result
            SWEEP.save_checkpoint(checkpoint, self.config, completed)

            resumed_path, resumed = SWEEP.prepare_checkpoint(self.out, self.config, True)

            self.assertEqual(resumed_path, checkpoint)
            self.assertEqual(resumed, {"org.tilo.calculator": self.result})
            self.assertEqual(
                json.loads(checkpoint.read_text())["version"], SWEEP.PROGRESS_VERSION
            )
            self.assertFalse(list(self.out.glob(".*.tmp")))
        with self.subTest(phase='fresh run replaces an old checkpoint'):
            self.setUp()
            checkpoint, completed = SWEEP.prepare_checkpoint(self.out, self.config, False)
            completed["org.tilo.calculator"] = self.result
            SWEEP.save_checkpoint(checkpoint, self.config, completed)

            _, restarted = SWEEP.prepare_checkpoint(self.out, self.config, False)

            self.assertEqual(restarted, {})
            self.assertEqual(json.loads(checkpoint.read_text())["completed"], {})

    def test_resume_rejects_corrupt_incompatible_or_malformed_state(self):
        with self.subTest(phase='corrupt or incompatible state'):
            self.setUp()
            self.out.mkdir()
            checkpoint = self.out / SWEEP.PROGRESS_FILENAME
            checkpoint.write_text("{not json")
            with self.assertRaisesRegex(SWEEP.SweepStateError, "corrupt"):
                SWEEP.prepare_checkpoint(self.out, self.config, True)

            SWEEP.save_checkpoint(checkpoint, self.config, {})
            incompatible = dict(self.config, shots=True)
            with self.assertRaisesRegex(SWEEP.SweepStateError, "different sweep options"):
                SWEEP.prepare_checkpoint(self.out, incompatible, True)
        with self.subTest(phase='unknown or malformed completed records'):
            self.setUp()
            self.out.mkdir()
            checkpoint = self.out / SWEEP.PROGRESS_FILENAME
            SWEEP.atomic_write_json(
                checkpoint,
                {
                    "version": SWEEP.PROGRESS_VERSION,
                    "configuration": self.config,
                    "completed": {"unknown.app": {"gui": True}},
                },
            )

            with self.assertRaisesRegex(SWEEP.SweepStateError, "invalid completed"):
                SWEEP.prepare_checkpoint(self.out, self.config, True)
        with self.subTest(phase='non-object, wrong version and bad map'):
            self.setUp()
            self.out.mkdir()
            checkpoint = self.out / SWEEP.PROGRESS_FILENAME

            checkpoint.write_text("[]")
            with self.assertRaisesRegex(SWEEP.SweepStateError, "not a JSON object"):
                SWEEP.prepare_checkpoint(self.out, self.config, True)

            SWEEP.atomic_write_json(
                checkpoint,
                {
                    "version": SWEEP.PROGRESS_VERSION + 1,
                    "configuration": self.config,
                    "completed": {},
                },
            )
            with self.assertRaisesRegex(SWEEP.SweepStateError, "incompatible version"):
                SWEEP.prepare_checkpoint(self.out, self.config, True)

            SWEEP.atomic_write_json(
                checkpoint,
                {
                    "version": SWEEP.PROGRESS_VERSION,
                    "configuration": self.config,
                    "completed": [],
                },
            )
            with self.assertRaisesRegex(
                SWEEP.SweepStateError, "valid completed-results map"
            ):
                SWEEP.prepare_checkpoint(self.out, self.config, True)

            bad_result = dict(self.result, compositor_started="yes")
            SWEEP.atomic_write_json(
                checkpoint,
                {
                    "version": SWEEP.PROGRESS_VERSION,
                    "configuration": self.config,
                    "completed": {"org.tilo.calculator": bad_result},
                },
            )
            with self.assertRaisesRegex(SWEEP.SweepStateError, "invalid completed"):
                SWEEP.prepare_checkpoint(self.out, self.config, True)

    def _make_cli_fixture(self):
        root = pathlib.Path(self.tempdir.name) / "fixture"
        worktree = root / "repo"
        (worktree / "target/debug").mkdir(parents=True)
        (worktree / "target/debug/shell").write_text("")
        (worktree / "target/debug/tilo-launcher").write_text("")
        data_dir = root / "data"
        (data_dir / "payloads/org.tilo.cli").mkdir(parents=True)
        (data_dir / "registry.toml").write_text('id = "org.tilo.cli"\n')
        (data_dir / "payloads/org.tilo.cli/manifest.toml").write_text('type = "cli"\n')
        return worktree, data_dir, root / "out"

    def _run_cli(self, worktree, data_dir, out, *extra):
        return subprocess.run(
            [
                sys.executable,
                str(MODULE_PATH),
                "--worktree",
                str(worktree),
                "--data-dir",
                str(data_dir),
                "--out",
                str(out),
                *extra,
            ],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_cli_resume_rejects_bad_checkpoints_and_reruns_only_unfinished_packages(self):
        with self.subTest(phase='cli rejects corrupt and incompatible checkpoints'):
            self.setUp()
            worktree, data_dir, out = self._make_cli_fixture()
            out.mkdir(parents=True)
            checkpoint = out / SWEEP.PROGRESS_FILENAME

            checkpoint.write_text("{broken")
            corrupt = self._run_cli(worktree, data_dir, out, "--resume")
            self.assertEqual(corrupt.returncode, 2)
            self.assertIn("corrupt", corrupt.stderr)

            fresh = self._run_cli(worktree, data_dir, out)
            self.assertEqual(fresh.returncode, 0, fresh.stderr)
            incompatible = self._run_cli(worktree, data_dir, out, "--resume", "--shots")
            self.assertEqual(incompatible.returncode, 2)
            self.assertIn("different sweep options", incompatible.stderr)
        with self.subTest(phase='interrupted run resumes only unfinished packages'):
            self.setUp()
            root = pathlib.Path(self.tempdir.name) / "fixture"
            worktree = root / "repo"
            (worktree / "target/debug").mkdir(parents=True)
            (worktree / "target/debug/shell").write_text("")
            (worktree / "target/debug/tilo-launcher").write_text("")
            data_dir = root / "data"
            for package_id in ("org.tilo.first", "org.tilo.second"):
                (data_dir / "payloads" / package_id).mkdir(parents=True)
            (data_dir / "registry.toml").write_text(
                'id = "org.tilo.first"\nid = "org.tilo.second"\n'
            )
            for package_id in ("org.tilo.first", "org.tilo.second"):
                (data_dir / "payloads" / package_id / "manifest.toml").write_text(
                    'type = "gui"\n'
                )
            out = root / "out"
            argv = [
                "app-launch-sweep.py",
                "--worktree",
                str(worktree),
                "--data-dir",
                str(data_dir),
                "--out",
                str(out),
                "--settle",
                "0",
            ]
            result = dict(self.result, adopted=["org.tilo.first#1"])
            calls = []

            def interrupt_on_second(_args, package_id, _out, _env_extra):
                calls.append(package_id)
                if package_id == "org.tilo.second":
                    raise KeyboardInterrupt
                return dict(result)

            with mock.patch.object(SWEEP, "run_one", side_effect=interrupt_on_second):
                with mock.patch.object(sys, "argv", argv):
                    with self.assertRaises(KeyboardInterrupt):
                        SWEEP.main()

            checkpoint = out / SWEEP.PROGRESS_FILENAME
            saved = json.loads(checkpoint.read_text())
            self.assertEqual(list(saved["completed"]), ["org.tilo.first"])
            self.assertEqual(calls, ["org.tilo.first", "org.tilo.second"])
            calls.clear()

            second_result = dict(self.result, adopted=["org.tilo.second#1"])

            def record_second_run(_args, package_id, _out, _env_extra):
                calls.append(package_id)
                return second_result

            with mock.patch.object(SWEEP, "run_one", side_effect=record_second_run):
                with mock.patch.object(sys, "argv", argv + ["--resume"]):
                    self.assertEqual(SWEEP.main(), 0)

            self.assertEqual(calls, ["org.tilo.second"])
            completed = json.loads(checkpoint.read_text())["completed"]
            self.assertEqual(set(completed), {"org.tilo.first", "org.tilo.second"})
            report = json.loads((out / "results.json").read_text())
            self.assertEqual(set(report), {"org.tilo.first", "org.tilo.second"})


if __name__ == "__main__":
    unittest.main()

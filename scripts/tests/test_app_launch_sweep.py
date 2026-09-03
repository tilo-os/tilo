import importlib.util
import json
import pathlib
import tempfile
import unittest


MODULE_PATH = pathlib.Path(__file__).resolve().parents[1] / "app-launch-sweep.py"
SPEC = importlib.util.spec_from_file_location("app_launch_sweep", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
SWEEP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SWEEP)


class SweepCheckpointTest(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.out = pathlib.Path(self.tempdir.name) / "out"
        self.config = {
            "packages": ["org.qos.calculator", "org.qos.notes"],
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
            "adopted": ["org.qos.calculator#1"],
            "errors": [],
            "compositor_started": True,
            "launched": True,
            "png": None,
            "frames": [],
            "gui": True,
        }

    def tearDown(self):
        self.tempdir.cleanup()

    def test_checkpoint_persists_each_completed_result_for_resume(self):
        checkpoint, completed = SWEEP.prepare_checkpoint(self.out, self.config, False)
        completed["org.qos.calculator"] = self.result
        SWEEP.save_checkpoint(checkpoint, self.config, completed)

        resumed_path, resumed = SWEEP.prepare_checkpoint(self.out, self.config, True)

        self.assertEqual(resumed_path, checkpoint)
        self.assertEqual(resumed, {"org.qos.calculator": self.result})
        self.assertEqual(
            json.loads(checkpoint.read_text())["version"], SWEEP.PROGRESS_VERSION
        )
        self.assertFalse(list(self.out.glob(".*.tmp")))

    def test_fresh_run_replaces_an_old_checkpoint(self):
        checkpoint, completed = SWEEP.prepare_checkpoint(self.out, self.config, False)
        completed["org.qos.calculator"] = self.result
        SWEEP.save_checkpoint(checkpoint, self.config, completed)

        _, restarted = SWEEP.prepare_checkpoint(self.out, self.config, False)

        self.assertEqual(restarted, {})
        self.assertEqual(json.loads(checkpoint.read_text())["completed"], {})

    def test_resume_rejects_corrupt_or_incompatible_state(self):
        self.out.mkdir()
        checkpoint = self.out / SWEEP.PROGRESS_FILENAME
        checkpoint.write_text("{not json")
        with self.assertRaisesRegex(SWEEP.SweepStateError, "corrupt"):
            SWEEP.prepare_checkpoint(self.out, self.config, True)

        SWEEP.save_checkpoint(checkpoint, self.config, {})
        incompatible = dict(self.config, shots=True)
        with self.assertRaisesRegex(SWEEP.SweepStateError, "different sweep options"):
            SWEEP.prepare_checkpoint(self.out, incompatible, True)

    def test_resume_rejects_unknown_or_malformed_completed_records(self):
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


if __name__ == "__main__":
    unittest.main()

"""The diagnostic must fail closed on lost coverage or observation."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest import mock


SOURCE = Path(__file__).resolve().parents[1] / "ci/browser_compilation_trial.py"
SPEC = importlib.util.spec_from_file_location("browser_compilation_trial", SOURCE)
TRIAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TRIAL)


class BrowserCompilationTrial(unittest.TestCase):
    def test_wrapper_preserves_driver_arguments_and_only_the_selected_flag(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            chrome = root / "chrome"
            chrome.write_text("#!/usr/bin/env python3\nimport sys,json\nprint(json.dumps(sys.argv[1:]))\n")
            chrome.chmod(0o755)
            wrapper = root / "wrapper"
            TRIAL.write_wrapper(wrapper)
            for mode, flags in {"default": [], **TRIAL.CANDIDATES}.items():
                with self.subTest(mode=mode):
                    env = dict(os.environ, TRIAL_REAL_CHROME=str(chrome), TRIAL_TASKS=mode)
                    args = json.loads(subprocess.check_output(
                        [str(wrapper), "--user-data-dir=/owned/profile", "about:blank"], env=env, text=True))
                    self.assertEqual(args, [*flags, "--user-data-dir=/owned/profile", "about:blank"])

    def exercise(self, emitted, status=0, flag_error=False, swap=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cgroup = root / "cgroup"
            cgroup.mkdir()
            for name, value in {"cpu.stat": "usage_usec 50\n",
                                "memory.events": "max 0\noom 0\noom_kill 0\n",
                                "memory.current": "100", "memory.peak": "200",
                                "memory.swap.current": str(swap)}.items():
                (cgroup / name).write_text(value)
            binary = root / "bin"
            binary.mkdir()
            node = binary / "node"
            node.write_text("#!/usr/bin/env python3\nimport json,os,sys\n"
                            "for app in json.loads(os.environ['FIXTURE_APPS']):\n"
                            " print(json.dumps({'app':app,'status':'presented frames 10','log':[]}))\n"
                            "if os.environ['FIXTURE_FLAG']=='1':print('Error: unrecognized flag --wasm-num-compilation-tasks=2')\n"
                            "sys.exit(int(os.environ['FIXTURE_EXIT']))\n")
            node.chmod(0o755)
            output = root / "sweep"
            environment = {"CHROME": "/fixture/chrome", "FIXTURE_APPS": json.dumps(emitted),
                           "FIXTURE_EXIT": str(status), "FIXTURE_FLAG": "1" if flag_error else "0",
                           "PATH": str(binary) + os.pathsep + os.environ["PATH"]}
            with mock.patch.dict(os.environ, environment):
                try:
                    result = TRIAL.sweep(output, ["so.tilo.calculator", "desktop"], "2", cgroup)
                    failed = False
                except RuntimeError:
                    result = json.loads((output / "receipt.json").read_text())
                    failed = True
            return failed, result

    def test_real_child_output_covers_every_expected_app(self):
        failed, result = self.exercise(["so.tilo.calculator", "desktop"])
        self.assertFalse(failed)
        self.assertTrue(result["coverage_valid"])
        self.assertEqual(result["exit"], 0)
        self.assertEqual(result["memory_current_sampled_peak_bytes"], 100)

    def test_child_failure_survives_complete_probes(self):
        failed, result = self.exercise(["so.tilo.calculator", "desktop"], status=7)
        self.assertTrue(failed)
        self.assertEqual(result["exit"], 7)

    def test_swap_cannot_qualify_even_when_all_probes_pass(self):
        failed, result = self.exercise(["so.tilo.calculator", "desktop"], swap=1)
        self.assertTrue(failed)
        self.assertEqual(result["swap_current_sampled_peak_bytes"], 1)

    def test_unseen_code_cache_is_unobserved_not_healthy_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertIsNone(TRIAL.cache_sizes(root))
            cache = root / "smoke-browsers.fixture/0/Default/Code Cache/wasm"
            cache.mkdir(parents=True)
            (cache / "entry").write_bytes(b"12345")
            self.assertEqual(TRIAL.cache_sizes(root),
                             {"directories": 1, "bytes": 5, "largest_file_bytes": 5})

    def test_zero_exit_with_missing_or_duplicate_apps_fails(self):
        for apps in (["desktop"], ["desktop", "desktop", "so.tilo.calculator"]):
            with self.subTest(apps=apps):
                failed, result = self.exercise(apps)
                self.assertTrue(failed)
                self.assertFalse(result["coverage_valid"])

    def test_ignored_browser_flag_cannot_become_a_candidate_success(self):
        failed, result = self.exercise(["so.tilo.calculator", "desktop"], flag_error=True)
        self.assertTrue(failed)
        self.assertTrue(result["flag_rejected"])

    def test_missing_counter_and_reset_counter_fail_observation(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                TRIAL.counters(Path(directory), "cpu.stat")
        with self.assertRaisesRegex(ValueError, "counters decreased"):
            TRIAL.delta({"usage_usec": 10}, {"usage_usec": 9})


if __name__ == "__main__":
    unittest.main()

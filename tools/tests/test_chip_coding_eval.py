"""Acceptance evidence must fail closed when tests were not actually observed."""

import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


PATH = Path(__file__).resolve().parents[1] / "chip" / "coding_eval.py"
SPEC = importlib.util.spec_from_file_location("coding_eval", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class CodingEvalTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.log = self.root / "acceptance.log"
        self.log.write_text("test acceptance::real_path ... ok\n"
                            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;\n")
        self.check = {"id": "accept", "argv": ["cargo", "test", "acceptance::"],
                      "test_source": "tests/acceptance.rs", "test_source_sha256": "b" * 64,
                      "expected_tests": ["acceptance::real_path"]}
        self.pack = {"schema_version": 1, "purpose": "baseline_readiness",
                     "repository_revision": "a" * 40,
                     "tasks": [{"id": "task", "instruction": "Exercise production path",
                                "category": "readiness", "checks": [self.check]}]}
        self.pack_hash = "c" * 64
        self.observed = self.check | {"log": "acceptance.log", "returncode": 0,
                                     "log_sha256": MODULE.digest(self.log.read_bytes())}
        self.run = {"schema_version": 1, "mode": "baseline_readiness",
                    "manifest_sha256": self.pack_hash, "candidate_revision": "a" * 40,
                    "harness_revision": "a" * 40, "environment_sha256": "d" * 64,
                    "action_schema_sha256": "e" * 64, "trace_sha256": "f" * 64,
                    "harness": "tilo-agent", "permissions": "scoped resource actions",
                    "network_policy": "offline", "grader_identity": "independent grader",
                    "tasks": [{"id": "task", "checks": [self.observed]}]}

    def score(self):
        return MODULE.score(self.pack, self.run, self.pack_hash, self.root)

    def replace_log(self, text):
        self.log.write_text(text)
        self.observed["log_sha256"] = MODULE.digest(self.log.read_bytes())

    def test_zero_tests_and_missing_named_test_are_not_success(self):
        for text in ("test result: ok. 0 passed; 0 failed; 0 ignored;\n",
                     "test other::test ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n",
                     "build finished successfully\n"):
            with self.subTest(output=text):
                self.replace_log(text)
                with self.assertRaises(ValueError):
                    self.score()

    def test_failed_or_ignored_test_and_nonzero_exit_fail_task(self):
        for result in ("FAILED", "ignored"):
            with self.subTest(result=result):
                self.replace_log(f"test acceptance::real_path ... {result}\n"
                                 "test result: FAILED. 0 passed; 1 failed; 0 ignored;\n")
                self.assertEqual(self.score()["passed_tasks"], 0)
        self.replace_log("test acceptance::real_path ... ok\n"
                         "test result: ok. 1 passed; 0 failed; 0 ignored;\n")
        self.observed["returncode"] = 1
        self.assertEqual(self.score()["passed_tasks"], 0)

    def test_changed_logs_or_acceptance_source_are_rejected(self):
        self.log.write_text("changed evidence")
        with self.assertRaisesRegex(ValueError, "digest"):
            self.score()
        self.observed["test_source_sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "source mismatch"):
            self.score()

    def test_readiness_cannot_claim_model_quality(self):
        self.run["mode"] = "model_evaluation"
        with self.assertRaisesRegex(ValueError, "readiness"):
            self.score()

    def test_model_evaluation_requires_full_pinned_provenance(self):
        self.pack["purpose"] = "coding_tasks"
        self.run["mode"] = "model_evaluation"
        with self.assertRaisesRegex(ValueError, "model id"):
            self.score()
        self.run["model"] = {"id": "Qwen/Qwen3.8-27B", "revision": "main"}
        with self.assertRaisesRegex(ValueError, "model revision"):
            self.score()

    def test_missing_duplicate_or_extra_task_evidence_fails(self):
        original = copy.deepcopy(self.run["tasks"])
        for tasks in ([], original * 2, original + [{"id": "other", "checks": []}]):
            with self.subTest(tasks=tasks):
                self.run["tasks"] = tasks
                with self.assertRaisesRegex(ValueError, "identities"):
                    self.score()

    def test_log_escape_and_duplicate_completion_fail(self):
        self.observed["log"] = "../acceptance.log"
        with self.assertRaisesRegex(ValueError, "relative"):
            self.score()
        self.observed["log"] = "acceptance.log"
        self.replace_log(self.log.read_text() + "test acceptance::real_path ... ok\n")
        with self.assertRaisesRegex(ValueError, "duplicate completion"):
            self.score()

    def test_cli_scores_fixture_and_refuses_to_overwrite_evidence(self):
        pack_path = self.root / "manifest.json"
        pack_path.write_text(json.dumps(self.pack))
        self.run["manifest_sha256"] = MODULE.digest(pack_path.read_bytes())
        run_path = self.root / "run.json"
        run_path.write_text(json.dumps(self.run))
        out = self.root / "score.json"
        command = ["python3", str(PATH), "score", "--manifest", str(pack_path),
                   "--run", str(run_path), "--out", str(out)]
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(out.read_text())
        self.assertEqual(report["purpose"], "baseline_readiness")
        self.assertEqual(report["passed_tasks"], 1)
        self.assertNotIn("tokens_per_second", report)
        self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)

    def test_nextest_junit_checks_binary_names_and_rejects_failures_skips_retries(self):
        self.check.update(formats=["nextest_junit"], expected_binary="agent::it")
        self.observed["format"] = "nextest_junit"
        template = ('<testsuites tests="1" failures="0"><testsuite name="agent::it">'
                    '<testcase classname="agent::it" name="acceptance::real_path">'
                    '{}</testcase></testsuite></testsuites>')
        self.replace_log(template.format(""))
        self.assertEqual(self.score()["passed_tasks"], 1)
        for tag in ("failure", "error", "skipped", "flakyFailure", "rerunError"):
            with self.subTest(tag=tag):
                self.replace_log(template.format(f"<{tag}/>"))
                self.assertEqual(self.score()["passed_tasks"], 0)
        self.replace_log(template.format("").replace('classname="agent::it"', 'classname="other::it"'))
        with self.assertRaisesRegex(ValueError, "missing"):
            self.score()
        self.replace_log('<testsuites tests="0"/>')
        with self.assertRaisesRegex(ValueError, "no observed"):
            self.score()

    def test_nextest_console_requires_named_passes_and_completion_summary(self):
        self.check.update(formats=["nextest_text"], expected_binary="agent::it")
        self.observed["format"] = "nextest_text"
        self.replace_log("        PASS [   0.004s] agent::it acceptance::real_path\n"
                         "     Summary [   0.006s] 1 tests run: 1 passed, 44 skipped\n")
        self.assertEqual(self.score()["passed_tasks"], 1)
        self.replace_log("        SKIP [   0.004s] agent::it acceptance::real_path\n"
                         "     Summary [   0.006s] 1 tests run: 1 passed, 44 skipped\n")
        self.assertEqual(self.score()["passed_tasks"], 0)
        self.replace_log("     Summary [   0.006s] 1 tests run: 1 passed, 44 skipped\n")
        with self.assertRaisesRegex(ValueError, "missing"):
            self.score()

    def test_bool_schema_and_nonstring_log_fail_cleanly(self):
        self.pack["schema_version"] = True
        with self.assertRaisesRegex(ValueError, "schema"):
            self.score()
        self.pack["schema_version"] = 1
        self.observed["log"] = ["invalid"]
        with self.assertRaisesRegex(ValueError, "log"):
            self.score()


if __name__ == "__main__":
    unittest.main()

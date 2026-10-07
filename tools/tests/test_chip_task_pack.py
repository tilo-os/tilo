"""Invalid challenge calibration must never become a model benchmark."""

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


DIRECTORY = Path(__file__).resolve().parents[1] / "chip"
sys.path.insert(0, str(DIRECTORY))
SPEC = importlib.util.spec_from_file_location("task_pack", DIRECTORY / "task_pack.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class TaskPackTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.check = {"id": "accept", "argv": ["cargo", "test", "actual::check"],
                      "test_source": "tests/actual.rs", "test_source_sha256": "b" * 64,
                      "expected_tests": ["actual::check"]}
        contract = {"schema_version": 1, "purpose": "coding_tasks",
                    "repository_revision": "a" * 40,
                    "tasks": [{"id": "issue", "instruction": "Fix the observed defect",
                               "category": "issue_repair", "checks": [self.check]}]}
        self.task = {"id": "issue", "category": "issue_repair", "base_revision": "a" * 40,
                     "prompt": self.store("prompt.txt", b"Fix the observed defect"),
                     "acceptance_manifest": self.store("acceptance.json", json.dumps(contract).encode()),
                     "calibration": {}}
        for kind, revision, passed in (("baseline", "a", False), ("reference", "c", True),
                                       ("mutant", "d", False)):
            self.proof(kind, revision, passed)
        self.pack = {"schema_version": 1, "pack_id": "private-pack",
                     "visibility": "evaluator_private", "tasks": [self.task]}

    def store(self, name, raw):
        (self.root / name).write_bytes(raw)
        return {"path": name, "sha256": MODULE.digest(raw)}

    def proof(self, kind, revision, passed):
        result = "ok" if passed else "FAILED"
        count = "1 passed; 0 failed" if passed else "0 passed; 1 failed"
        raw = f"test actual::check ... {result}\ntest result: {result}. {count}; 0 ignored;\n".encode()
        log = self.store(f"{kind}.log", raw)
        proof = {"schema_version": 1, "candidate_revision": revision * 40,
                 "grader_identity": "test fixture grader",
                 "checks": [self.check | {"log": log["path"], "log_sha256": log["sha256"],
                                           "returncode": 0 if passed else 101}]}
        reference = self.store(f"{kind}.json", json.dumps(proof).encode())
        reference["candidate_revision"] = revision * 40
        self.task["calibration"][kind] = reference

    def test_valid_calibration_remains_task_evidence_not_model_score(self):
        result = MODULE.validate(self.pack, self.root)
        calibration = result["tasks"][0]["calibration"]
        self.assertFalse(calibration["baseline"]["passed"])
        self.assertTrue(calibration["reference"]["passed"])
        self.assertFalse(calibration["mutant"]["passed"])
        self.assertNotIn("model_score", result)

    def test_already_solved_baseline_and_insensitive_mutant_are_rejected(self):
        for kind, revision in (("baseline", "a"), ("mutant", "d")):
            with self.subTest(kind=kind):
                self.proof(kind, revision, True)
                with self.assertRaisesRegex(ValueError, "outcome"):
                    MODULE.validate(self.pack, self.root)
                self.proof(kind, revision, False)

    def test_compile_failure_is_not_evidence_of_a_real_bug(self):
        raw = b"error: source did not compile\n"
        self.store("baseline.log", raw)
        path = self.root / "baseline.json"
        proof = json.loads(path.read_text())
        proof["checks"][0]["log_sha256"] = MODULE.digest(raw)
        reference = self.store("baseline.json", json.dumps(proof).encode())
        reference["candidate_revision"] = "a" * 40
        self.task["calibration"]["baseline"] = reference
        with self.assertRaisesRegex(ValueError, "completion summary"):
            MODULE.validate(self.pack, self.root)

    def test_process_failure_with_all_assertions_passing_cannot_calibrate_bug(self):
        self.proof("baseline", "a", True)
        proof = json.loads((self.root / "baseline.json").read_text())
        proof["checks"][0]["returncode"] = 101
        reference = self.store("baseline.json", json.dumps(proof).encode())
        reference["candidate_revision"] = "a" * 40
        self.task["calibration"]["baseline"] = reference
        with self.assertRaisesRegex(ValueError, "expected-test failure"):
            MODULE.validate(self.pack, self.root)

    def test_changed_prompt_and_missing_calibration_fail(self):
        (self.root / "prompt.txt").write_text("changed prompt")
        with self.assertRaisesRegex(ValueError, "digest"):
            MODULE.validate(self.pack, self.root)
        self.task["prompt"] = self.store("prompt.txt", b"Fix the observed defect")
        del self.task["calibration"]["mutant"]
        with self.assertRaisesRegex(ValueError, "calibrations"):
            MODULE.validate(self.pack, self.root)

    def test_readiness_pack_and_public_visibility_cannot_claim_heldout(self):
        self.pack["visibility"] = "public"
        with self.assertRaisesRegex(ValueError, "private"):
            MODULE.validate(self.pack, self.root)
        self.pack["visibility"] = "evaluator_private"
        path = self.root / "acceptance.json"
        contract = json.loads(path.read_text())
        contract["purpose"] = "baseline_readiness"
        self.task["acceptance_manifest"] = self.store("acceptance.json", json.dumps(contract).encode())
        with self.assertRaisesRegex(ValueError, "coding tasks"):
            MODULE.validate(self.pack, self.root)


if __name__ == "__main__":
    unittest.main()

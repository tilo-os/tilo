"""The PR filesystem gate propagates one exact exclusive-lane result."""
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "run_fuse_contract", ROOT / "tools/ci/run_fuse_contract.py"
)
contract = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contract)
SHA = "a" * 40


class API:
    def __init__(self, polls):
        self.polls = iter(polls)
        self.calls = []

    def __call__(self, method, path, payload=None):
        self.calls.append((method, path, payload))
        if method == "POST":
            return {}
        return {"workflow_runs": next(self.polls)}


def run(run_id, sha=SHA, status="success", workflow=contract.WORKFLOW):
    return {"id": run_id, "workflow_id": workflow, "event": "workflow_dispatch",
            "commit_sha": sha, "status": status}


class RunFuseContract(unittest.TestCase):
    def test_dispatches_and_propagates_the_exact_new_success(self):
        api = API([[run(7)], [run(7), run(8, status="waiting")], [run(8)]])
        result = contract.execute(api, "qos/qos", "topic", SHA, sleep=lambda _: None)
        self.assertEqual(result["id"], 8)
        self.assertIn(("POST", "repos/qos/qos/actions/workflows/fs-fuse-executor.yml/dispatches",
                       {"ref": "topic", "inputs": {"source_sha": SHA}}), api.calls)

    def test_failure_and_unknown_status_fail_closed(self):
        for status in ("failure", "cancelled", "mystery"):
            with self.subTest(status=status), self.assertRaises(RuntimeError):
                contract.execute(API([[], [run(1, status=status)]]), "qos/qos", "topic", SHA,
                                 sleep=lambda _: None)

    def test_wrong_revision_or_missing_run_fails_closed(self):
        with self.assertRaisesRegex(RuntimeError, "branch advanced"):
            contract.execute(API([[], [run(1, sha="b" * 40)]]), "qos/qos", "topic", SHA,
                             sleep=lambda _: None)
        with self.assertRaisesRegex(RuntimeError, "timed out"):
            contract.execute(API([[], []]), "qos/qos", "topic", SHA,
                             sleep=lambda _: None, attempts=1)

    def test_rejects_mutable_or_malformed_dispatch_inputs(self):
        for ref, sha in (("topic", "main"), ("", SHA), ("bad ref", SHA), ("-oops", SHA)):
            with self.subTest(ref=ref, sha=sha), self.assertRaises(ValueError):
                contract.execute(API([]), "qos/qos", ref, sha, sleep=lambda _: None)


if __name__ == "__main__":
    unittest.main()

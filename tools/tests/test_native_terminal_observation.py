#!/usr/bin/env python3
"""Exercise the existing native bootstrap status reader over its real socket client.

The bounded socket peer is a protocol fixture, not a certified compositor or
proof of native task authority. The shell's production custody tests remain
responsible for producing/refusing the status response.
"""
from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import socket
import sys
import tempfile
import threading
import unittest
from hang import HANG_BOUND

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/headless-examples"))
SPEC = importlib.util.spec_from_file_location(
    "packaged_native_bootstrap", ROOT / "scripts/headless-examples/packaged-native-bootstrap.py")
assert SPEC and SPEC.loader
BOOTSTRAP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BOOTSTRAP)
INVOCATION = "f77554cc-0551-4a1e-8e3c-6091c1dcfb8e"


def accepted():
    return {"receipt": {"broker_invocation_id": 37,
                        "update": {"invocation": INVOCATION, "state": "accepted",
                                   "draft": "must not be exported"}}}


class TerminalObservationTests(unittest.TestCase):
    def exchange(self, response, *, export=True):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "control.sock"
            destination = root / "public-failure.json"
            requests = []
            errors = []
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
                server.bind(str(path))
                server.listen(1)
                server.settimeout(5)

                def respond():
                    try:
                        connection, _ = server.accept()
                        with connection:
                            connection.settimeout(5)
                            with connection.makefile("rb") as reader:
                                requests.append(json.loads(reader.readline(65536)))
                            connection.sendall(json.dumps(response).encode() + b"\n")
                    except Exception as error:
                        errors.append(error)

                worker = threading.Thread(target=respond)
                worker.start()
                try:
                    with self.assertRaisesRegex(RuntimeError, "terminal observation was refused"):
                        BOOTSTRAP.wait_terminal(
                            str(path), accepted(), None,
                            public_failure_path=destination if export else None)
                finally:
                    worker.join(timeout=HANG_BOUND)
                self.assertFalse(worker.is_alive())
                self.assertEqual(errors, [])
            self.assertEqual(requests, [{"op": "actions_status", "invocation_id": 37}])
            return json.loads(destination.read_text()) if destination.exists() else None

    def test_refusal_retains_public_error_and_original_identity_without_private_payloads(self):
        result = self.exchange({"err": {"code": "unknown_action", "message": "job is not visible",
                                       "secret": "private"}, "answer": {"draft": "private"}})
        self.assertEqual(result, {"observation": "actions_status_refused", "invocation": INVOCATION,
                                  "broker_invocation_id": 37,
                                  "err": {"code": "unknown_action", "message": "job is not visible"}})
        self.assertNotIn("private", json.dumps(result))
        self.assertNotIn("draft", json.dumps(result))

    def test_without_explicit_public_opt_in_no_refusal_file_is_written(self):
        self.assertIsNone(self.exchange({"err": {"code": "unknown_action", "message": "private"}},
                                       export=False))

    def test_public_fields_are_bounded_and_truncation_is_explicit(self):
        result = self.exchange({"err": {"code": "x" * 129, "message": "💡" * 4097}})
        self.assertEqual(result["err"], {"code": "x" * 128, "code_truncated": True,
                                        "message": "💡" * 4096, "message_truncated": True})
        self.assertLess(len(json.dumps(result).encode()), 51000)

    def test_malformed_error_does_not_export_arbitrary_structure_or_claim_action_failure(self):
        for error in ("private", {"code": {"private": "nested"}, "message": ["private"]}):
            result = self.exchange({"err": error})
            self.assertEqual(result["err"], {})
            self.assertEqual(result["observation"], "actions_status_refused")
            self.assertNotIn("state", result)

    def test_original_terminal_success_is_unchanged(self):
        receipt = {"update": {"invocation": INVOCATION, "state": "succeeded", "output": {"n": 4}}}
        self.assertIs(BOOTSTRAP.wait_terminal("unused", {"receipt": receipt}, None), receipt)

    def test_original_failed_receipt_remains_available_only_at_opted_in_path(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "failure.json"
            receipt = {"update": {"invocation": INVOCATION, "state": "failed", "message": "public"}}
            with self.assertRaisesRegex(RuntimeError, "terminated failed"):
                BOOTSTRAP.wait_terminal("unused", {"receipt": receipt}, None,
                                        public_failure_path=destination)
            self.assertEqual(json.loads(destination.read_text()), receipt)


if __name__ == "__main__":
    unittest.main()

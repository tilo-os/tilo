#!/usr/bin/env python3
"""Tests for protected-branch native kernel snapshot promotion."""

from __future__ import annotations

import hashlib
import importlib.util
import os
from pathlib import Path
import sys
import unittest
from unittest import mock


SCRIPT = Path(__file__).parents[1] / "dev" / "open-kernel-snapshot-pr.py"
SPEC = importlib.util.spec_from_file_location("open_kernel_snapshot_pr", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class KernelSnapshotPullRequestTests(unittest.TestCase):
    x86_ref = "code.podesta.ai/qos/tilo-kernel@sha256:" + "a" * 64
    arm_ref = "code.podesta.ai/qos/tilo-kernel@sha256:" + "b" * 64

    def setUp(self):
        contents = MODULE.snapshot_contents(self.x86_ref, self.arm_ref)
        self.digest = hashlib.sha256(contents.encode()).hexdigest()
        self.branch = "automation/kernel-snapshot-" + self.digest[:16]

    def argv(self, **overrides):
        values = {
            "branch": self.branch,
            "digest": self.digest,
            "x86": self.x86_ref,
            "arm": self.arm_ref,
        }
        values.update(overrides)
        return [
            str(SCRIPT), "--server", "https://forge.invalid",
            "--repository", "qos/qos", "--branch", values["branch"],
            "--snapshot-digest", values["digest"], "--x86-ref", values["x86"],
            "--arm-ref", values["arm"],
        ]

    def test_finds_existing_open_pull_from_same_repository(self):
        pulls = [{
            "number": 7,
            "html_url": "https://forge.invalid/qos/qos/pulls/7",
            "head": {"ref": self.branch, "repo": {"full_name": "qos/qos"}},
            "base": {"ref": "main"},
        }]
        with mock.patch.object(MODULE, "request", return_value=pulls):
            self.assertEqual(
                MODULE.find_open("https://forge.invalid", "qos/qos", "secret", self.branch),
                pulls[0],
            )

    def test_main_creates_digest_bound_pull(self):
        created = {"number": 8, "html_url": "https://forge.invalid/qos/qos/pulls/8"}
        with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                mock.patch.object(sys, "argv", self.argv()), \
                mock.patch.object(MODULE, "find_open", return_value=None), \
                mock.patch.object(MODULE, "request", return_value=created) as request:
            self.assertEqual(MODULE.main(), 0)
        payload = request.call_args.args[5]
        self.assertEqual(payload["base"], "main")
        self.assertEqual(payload["head"], self.branch)
        self.assertIn(self.digest, payload["body"])

    def test_ignores_same_named_branch_from_fork(self):
        pulls = [{
            "number": 9,
            "html_url": "https://forge.invalid/qos/qos/pulls/9",
            "head": {"ref": self.branch, "repo": {"full_name": "attacker/qos"}},
            "base": {"ref": "main"},
        }]
        with mock.patch.object(MODULE, "request", return_value=pulls):
            self.assertIsNone(
                MODULE.find_open("https://forge.invalid", "qos/qos", "secret", self.branch)
            )

    def test_rejects_branch_not_bound_to_snapshot(self):
        with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                mock.patch.object(sys, "argv", self.argv(branch="automation/kernel-snapshot-wrong")), \
                self.assertRaises(SystemExit):
            MODULE.main()

    def test_rejects_snapshot_digest_not_bound_to_both_refs(self):
        with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                mock.patch.object(sys, "argv", self.argv(digest="c" * 64)), \
                self.assertRaises(SystemExit):
            MODULE.main()

    def test_rejects_shared_or_mutable_native_ref(self):
        cases = (
            {"arm": self.x86_ref},
            {"arm": "code.podesta.ai/qos/tilo-kernel:latest"},
        )
        for overrides in cases:
            with self.subTest(overrides=overrides), \
                    mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                    mock.patch.object(sys, "argv", self.argv(**overrides)), \
                    self.assertRaises(SystemExit):
                MODULE.main()


if __name__ == "__main__":
    unittest.main()

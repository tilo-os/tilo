#!/usr/bin/env python3
"""Tests for protected-branch RPM snapshot promotion."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import sys
import unittest
from unittest import mock


SCRIPT = Path(__file__).parents[1] / "dev" / "open-rpm-snapshot-pr.py"
SPEC = importlib.util.spec_from_file_location("open_rpm_snapshot_pr", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class SnapshotPullRequestTests(unittest.TestCase):
    digest = "a" * 64
    branch = "automation/rpm-snapshot-" + digest[:16]

    def test_finds_existing_open_pull(self):
        pulls = [{
            "number": 7,
            "html_url": "https://forge.invalid/qos/qos/pulls/7",
            "head": {"ref": self.branch, "repo": {"full_name": "qos/qos"}},
            "base": {"ref": "main"},
        }]
        with mock.patch.object(MODULE, "request", return_value=pulls) as request:
            self.assertEqual(
                MODULE.find_open("https://forge.invalid", "qos/qos", "secret", self.branch),
                pulls[0],
            )
        request.assert_called_once_with(
            "https://forge.invalid", "qos/qos", "secret", "/pulls?state=open&limit=50&page=1"
        )

    def test_main_creates_pull_with_digest_bound_branch(self):
        created = {"number": 8, "html_url": "https://forge.invalid/qos/qos/pulls/8"}
        argv = [
            str(SCRIPT), "--server", "https://forge.invalid", "--repository", "qos/qos",
            "--branch", self.branch, "--digest", self.digest,
        ]
        with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                mock.patch.object(sys, "argv", argv), \
                mock.patch.object(MODULE, "find_open", return_value=None), \
                mock.patch.object(MODULE, "request", return_value=created) as request:
            self.assertEqual(MODULE.main(), 0)
        payload = request.call_args.args[5]
        self.assertEqual(payload["base"], "main")
        self.assertEqual(payload["head"], self.branch)
        self.assertIn(self.digest, payload["body"])

    def test_ignores_same_named_branch_from_a_fork(self):
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

    def test_rejects_branch_not_bound_to_digest(self):
        argv = [
            str(SCRIPT), "--server", "https://forge.invalid", "--repository", "qos/qos",
            "--branch", "automation/rpm-snapshot-wrong", "--digest", self.digest,
        ]
        with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                mock.patch.object(sys, "argv", argv), \
                self.assertRaises(SystemExit):
            MODULE.main()


if __name__ == "__main__":
    unittest.main()

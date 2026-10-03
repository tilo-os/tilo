#!/usr/bin/env python3
"""Tests for protected-branch snapshot pin promotion."""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import sys
import unittest
from unittest import mock


SCRIPT = Path(__file__).parents[1] / "dev" / "open-snapshot-pr.py"
SPEC = importlib.util.spec_from_file_location("open_snapshot_pr", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)

REPO = Path(__file__).parents[2]


def run_main(argv, created=None):
    created = created or {"number": 8, "html_url": "https://forge.invalid/tilo/tilo/pulls/8"}
    with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
            mock.patch.object(sys, "argv", argv), \
            mock.patch.object(MODULE, "find_open", return_value=None), \
            mock.patch.object(MODULE, "request", return_value=created) as request:
        result = MODULE.main()
    return result, request


def print_pins(kind, x86_ref, arm_ref):
    out = io.StringIO()
    argv = [str(SCRIPT), "pins", kind, "--x86-ref", x86_ref, "--arm-ref", arm_ref]
    with mock.patch.object(sys, "argv", argv), contextlib.redirect_stdout(out):
        assert MODULE.main() == 0
    return out.getvalue()


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


class KernelSnapshotPullRequestTests(unittest.TestCase):
    x86_ref = "registry.tilo.so/tilo/tilo-kernel@sha256:" + "a" * 64
    arm_ref = "registry.tilo.so/tilo/tilo-kernel@sha256:" + "b" * 64

    def setUp(self):
        contents = MODULE.kernel_contents(self.x86_ref, self.arm_ref)
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
            "--repository", "tilo/tilo", "--branch", values["branch"],
            "--snapshot-digest", values["digest"], "kernel",
            "--x86-ref", values["x86"], "--arm-ref", values["arm"],
        ]

    def test_kernel_pull_creation(self):
        with self.subTest(phase="finds_existing_open_pull_from_same_repository"):
            pulls = [{
                "number": 7,
                "html_url": "https://forge.invalid/tilo/tilo/pulls/7",
                "head": {"ref": self.branch, "repo": {"full_name": "tilo/tilo"}},
                "base": {"ref": "main"},
            }]
            with mock.patch.object(MODULE, "request", return_value=pulls):
                self.assertEqual(
                    MODULE.find_open("https://forge.invalid", "tilo/tilo", "secret", self.branch),
                    pulls[0],
                )
        restart_fixture(self)
        with self.subTest(phase="main_creates_digest_bound_pull"):
            result, request = run_main(self.argv())
            self.assertEqual(result, 0)
            payload = request.call_args.args[5]
            self.assertEqual(payload["base"], "main")
            self.assertEqual(payload["head"], self.branch)
            self.assertEqual(payload["title"], "build(os): refresh native kernel snapshots")
            self.assertIn(self.digest, payload["body"])
        restart_fixture(self)
        with self.subTest(phase="ignores_same_named_branch_from_fork"):
            pulls = [{
                "number": 9,
                "html_url": "https://forge.invalid/tilo/tilo/pulls/9",
                "head": {"ref": self.branch, "repo": {"full_name": "attacker/tilo"}},
                "base": {"ref": "main"},
            }]
            with mock.patch.object(MODULE, "request", return_value=pulls):
                self.assertIsNone(
                    MODULE.find_open("https://forge.invalid", "tilo/tilo", "secret", self.branch)
                )

    def test_kernel_pull_binding_rejections(self):
        with self.subTest(phase="rejects_branch_not_bound_to_snapshot"):
            for branch in ("automation/kernel-snapshot-wrong",
                           "automation/rpm-snapshot-" + self.digest[:16]):
                with self.subTest(branch=branch), self.assertRaises(SystemExit):
                    run_main(self.argv(branch=branch))
        restart_fixture(self)
        with self.subTest(phase="rejects_snapshot_digest_not_bound_to_both_refs"):
            with self.assertRaises(SystemExit):
                run_main(self.argv(digest="c" * 64))
        restart_fixture(self)
        with self.subTest(phase="rejects_shared_or_mutable_native_ref"):
            cases = (
                {"arm": self.x86_ref},
                {"arm": "registry.tilo.so/tilo/tilo-kernel:latest"},
            )
            for overrides in cases:
                with self.subTest(overrides=overrides), self.assertRaises(SystemExit):
                    run_main(self.argv(**overrides))


class TokenFailureReportingTests(unittest.TestCase):
    x86_ref = "registry.tilo.so/tilo/tilo-kernel@sha256:" + "a" * 64
    arm_ref = "registry.tilo.so/tilo/tilo-kernel@sha256:" + "b" * 64

    def argv(self):
        contents = MODULE.kernel_contents(self.x86_ref, self.arm_ref)
        digest = hashlib.sha256(contents.encode()).hexdigest()
        return [
            str(SCRIPT), "--server", "https://forge.invalid", "--repository", "tilo/tilo",
            "--branch", "automation/kernel-snapshot-" + digest[:16],
            "--snapshot-digest", digest, "kernel",
            "--x86-ref", self.x86_ref, "--arm-ref", self.arm_ref,
        ]

    @staticmethod
    def http_error(code):
        return MODULE.urllib.error.HTTPError("https://forge.invalid", code, "denied", {}, None)

    def run_with_listing_error(self, code):
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                mock.patch.object(sys, "argv", self.argv()), \
                mock.patch.object(MODULE, "request", side_effect=self.http_error(code)), \
                contextlib.redirect_stderr(stderr):
            result = MODULE.main()
        return result, stderr.getvalue()

    def test_token_failure_messages(self):
        with self.subTest(phase="unreadable_pull_requests_name_the_token_to_set"):
            # Forgejo reports repositories the token cannot see as 404.
            for code in (401, 403, 404):
                with self.subTest(code=code):
                    result, stderr = self.run_with_listing_error(code)
                    self.assertEqual(result, 1)
                    self.assertIn("token cannot read pull requests on tilo/tilo", stderr)
                    self.assertIn("RELEASE_TOKEN", stderr)
                    self.assertNotIn("Traceback", stderr)
        with self.subTest(phase="server_error_is_not_blamed_on_the_token"):
            result, stderr = self.run_with_listing_error(502)
            self.assertEqual(result, 1)
            self.assertIn("HTTP 502", stderr)
            self.assertNotIn("RELEASE_TOKEN", stderr)
        with self.subTest(phase="refused_creation_names_the_token_to_set"):
            stderr = io.StringIO()
            with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                    mock.patch.object(sys, "argv", self.argv()), \
                    mock.patch.object(MODULE, "find_open", return_value=None), \
                    mock.patch.object(MODULE, "request", side_effect=self.http_error(403)), \
                    contextlib.redirect_stderr(stderr):
                self.assertEqual(MODULE.main(), 1)
            self.assertIn("refused kernel snapshot pull-request creation (HTTP 403)", stderr.getvalue())
            self.assertIn("RELEASE_TOKEN", stderr.getvalue())
        with self.subTest(phase="missing_token_names_the_secret"):
            stderr = io.StringIO()
            with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": ""}), \
                    mock.patch.object(sys, "argv", self.argv()), \
                    contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
                MODULE.main()
            self.assertIn("RELEASE_TOKEN", stderr.getvalue())



class WpeSnapshotPullRequestTests(unittest.TestCase):
    x86_ref = "registry.tilo.so/tilo/tilo-wpe-buildroot@sha256:" + "a" * 64
    arm_ref = "registry.tilo.so/tilo/tilo-wpe-buildroot@sha256:" + "b" * 64

    def setUp(self):
        contents = MODULE.wpe_contents(self.x86_ref, self.arm_ref)
        self.digest = hashlib.sha256(contents.encode()).hexdigest()
        self.branch = "automation/wpe-buildroot-snapshot-" + self.digest[:16]

    def argv(self, **overrides):
        values = {"branch": self.branch, "digest": self.digest,
                  "x86": self.x86_ref, "arm": self.arm_ref}
        values.update(overrides)
        return [
            str(SCRIPT), "--server", "https://forge.invalid",
            "--repository", "tilo/tilo", "--branch", values["branch"],
            "--snapshot-digest", values["digest"], "wpe",
            "--x86-ref", values["x86"], "--arm-ref", values["arm"],
        ]

    def test_wpe_snapshot_pull(self):
        with self.subTest(phase="main_creates_digest_bound_pull"):
            result, request = run_main(self.argv())
            self.assertEqual(result, 0)
            payload = request.call_args.args[5]
            self.assertEqual(payload["head"], self.branch)
            self.assertEqual(payload["title"], "build(os): refresh native WPE buildroot snapshots")
            self.assertIn(self.digest, payload["body"])
        restart_fixture(self)
        with self.subTest(phase="promotion_writes_the_canonical_pin_file"):
            self.assertEqual(print_pins("wpe", self.x86_ref, self.arm_ref),
                             MODULE.wpe_contents(self.x86_ref, self.arm_ref))
            script = (REPO / "os/dev/buildroot-snapshot.sh").read_text()
            self.assertIn('open-snapshot-pr.py pins "$promote_kind"', script)
            self.assertIn("wpe) key=WPE_BUILDROOT label=WPE promote_kind=wpe", script)
        restart_fixture(self)
        with self.subTest(phase="committed_pins_use_the_canonical_names"):
            committed = (REPO / "os/wpe-buildroot-snapshot.env").read_text()
            refs = dict(line.split("=", 1) for line in committed.splitlines()
                        if line and not line.startswith("#"))
            self.assertEqual(set(refs), {"WPE_BUILDROOT_X86_64_REF", "WPE_BUILDROOT_AARCH64_REF"})
            if all(refs.values()):
                self.assertEqual(
                    MODULE.wpe_contents(refs["WPE_BUILDROOT_X86_64_REF"],
                                        refs["WPE_BUILDROOT_AARCH64_REF"]),
                    committed,
                )
        restart_fixture(self)
        with self.subTest(phase="rejects_shared_mutable_or_foreign_ref"):
            cases = (
                {"arm": self.x86_ref},
                {"arm": "registry.tilo.so/tilo/tilo-wpe-buildroot:latest"},
                {"arm": "registry.tilo.so/tilo/tilo-kernel@sha256:" + "b" * 64},
            )
            for overrides in cases:
                with self.subTest(overrides=overrides), self.assertRaises(SystemExit):
                    run_main(self.argv(**overrides))
        restart_fixture(self)
        with self.subTest(phase="rejects_digest_or_branch_not_bound_to_refs"):
            for overrides in ({"digest": "e" * 64},
                              {"branch": "automation/kernel-snapshot-" + self.digest[:16]}):
                with self.subTest(overrides=overrides), self.assertRaises(SystemExit):
                    run_main(self.argv(**overrides))


class KernelBuildrootSnapshotPullRequestTests(unittest.TestCase):
    x86_ref = "registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:" + "a" * 64
    arm_ref = "registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:" + "b" * 64

    def setUp(self):
        contents = MODULE.kernel_buildroot_contents(self.x86_ref, self.arm_ref)
        self.digest = hashlib.sha256(contents.encode()).hexdigest()
        self.branch = "automation/kernel-buildroot-snapshot-" + self.digest[:16]

    def argv(self, **overrides):
        values = {"branch": self.branch, "digest": self.digest,
                  "x86": self.x86_ref, "arm": self.arm_ref}
        values.update(overrides)
        return [
            str(SCRIPT), "--server", "https://forge.invalid",
            "--repository", "tilo/tilo", "--branch", values["branch"],
            "--snapshot-digest", values["digest"], "kernel-buildroot",
            "--x86-ref", values["x86"], "--arm-ref", values["arm"],
        ]

    def test_kernel_buildroot_snapshot_pull(self):
        with self.subTest(phase="main_creates_digest_bound_pull"):
            result, request = run_main(self.argv())
            self.assertEqual(result, 0)
            payload = request.call_args.args[5]
            self.assertEqual(payload["head"], self.branch)
            self.assertEqual(payload["title"],
                             "build(os): refresh native kernel buildroot snapshots")
            self.assertIn(self.digest, payload["body"])
        restart_fixture(self)
        with self.subTest(phase="promotion_writes_the_canonical_pin_file"):
            self.assertEqual(print_pins("kernel-buildroot", self.x86_ref, self.arm_ref),
                             MODULE.kernel_buildroot_contents(self.x86_ref, self.arm_ref))
            script = (REPO / "os/dev/buildroot-snapshot.sh").read_text()
            self.assertIn("kernel) key=KERNEL_BUILDROOT label=kernel promote_kind=kernel-buildroot",
                          script)
            self.assertIn('branch="automation/$kind-buildroot-snapshot-${snapshot_digest:0:16}"',
                          script)
        restart_fixture(self)
        with self.subTest(phase="committed_pins_use_the_canonical_names"):
            committed = (REPO / "os/kernel-buildroot-snapshot.env").read_text()
            refs = dict(line.split("=", 1) for line in committed.splitlines()
                        if line and not line.startswith("#"))
            self.assertEqual(set(refs), {"KERNEL_BUILDROOT_X86_64_REF",
                                         "KERNEL_BUILDROOT_AARCH64_REF"})
            if all(refs.values()):
                self.assertEqual(
                    MODULE.kernel_buildroot_contents(refs["KERNEL_BUILDROOT_X86_64_REF"],
                                                     refs["KERNEL_BUILDROOT_AARCH64_REF"]),
                    committed,
                )
        restart_fixture(self)
        with self.subTest(phase="rejects_shared_mutable_or_foreign_ref"):
            cases = (
                {"arm": self.x86_ref},
                {"arm": "registry.tilo.so/tilo/tilo-kernel-buildroot:latest"},
                {"arm": "registry.tilo.so/tilo/tilo-wpe-buildroot@sha256:" + "b" * 64},
                {"arm": "registry.tilo.so/tilo/rpms@sha256:" + "b" * 64},
            )
            for overrides in cases:
                with self.subTest(overrides=overrides), self.assertRaises(SystemExit):
                    run_main(self.argv(**overrides))
        restart_fixture(self)
        with self.subTest(phase="rejects_digest_or_branch_not_bound_to_refs"):
            for overrides in ({"digest": "e" * 64},
                              {"branch": "automation/wpe-buildroot-snapshot-" + self.digest[:16]}):
                with self.subTest(overrides=overrides), self.assertRaises(SystemExit):
                    run_main(self.argv(**overrides))



class WpeStageSnapshotPullRequestTests(unittest.TestCase):
    x86_ref = "registry.tilo.so/tilo/tilo-wpe-stage@sha256:" + "c" * 64
    arm_ref = "registry.tilo.so/tilo/tilo-wpe-stage@sha256:" + "d" * 64

    def test_wpe_stage_snapshot_pull(self):
        contents = MODULE.wpe_stage_contents(self.x86_ref, self.arm_ref)
        digest = hashlib.sha256(contents.encode()).hexdigest()
        branch = "automation/wpe-stage-snapshot-" + digest[:16]
        argv = [
            str(SCRIPT), "--server", "https://forge.invalid",
            "--repository", "tilo/tilo", "--branch", branch,
            "--snapshot-digest", digest, "wpe-stage",
            "--x86-ref", self.x86_ref, "--arm-ref", self.arm_ref,
        ]
        with self.subTest(phase="the_promotion_pushes_the_branch_it_names"):
            script = (REPO / "os/dev/buildroot-snapshot.sh").read_text()
            self.assertIn('branch="automation/wpe-stage-snapshot-${snapshot_digest:0:16}"', script)
        with self.subTest(phase="main_creates_digest_bound_pull"):
            result, request = run_main(argv)
            self.assertEqual(result, 0)
            payload = request.call_args.args[5]
            self.assertEqual(payload["head"], branch)
            self.assertEqual(payload["title"], "build(os): refresh native WPE engine stages")
        restart_fixture(self)
        with self.subTest(phase="promotion_writes_the_canonical_pin_file"):
            self.assertEqual(print_pins("wpe-stage", self.x86_ref, self.arm_ref), contents)
            script = (REPO / "os/dev/buildroot-snapshot.sh").read_text()
            self.assertIn("wpe-stage) key=WPE_STAGE label=\"WPE engine\" promote_kind=wpe-stage", script)
        restart_fixture(self)
        with self.subTest(phase="committed_pins_use_the_canonical_names"):
            committed = (REPO / "os/wpe-stage-snapshot.env").read_text()
            refs = dict(line.split("=", 1) for line in committed.splitlines()
                        if line and not line.startswith("#"))
            self.assertEqual(set(refs), {"WPE_STAGE_X86_64_REF", "WPE_STAGE_AARCH64_REF"})
        restart_fixture(self)
        with self.subTest(phase="rejects_buildroot_or_mutable_refs"):
            for arm in ("registry.tilo.so/tilo/tilo-wpe-buildroot@sha256:" + "d" * 64,
                        "registry.tilo.so/tilo/tilo-wpe-stage:latest", self.x86_ref):
                bad = list(argv)
                bad[-1] = arm
                with self.subTest(arm=arm), self.assertRaises(SystemExit):
                    run_main(bad)

if __name__ == "__main__":
    unittest.main()

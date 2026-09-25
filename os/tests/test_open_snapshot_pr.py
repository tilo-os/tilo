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
    created = created or {"number": 8, "html_url": "https://forge.invalid/qos/qos/pulls/8"}
    with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
            mock.patch.object(sys, "argv", argv), \
            mock.patch.object(MODULE, "find_open", return_value=None), \
            mock.patch.object(MODULE, "request", return_value=created) as request:
        result = MODULE.main()
    return result, request


class KernelSnapshotPullRequestTests(unittest.TestCase):
    x86_ref = "code.sandbox.example/qos/tilo-kernel@sha256:" + "a" * 64
    arm_ref = "code.sandbox.example/qos/tilo-kernel@sha256:" + "b" * 64

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
            "--repository", "qos/qos", "--branch", values["branch"],
            "--snapshot-digest", values["digest"], "kernel",
            "--x86-ref", values["x86"], "--arm-ref", values["arm"],
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
        result, request = run_main(self.argv())
        self.assertEqual(result, 0)
        payload = request.call_args.args[5]
        self.assertEqual(payload["base"], "main")
        self.assertEqual(payload["head"], self.branch)
        self.assertEqual(payload["title"], "build(os): refresh native kernel snapshots")
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
        for branch in ("automation/kernel-snapshot-wrong",
                       "automation/rpm-snapshot-" + self.digest[:16]):
            with self.subTest(branch=branch), self.assertRaises(SystemExit):
                run_main(self.argv(branch=branch))

    def test_rejects_snapshot_digest_not_bound_to_both_refs(self):
        with self.assertRaises(SystemExit):
            run_main(self.argv(digest="c" * 64))

    def test_rejects_shared_or_mutable_native_ref(self):
        cases = (
            {"arm": self.x86_ref},
            {"arm": "code.sandbox.example/qos/tilo-kernel:latest"},
        )
        for overrides in cases:
            with self.subTest(overrides=overrides), self.assertRaises(SystemExit):
                run_main(self.argv(**overrides))


class TokenFailureReportingTests(unittest.TestCase):
    x86_ref = "code.sandbox.example/qos/tilo-kernel@sha256:" + "a" * 64
    arm_ref = "code.sandbox.example/qos/tilo-kernel@sha256:" + "b" * 64

    def argv(self):
        contents = MODULE.kernel_contents(self.x86_ref, self.arm_ref)
        digest = hashlib.sha256(contents.encode()).hexdigest()
        return [
            str(SCRIPT), "--server", "https://forge.invalid", "--repository", "qos/qos",
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

    def test_unreadable_pull_requests_name_the_token_to_set(self):
        # Forgejo reports repositories the token cannot see as 404.
        for code in (401, 403, 404):
            with self.subTest(code=code):
                result, stderr = self.run_with_listing_error(code)
                self.assertEqual(result, 1)
                self.assertIn("token cannot read pull requests on qos/qos", stderr)
                self.assertIn("RELEASE_TOKEN", stderr)
                self.assertNotIn("Traceback", stderr)

    def test_server_error_is_not_blamed_on_the_token(self):
        result, stderr = self.run_with_listing_error(502)
        self.assertEqual(result, 1)
        self.assertIn("HTTP 502", stderr)
        self.assertNotIn("RELEASE_TOKEN", stderr)

    def test_refused_creation_names_the_token_to_set(self):
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                mock.patch.object(sys, "argv", self.argv()), \
                mock.patch.object(MODULE, "find_open", return_value=None), \
                mock.patch.object(MODULE, "request", side_effect=self.http_error(403)), \
                contextlib.redirect_stderr(stderr):
            self.assertEqual(MODULE.main(), 1)
        self.assertIn("refused kernel snapshot pull-request creation (HTTP 403)", stderr.getvalue())
        self.assertIn("RELEASE_TOKEN", stderr.getvalue())

    def test_missing_token_names_the_secret(self):
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": ""}), \
                mock.patch.object(sys, "argv", self.argv()), \
                contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
            MODULE.main()
        self.assertIn("RELEASE_TOKEN", stderr.getvalue())


class RpmAndKernelSnapshotPullRequestTests(unittest.TestCase):
    rpms_ref = "code.sandbox.example/qos/rpms@sha256:" + "d" * 64
    x86_ref = "code.sandbox.example/qos/tilo-kernel@sha256:" + "a" * 64
    arm_ref = "code.sandbox.example/qos/tilo-kernel@sha256:" + "b" * 64
    source = "c" * 40

    def setUp(self):
        contents = (MODULE.rpms_contents(self.rpms_ref, self.source)
                    + MODULE.kernel_contents(self.x86_ref, self.arm_ref))
        self.digest = hashlib.sha256(contents.encode()).hexdigest()
        self.branch = "automation/rpm-kernel-snapshot-" + self.digest[:16]

    def argv(self, **overrides):
        values = {"branch": self.branch, "digest": self.digest, "ref": self.rpms_ref,
                  "source": self.source}
        values.update(overrides)
        return [
            str(SCRIPT), "--server", "https://forge.invalid",
            "--repository", "qos/qos", "--branch", values["branch"],
            "--snapshot-digest", values["digest"], "kernel", "--rpms-ref", values["ref"],
            "--rpms-source-revision", values["source"],
            "--x86-ref", self.x86_ref, "--arm-ref", self.arm_ref,
        ]

    def test_main_creates_one_pull_for_both_pins(self):
        result, request = run_main(self.argv())
        self.assertEqual(result, 0)
        payload = request.call_args.args[5]
        self.assertEqual(payload["head"], self.branch)
        self.assertEqual(payload["title"],
                         "build(os): refresh the RPM snapshot and its native kernels")
        self.assertIn(self.digest, payload["body"])

    def test_canonical_contents_match_the_committed_pin_shape(self):
        committed = (REPO / "os/rpms-snapshot.env").read_text()
        fields = dict(line.split("=", 1) for line in committed.splitlines()
                      if line and not line.startswith("#"))
        # A pin published before source revisions were recorded lacks one.
        if "RPMS_SOURCE_REVISION" in fields:
            self.assertEqual(MODULE.rpms_contents(fields["RPMS_REF"],
                                                  fields["RPMS_SOURCE_REVISION"]), committed)
        else:
            self.assertEqual(set(fields), {"RPMS_REF"})

    def test_workflow_writes_the_canonical_pin_files(self):
        workflow = (REPO / ".forgejo/workflows/build-kernel.yml").read_text()
        for line in (MODULE.rpms_contents("$CALLER_RPMS_REF", "$GITHUB_SHA").splitlines()[:2]
                     + MODULE.kernel_contents("$X", "$Y").splitlines()[:2]):
            self.assertIn(f"'{line}'", workflow)
        self.assertIn('"RPMS_REF=$CALLER_RPMS_REF"', workflow)
        self.assertIn('"RPMS_SOURCE_REVISION=$GITHUB_SHA" > os/rpms-snapshot.env', workflow)
        self.assertIn('pins=(os/rpms-snapshot.env os/kernel-snapshot.env)', workflow)
        self.assertIn('snapshot_digest="$(cat "${pins[@]}" | sha256sum', workflow)

    def test_rejects_mutable_or_foreign_ref(self):
        for ref in ("code.sandbox.example/qos/rpms:latest",
                    "code.sandbox.example/qos/tilo-kernel@sha256:" + "d" * 64):
            with self.subTest(ref=ref), self.assertRaises(SystemExit):
                run_main(self.argv(ref=ref))

    def test_rejects_abbreviated_or_missing_source_revision(self):
        for source in ("c" * 12, ""):
            with self.subTest(source=source), self.assertRaises(SystemExit):
                run_main(self.argv(source=source))

    def test_closes_superseded_open_pin_pulls_only(self):
        older = "automation/rpm-snapshot-" + "1" * 16
        gone = "automation/kernel-snapshot-" + "2" * 16
        argv = self.argv()
        argv[argv.index("kernel"):argv.index("kernel")] = [
            "--supersede", older, "--supersede", gone]
        opened = {older: {"number": 5}, gone: None, self.branch: None}
        calls = []

        def request(server, repository, token, path, method="GET", payload=None):
            calls.append((path, method, payload))
            return {"number": 8, "html_url": "https://forge.invalid/qos/qos/pulls/8"}

        with mock.patch.dict(os.environ, {"FORGEJO_TOKEN": "secret"}), \
                mock.patch.object(sys, "argv", argv), \
                mock.patch.object(MODULE, "find_open",
                                  side_effect=lambda s, r, t, branch: opened[branch]), \
                mock.patch.object(MODULE, "request", side_effect=request), \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(MODULE.main(), 0)
        self.assertEqual(calls[-1], ("/pulls/5", "PATCH", {"state": "closed"}))
        self.assertEqual([c for c in calls if c[1] == "PATCH"], [calls[-1]])

    def test_never_supersedes_a_non_automation_branch(self):
        argv = self.argv()
        argv[argv.index("kernel"):argv.index("kernel")] = ["--supersede", "main"]
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(io.StringIO()):
            result, request = run_main(argv)
        self.assertEqual(result, 1)
        self.assertNotIn("PATCH", [call.args[4] for call in request.call_args_list
                                   if len(call.args) > 4])

    def test_rejects_digest_or_branch_not_bound_to_both_pins(self):
        kernel_only = MODULE.kernel_contents(self.x86_ref, self.arm_ref)
        kernel_digest = hashlib.sha256(kernel_only.encode()).hexdigest()
        for overrides in ({"digest": "e" * 64},
                          {"digest": kernel_digest},
                          {"branch": "automation/kernel-snapshot-" + self.digest[:16]},
                          {"branch": "automation/rpm-snapshot-" + self.digest[:16]}):
            with self.subTest(overrides=overrides), self.assertRaises(SystemExit):
                run_main(self.argv(**overrides))


class WpeSnapshotPullRequestTests(unittest.TestCase):
    x86_ref = "code.sandbox.example/qos/tilo-wpe-buildroot@sha256:" + "a" * 64
    arm_ref = "code.sandbox.example/qos/tilo-wpe-buildroot@sha256:" + "b" * 64

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
            "--repository", "qos/qos", "--branch", values["branch"],
            "--snapshot-digest", values["digest"], "wpe",
            "--x86-ref", values["x86"], "--arm-ref", values["arm"],
        ]

    def test_main_creates_digest_bound_pull(self):
        result, request = run_main(self.argv())
        self.assertEqual(result, 0)
        payload = request.call_args.args[5]
        self.assertEqual(payload["head"], self.branch)
        self.assertEqual(payload["title"], "build(os): refresh native WPE buildroot snapshots")
        self.assertIn(self.digest, payload["body"])

    def test_workflow_writes_the_canonical_pin_file(self):
        workflow = (REPO / ".forgejo/workflows/snapshot-wpe-buildroot.yml").read_text()
        for line in MODULE.wpe_contents("$WPE_BUILDROOT_X86_64_REF",
                                        "$WPE_BUILDROOT_AARCH64_REF").splitlines()[:2]:
            self.assertIn(f"'{line}'", workflow)
        self.assertIn('"WPE_BUILDROOT_X86_64_REF=$WPE_BUILDROOT_X86_64_REF"', workflow)
        self.assertIn('"WPE_BUILDROOT_AARCH64_REF=$WPE_BUILDROOT_AARCH64_REF"', workflow)

    def test_committed_pins_use_the_canonical_names(self):
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

    def test_rejects_shared_mutable_or_foreign_ref(self):
        cases = (
            {"arm": self.x86_ref},
            {"arm": "code.sandbox.example/qos/tilo-wpe-buildroot:latest"},
            {"arm": "code.sandbox.example/qos/tilo-kernel@sha256:" + "b" * 64},
        )
        for overrides in cases:
            with self.subTest(overrides=overrides), self.assertRaises(SystemExit):
                run_main(self.argv(**overrides))

    def test_rejects_digest_or_branch_not_bound_to_refs(self):
        for overrides in ({"digest": "e" * 64},
                          {"branch": "automation/kernel-snapshot-" + self.digest[:16]}):
            with self.subTest(overrides=overrides), self.assertRaises(SystemExit):
                run_main(self.argv(**overrides))


if __name__ == "__main__":
    unittest.main()

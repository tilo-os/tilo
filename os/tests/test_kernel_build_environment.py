#!/usr/bin/env python3
"""Tests for selecting the signed build environment a kernel's provenance binds."""

from __future__ import annotations

import base64
import json
from pathlib import Path
import subprocess
import sys
import unittest

SCRIPT = Path(__file__).parents[1] / "dev" / "kernel-build-environment.py"
DIGEST = "sha256:" + "1" * 64
SOURCE = "git+ssh://code.podesta.ai/qos/qos@" + "2" * 40
BUILDROOT = "code.podesta.ai/qos/tilo-kernel-buildroot@sha256:" + "3" * 64
BASE = "code.podesta.ai/qos/fedora-bootc@sha256:" + "4" * 64
RPMS = "code.podesta.ai/qos/rpms@sha256:" + "5" * 64


def envelope(uris, architecture="x86_64", digest=DIGEST,
             build_type="https://tilo.sh/buildtypes/kernel-rpm/v1"):
    statement = {"predicate": {"buildDefinition": {
        "buildType": build_type,
        "externalParameters": {"architecture": architecture},
        "internalParameters": {"expectedReleaseDigest": digest},
        "resolvedDependencies": [{"uri": uri} for uri in uris],
    }}}
    return {"payload": base64.b64encode(json.dumps(statement).encode()).decode()}


def run(*envelopes, architecture="x86_64", digest=DIGEST, as_array=False):
    text = (json.dumps(list(envelopes)) if as_array
            else "\n".join(json.dumps(item) for item in envelopes))
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--digest", digest, "--architecture", architecture],
        input=text, capture_output=True, text=True)


class KernelBuildEnvironmentTests(unittest.TestCase):
    def test_selects_the_kernel_buildroot(self):
        result = run(envelope([SOURCE, BUILDROOT, BASE]))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), BUILDROOT)

    def test_prefers_the_buildroot_over_any_rpm_snapshot(self):
        result = run(envelope([SOURCE, BUILDROOT, BASE, RPMS]), as_array=True)
        self.assertEqual(result.stdout.strip(), BUILDROOT)

    def test_accepts_a_kernel_published_before_the_buildroot(self):
        result = run(envelope([SOURCE, BASE, RPMS]))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), RPMS)

    def test_ignores_the_os_rpm_snapshot_and_base(self):
        # Any signed RPM snapshot is accepted; no equality with the OS pins.
        other = "code.podesta.ai/qos/rpms@sha256:" + "6" * 64
        self.assertEqual(run(envelope([SOURCE, other])).stdout.strip(), other)

    def test_rejects_missing_or_ambiguous_environments(self):
        other = "code.podesta.ai/qos/tilo-kernel-buildroot@sha256:" + "7" * 64
        cases = (
            envelope([SOURCE, BASE]),
            envelope([SOURCE, BUILDROOT, other]),
            envelope([BUILDROOT, BASE]),  # no qos/qos source commit
            envelope([SOURCE, "code.podesta.ai/qos/tilo-kernel-buildroot:latest"]),
            envelope([SOURCE, "code.podesta.ai/other/rpms@sha256:" + "5" * 64]),
        )
        for case in cases:
            with self.subTest(case=case):
                self.assertEqual(run(case).returncode, 1)

    def test_two_predicates_naming_different_environments_are_refused(self):
        other = "code.podesta.ai/qos/tilo-kernel-buildroot@sha256:" + "7" * 64
        self.assertEqual(run(envelope([SOURCE, BUILDROOT]), envelope([SOURCE, other])).returncode, 1)

    def test_binds_digest_architecture_and_build_type(self):
        self.assertEqual(run(envelope([SOURCE, BUILDROOT]), architecture="aarch64").returncode, 1)
        self.assertEqual(run(envelope([SOURCE, BUILDROOT], digest="sha256:" + "8" * 64)).returncode, 1)
        self.assertEqual(run(envelope([SOURCE, BUILDROOT],
                                      build_type="https://example.invalid/kernel-rpm/v1")).returncode, 1)

    def test_rejects_unparseable_input(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--digest", DIGEST, "--architecture", "x86_64"],
            input="not json", capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)


if __name__ == "__main__":
    unittest.main()

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
SOURCE = "git+ssh://code.tilo.so/tilo/tilo@" + "2" * 40
BUILDROOT = "registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:" + "3" * 64
BASE = "registry.tilo.so/tilo/fedora-bootc@sha256:" + "4" * 64
RPMS = "registry.tilo.so/tilo/rpms@sha256:" + "5" * 64


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
    def test_environment_selection(self):
        with self.subTest(phase="selects_the_kernel_buildroot"):
            result = run(envelope([SOURCE, BUILDROOT, BASE]))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), BUILDROOT)
        with self.subTest(phase="ignores_any_rpm_snapshot_the_provenance_also_names"):
            result = run(envelope([SOURCE, BUILDROOT, BASE, RPMS]), as_array=True)
            self.assertEqual(result.stdout.strip(), BUILDROOT)
        with self.subTest(phase="binds_digest_architecture_and_build_type"):
            self.assertEqual(run(envelope([SOURCE, BUILDROOT]), architecture="aarch64").returncode, 1)
            self.assertEqual(run(envelope([SOURCE, BUILDROOT], digest="sha256:" + "8" * 64)).returncode, 1)
            self.assertEqual(run(envelope([SOURCE, BUILDROOT],
                                          build_type="https://example.invalid/kernel-rpm/v1")).returncode, 1)

    def test_valid_predicate_cannot_mask_an_unusable_matching_predicate(self):
        valid = envelope([SOURCE, BUILDROOT])
        other = "registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:" + "7" * 64
        for invalid in (envelope([SOURCE, BUILDROOT, other]), envelope([SOURCE, BASE])):
            for pair in ((valid, invalid), (invalid, valid)):
                with self.subTest(pair=pair):
                    result = run(*pair)
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertEqual(result.stdout, "")

    def test_unrelated_and_repeated_valid_predicates_remain_supported(self):
        result = run(envelope([SOURCE, BUILDROOT]), envelope([SOURCE, BUILDROOT]),
                     envelope([SOURCE, BASE], architecture="aarch64"),
                     envelope([BUILDROOT]), as_array=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), BUILDROOT)

    def test_malformed_unrelated_records_do_not_crash_selection(self):
        valid = envelope([SOURCE, BUILDROOT])
        def wrapped(statement):
            return {'payload': base64.b64encode(json.dumps(statement).encode()).decode()}
        malformed = [None, 4, {'payload': []}, wrapped(None), wrapped([]),
                     wrapped({'predicate': {'buildDefinition': ['not a definition']}})]
        for field in ('externalParameters', 'internalParameters'):
            statement = json.loads(base64.b64decode(valid['payload']))
            statement['predicate']['buildDefinition'][field] = ['not an object']
            malformed.append(wrapped(statement))
        for bad in malformed:
            with self.subTest(bad=bad):
                rejected = run(bad)
                self.assertEqual(rejected.returncode, 1, rejected.stderr)
                self.assertEqual(rejected.stderr, '')
                recovered = run(bad, valid, as_array=True)
                self.assertEqual(recovered.returncode, 0, recovered.stderr)
                self.assertEqual(recovered.stdout.strip(), BUILDROOT)

    def test_matching_malformed_dependencies_cannot_be_masked(self):
        valid = envelope([SOURCE, BUILDROOT])
        for dependencies in (1, 'not a list', {'uri': BUILDROOT}):
            statement = json.loads(base64.b64decode(valid['payload']))
            statement['predicate']['buildDefinition']['resolvedDependencies'] = dependencies
            bad = {'payload': base64.b64encode(json.dumps(statement).encode()).decode()}
            result = run(valid, bad)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, '')
            self.assertEqual(result.stderr, '')

    def test_environment_rejections(self):
        with self.subTest(phase="rejects_a_kernel_built_from_an_rpm_snapshot"):
            self.assertEqual(run(envelope([SOURCE, BASE, RPMS])).returncode, 1)
        with self.subTest(phase="rejects_missing_or_ambiguous_environments"):
            other = "registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:" + "7" * 64
            cases = (
                envelope([SOURCE, BASE]),
                envelope([SOURCE, BUILDROOT, other]),
                envelope([BUILDROOT, BASE]),  # no tilo/tilo source commit
                envelope([SOURCE, "registry.tilo.so/tilo/tilo-kernel-buildroot:latest"]),
                envelope([SOURCE, "code.tilo.so/other/rpms@sha256:" + "5" * 64]),
            )
            for case in cases:
                with self.subTest(case=case):
                    self.assertEqual(run(case).returncode, 1)
        with self.subTest(phase="two_predicates_naming_different_environments_are_refused"):
            other = "registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:" + "7" * 64
            self.assertEqual(run(envelope([SOURCE, BUILDROOT]), envelope([SOURCE, other])).returncode, 1)
        with self.subTest(phase="rejects_unparseable_input"):
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--digest", DIGEST, "--architecture", "x86_64"],
                input="not json", capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Input and environment boundaries for the provisional full OS compilation job."""
import importlib.util
import os
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('compile_os', ROOT / 'os/dev/compile-development-os.py')
driver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(driver)
WPE = 'code.sandbox.example/qos/tilo-wpe-buildroot@sha256:' + 'a' * 64
REVISION = 'b' * 40


class CompileBoundaries(unittest.TestCase):
    def test_installation_discovery_exposes_no_disks_or_network(self):
        image = 'sha256:' + 'a' * 64
        with patch.object(driver, 'run', side_effect=['--via-loopback --generic-image-extra',
                                                     'bootc 1.0',
                                                     subprocess.CalledProcessError(127, ['podman'])]) as run:
            receipt = driver.installation_tooling(image)
        self.assertTrue(receipt['flags_advertised']['--via-loopback'])
        self.assertFalse(receipt['flags_advertised']['--generic-image'])
        self.assertEqual(receipt['bootctl_version']['exit_code'], 127)
        self.assertFalse(receipt['disk_created'])
        for call in run.call_args_list:
            args = call.args
            for flag in ('--remote=false', '--pull=never', '--network=none', '--read-only',
                         '--cap-drop=ALL', '--security-opt=no-new-privileges'):
                self.assertIn(flag, args)
            self.assertIn(image, args)
            self.assertNotIn('--privileged', args)
            self.assertNotIn('--volume', args)
            self.assertNotIn('--device', args)

    def test_capacity_admission_and_abort_reserve_boundaries(self):
        for starting, threshold in ((True, 80), (False, 8)):
            with self.subTest(starting=starting):
                self.assertTrue(driver.capacity_decision(threshold * driver.GIB, starting=starting))
                self.assertFalse(driver.capacity_decision(threshold * driver.GIB - 1, starting=starting))

    def test_capacity_abort_terminates_only_owned_build_and_preserves_receipt(self):
        child = MagicMock(pid=12345)
        child.poll.return_value = None
        child.wait.return_value = -15
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            with patch.object(driver, 'EVIDENCE', output), \
                 patch.object(driver, 'memory_budget', return_value={}), \
                 patch.object(driver.shutil, 'disk_usage', return_value=SimpleNamespace(free=7 * driver.GIB)), \
                 patch.object(driver.subprocess, 'Popen', return_value=child) as launch, \
                 patch.object(driver.os, 'killpg', create=True) as terminate:
                with self.assertRaisesRegex(ValueError, '8 GiB'):
                    driver.compile_with_capacity_watchdog({'PATH': '/usr/bin'})
                self.assertTrue(launch.call_args.kwargs['start_new_session'])
                terminate.assert_called_once_with(12345, driver.signal.SIGTERM)
            result = json.loads((output / 'capacity.json').read_text())
            self.assertEqual(result['status'], 'aborted-before-guest-storage-exhaustion')
            self.assertFalse((output / 'result.json').exists())

    def test_build_failure_never_records_success(self):
        child = MagicMock(pid=12345, args=['bash', 'build-image.sh'])
        child.poll.return_value = 9
        child.wait.return_value = 9
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            with patch.object(driver, 'EVIDENCE', output), \
                 patch.object(driver, 'memory_budget', return_value={}), \
                 patch.object(driver.shutil, 'disk_usage', return_value=SimpleNamespace(free=80 * driver.GIB)), \
                 patch.object(driver.subprocess, 'Popen', return_value=child):
                with self.assertRaises(subprocess.CalledProcessError):
                    driver.compile_with_capacity_watchdog({})
            self.assertEqual(json.loads((output / 'capacity.json').read_text())['status'], 'build-failed')
            self.assertFalse((output / 'result.json').exists())

    def test_accepts_reviewable_immutable_branch_inputs(self):
        driver.validate_inputs(WPE, '7', REVISION, 'refs/heads/codex/install-prep')

    def test_rejects_aliases_other_repositories_and_shell_fragments(self):
        for value in ('code.sandbox.example/qos/tilo-wpe-buildroot:latest',
                      WPE.replace('tilo-wpe-buildroot', 'tilo'), WPE + '; true',
                      WPE.replace('code.sandbox.example', 'example.invalid')):
            with self.subTest(value=value), self.assertRaises(ValueError):
                driver.validate_inputs(value, '7', REVISION, 'refs/heads/codex/test')

    def test_never_runs_main_or_implicitly_allocates_a_generation(self):
        for value in ('', '0', '-1', '07', '1;true', '1000000000000'):
            with self.subTest(generation=value), self.assertRaises(ValueError):
                driver.validate_inputs(WPE, value, REVISION, 'refs/heads/codex/test')
        for branch in ('refs/heads/main', 'refs/tags/test', 'main'):
            with self.subTest(branch=branch), self.assertRaises(ValueError):
                driver.validate_inputs(WPE, '7', REVISION, branch)

    def test_host_environment_cannot_remove_features_or_supply_production_keys(self):
        inherited = {'PATH': '/usr/bin', 'TILO_SKIP_BUILD': '1', 'TILO_WITH_BROWSER': '0',
            'TILO_PREBUILT_APPS': '1', 'TILO_BUILD_ARGS': '--target builder',
            'COSIGN_PRIVATE_KEY': 'secret', 'DOCKER_HOST': 'tcp://remote:2375',
            'CONTAINER_HOST': 'ssh://remote', 'TILO_PUBLIC_TRUST_MODE': 'production'}
        with patch.dict(os.environ, inherited, clear=True):
            env = driver.build_environment(WPE, '7', REVISION, 'c' * 64, '/private/signer')
        self.assertEqual(env['TILO_WITH_BROWSER'], '1')
        self.assertEqual(env['TILO_SKIP_BUILD'], '0')
        self.assertEqual(env['TILO_PUBLIC_TRUST_MODE'], 'development')
        self.assertEqual(env['WPE_BUILDROOT_REF'], WPE)
        self.assertEqual(env['TILO_GENERATION'], '7')
        self.assertNotIn('COSIGN_PRIVATE_KEY', env)
        self.assertNotIn('CONTAINER_HOST', env)
        self.assertNotIn('DOCKER_HOST', env)
        self.assertEqual(env['TILO_BUILD_ARGS'],
            '--build-arg TILO_PREBUILT_APPS=0 --build-arg TILO_PREBUILT_DESKTOP=0 --build-arg TILO_CARGO_BUILD_JOBS=2')
        self.assertTrue(env['TILO_TARGET_IMGREF'].endswith('-x86_64'))


if __name__ == '__main__':
    unittest.main()

#!/usr/bin/env python3
"""Input and environment boundaries for the provisional full OS compilation job."""
import importlib.util
import hashlib
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
WPE = 'code.podesta.ai/qos/tilo-wpe-buildroot@sha256:' + 'a' * 64
REVISION = 'b' * 40
CACHE_ENV = {'SCCACHE_ENDPOINT': 'https://s3.eu-central-1.amazonaws.com',
             'SCCACHE_BUCKET': 'podesta-agent-sccache-125650147312-eu-central-1',
             'SCCACHE_REGION': 'eu-central-1', 'SCCACHE_S3_USE_SSL': 'true',
             'SCCACHE_S3_KEY_PREFIX': 'forgejo-runners/v1'}
CACHE = driver.cache_configuration(CACHE_ENV)


class CompileBoundaries(unittest.TestCase):
    def test_arm_requires_explicit_kernel_and_never_uses_x64_default(self):
        kernel = 'code.podesta.ai/qos/tilo-kernel@sha256:' + 'c' * 64
        rpms = 'code.podesta.ai/qos/tilo-rpms@sha256:' + 'd' * 64
        defaults = {'BASE_REF': WPE, 'RPMS_REF': WPE, 'KERNEL_RPMS_REF': WPE}
        with patch.object(driver, 'pinned_inputs', return_value=defaults.copy()) as pins:
            with self.assertRaisesRegex(ValueError, 'explicit immutable kernel'):
                driver.selected_pins('aarch64')
            pins.assert_not_called()
            selected = driver.selected_pins('aarch64', kernel, rpms)
            self.assertEqual(selected['KERNEL_RPMS_REF'], kernel)
            self.assertEqual(selected['RPMS_REF'], rpms)
        driver.validate_inputs(WPE, '7', REVISION, 'refs/heads/codex/test',
                               architecture='aarch64', kernel_candidate=kernel)
        for bad in ('latest', kernel.replace('@sha256:', ':sha256-'), kernel + ';false'):
            with self.assertRaises(ValueError):
                driver.validate_inputs(WPE, '7', REVISION, 'refs/heads/codex/test',
                                       architecture='aarch64', kernel_candidate=bad)
        env = driver.build_environment(WPE, '7', REVISION, 'c' * 64, '/signer', CACHE,
                                       'aarch64', selected)
        self.assertTrue(env['TILO_TARGET_IMGREF'].endswith('-aarch64'))
        self.assertEqual(env['KERNEL_RPMS_REF'], kernel)
        self.assertEqual(env['RPMS_REF'], rpms)
        self.assertEqual(env['TILO_HARDWARE_PROFILE'], 'generic')
        self.assertEqual(driver.LOADER_NAMES['aarch64'], 'systemd-bootaa64.efi')

    def test_native_config_checks_reject_wrong_architecture_and_os(self):
        for architecture, config, accepted in (
            ('aarch64', {'architecture': 'arm64', 'os': 'linux'}, True),
            ('x86_64', {'architecture': 'amd64', 'os': 'linux'}, True),
            ('aarch64', {'architecture': 'amd64', 'os': 'linux'}, False),
            ('aarch64', {'architecture': 'arm64', 'os': 'windows'}, False)):
            with self.subTest(config=config), patch.object(driver, 'run', return_value=json.dumps(config)) as run:
                if accepted:
                    self.assertEqual(driver.verify_native_image(WPE, architecture)['architecture'], config['architecture'])
                else:
                    with self.assertRaises(ValueError):
                        driver.verify_native_image(WPE, architecture)
                command = run.call_args.args
                self.assertIn('--config', command)
                self.assertEqual(command[command.index('--override-arch') + 1], driver.ARCHITECTURES[architecture])

    def test_cross_architecture_host_rejected_before_any_work(self):
        with patch.object(driver.os, 'getuid', return_value=0, create=True), \
             patch.object(driver.platform, 'machine', return_value='x86_64'), \
             patch.object(driver, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'matching native'):
                driver.main(['--architecture', 'aarch64', '--revision', REVISION,
                             '--branch', 'refs/heads/codex/test'])
            run.assert_not_called()

    def test_full_build_requires_completed_wpe(self):
        with self.assertRaisesRegex(ValueError, 'requires a completed WPE'):
            driver.completed_wpe_receipt(None, WPE)

    @unittest.skipUnless(hasattr(os, 'O_NOFOLLOW'), 'native Linux receipt reader')
    def test_completed_wpe_preflight_binds_buildroot_architecture_and_epoch(self):
        inputs = {'architecture': 'x86_64', 'epoch': '0', 'wpe_buildroot': WPE}
        receipt = {'schema': 1, 'purpose': 'tilo-development-wpe-completed-stage',
            'production_signed': False, 'inputs': inputs,
            'reference': 'code.podesta.ai/qos/tilo-development-wpe-stage-x86_64@sha256:' + 'a' * 64,
            'config_digest': 'sha256:' + 'b' * 64,
            'build': {'jobs': 6, 'revision': REVISION, 'run_id': '1', 'attempt': '1',
                      'source_completed_stage_id': 'sha256:' + 'c' * 64}}
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'receipt.json'
            for field, value in [(None, None), ('epoch', '1'), ('architecture', 'aarch64'),
                                  ('wpe_buildroot', WPE.replace('a' * 64, 'c' * 64))]:
                with self.subTest(field=field):
                    selected = inputs if field is None else inputs | {field: value}
                    receipt['inputs'] = selected
                    receipt['dependency_key'] = hashlib.sha256(json.dumps(selected,
                        sort_keys=True, separators=(',', ':')).encode()).hexdigest()
                    path.write_text(json.dumps(receipt))
                    if field is None:
                        self.assertEqual(driver.completed_wpe_receipt(path, WPE), receipt)
                    else:
                        with self.assertRaises(ValueError):
                            driver.completed_wpe_receipt(path, WPE)
            receipt['inputs'] = inputs | {'architecture': 'aarch64'}
            receipt['reference'] = receipt['reference'].replace('stage-x86_64@', 'stage-aarch64@')
            receipt['dependency_key'] = hashlib.sha256(json.dumps(receipt['inputs'],
                sort_keys=True, separators=(',', ':')).encode()).hexdigest()
            path.write_text(json.dumps(receipt))
            self.assertEqual(driver.completed_wpe_receipt(path, WPE, 'aarch64'), receipt)
            with self.assertRaises(ValueError):
                driver.completed_wpe_receipt(path, WPE)

    def test_cache_only_and_default_exit_before_key_generation_or_os_build(self):
        for mode in ([], ['--mode', 'cache-only']):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                def git_only(*command, **kwargs):
                    if command == ('git', 'rev-parse', 'HEAD'):
                        return REVISION
                    if command == ('git', 'status', '--porcelain', '--untracked-files=all'):
                        return ''
                    self.fail(f'cache-only reached unexpected command: {command}')
                with patch.object(driver, 'PRIVATE', directory / 'private'), \
                     patch.object(driver, 'EVIDENCE', directory / 'evidence'), \
                     patch.object(driver.os, 'getuid', return_value=0, create=True), \
                     patch.object(driver.os, 'umask'), \
                     patch.object(driver.platform, 'machine', return_value='x86_64'), \
                     patch.dict(os.environ, CACHE_ENV, clear=True), \
                     patch.object(driver, 'run', side_effect=git_only), \
                     patch.object(driver, 'verify_remote_cache') as probe, \
                     patch.object(driver, 'pinned_inputs') as pins, \
                     patch.object(driver, 'compile_with_capacity_watchdog') as compile_os:
                    driver.main([*mode, '--revision', REVISION, '--branch', 'refs/heads/codex/test'])
                    probe.assert_called_once_with(CACHE)
                    pins.assert_not_called()
                    compile_os.assert_not_called()
                receipt = json.loads((directory / 'evidence/result.json').read_text())
                self.assertEqual(receipt['status'], 'remote-cache-probe-complete')
                for field in ('os_compiled', 'installable', 'signing_identities_created', 'disk_created', 'published'):
                    self.assertFalse(receipt[field])
                self.assertEqual(list((directory / 'private').iterdir()), [])

    def test_mode_requires_explicit_full_build_and_rejects_unknown_values(self):
        driver.validate_inputs('', '', REVISION, 'refs/heads/codex/test', 'cache-only')
        for mode in ('', 'full', 'full-build', 'cache-only; true'):
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                driver.validate_inputs('', '', REVISION, 'refs/heads/codex/test', mode)
        driver.validate_inputs(WPE, '7', REVISION, 'refs/heads/codex/test', 'full-build')

    def test_cache_rejects_missing_redirected_or_shell_configuration(self):
        for field, value in (('SCCACHE_ENDPOINT', 'https://attacker.invalid'),
                             ('SCCACHE_BUCKET', 'wrong-bucket'), ('SCCACHE_REGION', ''),
                             ('SCCACHE_S3_KEY_PREFIX', 'prefix --secret=other'),
                             ('SCCACHE_S3_KEY_PREFIX', '../escape')):
            with self.subTest(field=field), self.assertRaises(ValueError):
                driver.cache_configuration({**CACHE_ENV, field: value})

    def test_credential_staging_excludes_values_from_build_arguments(self):
        credentials = {'AWS_ACCESS_KEY_ID': 'test-access-key', 'AWS_SECRET_ACCESS_KEY': 'test-secret-key'}
        with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, {**CACHE_ENV, **credentials}, clear=True):
            directory = Path(temporary)
            driver.stage_cache_credentials(directory)
            for variable, filename in driver.CACHE_CREDENTIALS.items():
                self.assertEqual((directory / filename).read_text(), credentials[variable])
            env = driver.build_environment(WPE, '7', REVISION, 'c' * 64, '/signer', CACHE)
            self.assertIn('--build-arg TILO_REQUIRE_COMPILER_CACHE=1', env['TILO_BUILD_ARGS'])
            self.assertEqual(env['TILO_WPE_STAGE_RECEIPT'], str(driver.EVIDENCE / 'wpe-stage.json'))
            self.assertEqual(env['TILO_WPE_SOURCE_DATE_EPOCH'], '0')
            for variable, value in credentials.items():
                self.assertNotIn(variable, env)
                self.assertNotIn(value, repr(env))
            for filename in driver.CACHE_CREDENTIALS.values():
                self.assertIn(f'--secret id={filename},src=/run/tilo-cache-secrets/{filename}', env['TILO_BUILD_ARGS'])

    def test_temporary_or_missing_credentials_fail_before_staging(self):
        for extra in ({}, {'AWS_SESSION_TOKEN': 'temporary'}):
            with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, {**CACHE_ENV, **extra}, clear=True):
                with self.assertRaises(ValueError):
                    driver.stage_cache_credentials(temporary)
                self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_remote_counter_receipt_rejects_local_cache_and_errors(self):
        for payload in ({'cache_location': 'Local disk: /tmp', 'stats': {}},
                        {'cache_location': 's3, name: podesta-agent-sccache-125650147312-eu-central-1, prefix: /' + CACHE['SCCACHE_S3_KEY_PREFIX'] + '/',
                         'stats': {'cache_write_errors': 1}}):
            with self.assertRaises(ValueError):
                driver.cache_counters(json.dumps(payload), CACHE['SCCACHE_S3_KEY_PREFIX'])

    def test_probe_restarts_with_empty_local_caches_and_checks_env_dependency(self):
        for stale_epoch in (False, True):
            with self.subTest(stale_epoch=stale_epoch), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                calls = []
                def command(*args, env=None, **kwargs):
                    calls.append((args, dict(env)))
                    if len(args) > 1 and args[1] == 'rustc':
                        self.assertIn('--emit=link,dep-info', args)
                        self.assertNotIn('-o', args)
                        (Path(args[args.index('--out-dir') + 1]) / 'libtilo_cache_probe.rlib').write_bytes(
                            b'1' if stale_epoch else env['TILO_CACHE_PROBE_VALUE'].encode())
                    if '--stats-format=json' in args:
                        return json.dumps({'cache_location': 's3, name: podesta-agent-sccache-125650147312-eu-central-1, prefix: /' + CACHE['SCCACHE_S3_KEY_PREFIX'] + '/',
                                           'stats': {'cache_hits': {'counts': {'Rust': 1}}}})
                    return ''
                with patch.object(driver, 'PRIVATE', directory), patch.object(driver, 'EVIDENCE', directory), \
                     patch.object(driver, 'cache_credentials', return_value={'AWS_SECRET_ACCESS_KEY': 'private-sentinel'}), \
                     patch.object(driver, 'run', side_effect=command):
                    if stale_epoch:
                        with self.assertRaisesRegex(ValueError, 'cache probe failed'):
                            driver.verify_remote_cache(CACHE)
                    else:
                        driver.verify_remote_cache(CACHE)
                receipt_text = (directory / 'cache-configuration.json').read_text()
                self.assertNotIn('private-sentinel', receipt_text)
                receipt = json.loads(receipt_text)
                self.assertEqual(receipt['status'], 'failed-before-full-compilation' if stale_epoch else 'verified-remote-compile-and-hit')
                compilations = [env for args, env in calls if len(args) > 1 and args[1] == 'rustc']
                self.assertEqual(len({env['SCCACHE_DIR'] for env in compilations}), 4)
                self.assertGreaterEqual(sum('--stop-server' in args for args, env in calls), 4)
                self.assertEqual([env['SOURCE_DATE_EPOCH'] for env in compilations], ['1', '1', '2', '1'])
                self.assertEqual([env['TILO_CACHE_PROBE_VALUE'] for env in compilations], ['1', '1', '1', '2'])

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
        for value in ('code.podesta.ai/qos/tilo-wpe-buildroot:latest',
                      WPE.replace('tilo-wpe-buildroot', 'tilo'), WPE + '; true',
                      WPE.replace('code.podesta.ai', 'example.invalid')):
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
            env = driver.build_environment(WPE, '7', REVISION, 'c' * 64, '/private/signer', CACHE)
        self.assertEqual(env['TILO_WITH_BROWSER'], '1')
        self.assertEqual(env['TILO_SKIP_BUILD'], '0')
        self.assertEqual(env['TILO_RAW_ROOTFS_ONLY'], '1')
        self.assertTrue(env['TILO_WPE_STAGE_RECEIPT'].endswith('wpe-stage.json'))
        self.assertNotIn('TILO_WPE_BUILD_CACHE_REF', env)
        self.assertEqual(env['TILO_PUBLIC_TRUST_MODE'], 'development')
        self.assertEqual(env['WPE_BUILDROOT_REF'], WPE)
        self.assertEqual(env['TILO_GENERATION'], '7')
        self.assertNotIn('COSIGN_PRIVATE_KEY', env)
        self.assertNotIn('CONTAINER_HOST', env)
        self.assertNotIn('DOCKER_HOST', env)
        self.assertTrue(env['TILO_BUILD_ARGS'].startswith(
            '--build-arg TILO_PREBUILT_APPS=0 --build-arg TILO_PREBUILT_DESKTOP=0 --build-arg TILO_CARGO_BUILD_JOBS=2')
        )
        self.assertTrue(env['TILO_TARGET_IMGREF'].endswith('-x86_64'))


@unittest.skipUnless(hasattr(os, 'getuid') and os.getuid() == 0, 'secret ownership tests require root')
class CredentialFiles(unittest.TestCase):
    def test_only_private_regular_root_owned_files_are_accepted(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for filename in driver.CACHE_CREDENTIALS.values():
                path = directory / filename
                path.write_text('private-test-value')
                path.chmod(0o600)
            with patch.object(driver, 'CACHE_SECRETS', directory):
                self.assertEqual(set(driver.cache_credentials()), set(driver.CACHE_CREDENTIALS))
                selected = directory / 'aws_access_key_id'
                selected.chmod(0o644)
                with self.assertRaises(ValueError):
                    driver.cache_credentials()
                selected.unlink()
                selected.symlink_to(directory / 'aws_secret_access_key')
                with self.assertRaises(OSError):
                    driver.cache_credentials()
                selected.unlink()
                os.mkfifo(selected)
                with self.assertRaises(ValueError):
                    driver.cache_credentials()


if __name__ == '__main__':
    unittest.main()

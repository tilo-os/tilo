#!/usr/bin/env python3
"""Exercise the real workflow plan and actual signed development policy writer."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import textwrap

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT/'tools/policy'))
from check_image_builder_inputs import context_copies, logical_lines
WORKFLOW = (ROOT/'.forgejo/workflows/build-image.yml').read_text()
MATRIX_STEP = WORKFLOW.split('        id: matrix\n', 1)[1].split('\n      - name:', 1)[0]
PLAN = textwrap.dedent(MATRIX_STEP.split('        run: |\n', 1)[1])
spec = importlib.util.spec_from_file_location('board_policy', ROOT/'os/dev/signed-development-board-policy.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)
spec = importlib.util.spec_from_file_location('hardware_profile', ROOT/'os/dev/hardware_profile.py')
hardware = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hardware)


class SignedBoard(unittest.TestCase):
    def run_plan(self, **extra):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp)/'outputs'
            env = {**os.environ, 'GITHUB_OUTPUT': str(output), 'GITHUB_EVENT_NAME': 'workflow_dispatch',
                   'GITHUB_REF': 'refs/heads/main', 'SIGNED_DEVELOPMENT_BOARD': 'true',
                   'REQUESTED_RELEASE': 'false', 'HARDWARE_PROFILE': 'generic',
                   'CONTINUOUS_RELEASE': '0', 'PUBLICATION_CONFIGURED': 'true', **extra}
            result = subprocess.run(['bash', '-c', PLAN], env=env, capture_output=True, text=True)
            values = dict(line.split('=', 1) for line in output.read_text().splitlines()) if output.exists() else {}
            return result, values

    def test_main_signed_dispatch_is_one_arm_dev_lane(self):
        r, p = self.run_plan(LENOVO_FIRMWARE_REF='configured')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(p['publish'], 'false')
        self.assertEqual(p['mode'], 'dev')
        self.assertEqual(json.loads(p['boards']), ['orangepi-3b-v2-1'])
        self.assertEqual(json.loads(p['build-matrix'])['include'], [{'arch': 'aarch64', 'runner': 'runner-release-arm64'}])

    def test_signed_dispatch_rejects_unprotected_or_production_requests(self):
        for changed in [{'GITHUB_REF': 'refs/heads/topic'}, {'GITHUB_EVENT_NAME': 'workflow_call'},
                        {'REQUESTED_RELEASE': 'true'}, {'HARDWARE_PROFILE': 'orangepi-3b-v2-1'},
                        {'SIGNED_DEVELOPMENT_BOARD': 'invalid'}]:
            with self.subTest(changed=changed):
                r, _ = self.run_plan(**changed)
                self.assertNotEqual(r.returncode, 0)

    def test_existing_called_dev_build_stays_unsigned(self):
        r, p = self.run_plan(SIGNED_DEVELOPMENT_BOARD='false', GITHUB_EVENT_NAME='workflow_call', DEV_ARCHES='["aarch64"]')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(p['signed-development-board'], 'false')
        self.assertEqual(p['publish'], 'false')
        self.assertEqual(json.loads(p['build-matrix'])['include'][0]['runner'], 'runner-large-arm64')

    def test_production_board_remains_release(self):
        r, p = self.run_plan(SIGNED_DEVELOPMENT_BOARD='false', REQUESTED_RELEASE='true', HARDWARE_PROFILE='orangepi-3b-v2-1')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(p['publish'], 'true')
        self.assertEqual(p['mode'], 'release')
        self.assertEqual(json.loads(p['boards']), [])

    def test_signed_dev_uses_own_source_app_boundary_and_falls_back_to_compile(self):
        step = WORKFLOW.split('      - name: Materialize the pinned app collection\n', 1)[1].split('\n      - name:', 1)[0]
        self.assertIn("DEV_BUILD: ${{ github.event_name == 'workflow_call' || needs.plan.outputs.signed-development-board == 'true' }}", step)
        actual = textwrap.dedent(step.split('        run: |\n', 1)[1])
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            fake = root/'git'
            fake.write_text('#!/bin/sh\ncase "$1" in fetch) exit 0 ;; diff) exit 1 ;; *) exit 90 ;; esac\n')
            fake.chmod(0o755)
            output = root/'env'
            output.write_text('TILO_PREBUILT_APPS=0\n')
            result = subprocess.run(['bash', '-c', actual], cwd=ROOT, capture_output=True, text=True,
                env={**os.environ, 'PATH': str(root)+os.pathsep+os.environ['PATH'],
                     'DEV_BUILD': 'true', 'COLLECTION_SOURCE': 'a'*40, 'GITHUB_SHA': 'b'*40, 'TILO_IMAGE_SOURCE': 'b'*40,
                     'GITHUB_ENV': str(output)})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('app inputs changed', result.stdout)
            self.assertEqual(output.read_text(), 'TILO_PREBUILT_APPS=0\n')

    def test_board_sparse_context_contains_every_actual_copy_input(self):
        recipe = ROOT/'os/Containerfile.board'
        patterns = [line.strip() for line in recipe.with_name(recipe.name+'.dockerignore').read_text().splitlines()
                    if line.strip() and not line.lstrip().startswith('#')]
        # This deliberately small context uses only the blanket exclusion and
        # explicit file/directory exceptions, not arbitrary pattern semantics.
        self.assertEqual([p for p in patterns if not p.startswith('!')], ['*'])
        allowed = [p[1:].rstrip('/') for p in patterns if p.startswith('!')]
        self.assertTrue(all(not any(c in p for c in '*?[') for p in allowed))
        missing = []
        for copy_input in context_copies(logical_lines(recipe.read_text())):
            for source in copy_input.sources:
                self.assertTrue((ROOT/source).exists(), 'Board COPY source is missing: '+source)
                if not any(source == p or source.startswith(p+'/') for p in allowed):
                    missing.append(source)
        self.assertEqual(missing, [], 'Board COPY sources excluded by its sparse Docker context')

    def test_board_input_regression_gates_native_build(self):
        preflight = WORKFLOW.split('  signing-preflight:\n', 1)[1].split('\n  ensure-rpms:', 1)[0]
        step = preflight.split('      - name: Validate signed board inputs before native build\n', 1)[1].split('\n      - name:', 1)[0]
        self.assertIn("if: needs.plan.outputs.signed-development-board == 'true'", step)
        self.assertIn('run: python3 os/tests/test_signed_development_board.py', step)
        native = WORKFLOW.split('  build-and-push:\n', 1)[1].split('    steps:', 1)[0]
        self.assertIn('signing-preflight', native)

    def test_sealing_tools_do_not_inherit_device_admission_policy(self):
        board = (ROOT/'os/Containerfile.board').read_text()
        production = (ROOT/'os/Containerfile').read_text()
        self.assertIn('FROM ${BASE_REF} AS uki-tools', production)
        self.assertIn('FROM ${BASE_REF} AS uki-tools', board)
        self.assertIn('FROM uki-tools AS cfs-digest', board)
        self.assertIn('FROM ${BASE_IMAGE} AS board-root', board)
        self.assertIn('FROM split AS final', board)
        # Production/installed-device policy remains strict; only the
        # throwaway construction environment reads its own unsigned layers.
        original = json.loads((ROOT/'os/etc/containers/policy.json').read_text())
        self.assertEqual(original['transports']['docker'][policy.REPOSITORY], [{'type': 'reject'}])

    def test_actual_board_build_passes_pinned_tools_to_all_three_phases(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            log = root/'calls'
            auth = root/'empty-test-auth.json'
            auth.write_text('{}')
            fake = root/'docker'
            fake.write_text('#!'+sys.executable+'\n'+textwrap.dedent('''
                import json, os, pathlib, sys
                args = sys.argv[1:]
                with open(os.environ['CALLS'], 'a') as out:
                    out.write(json.dumps(args)+'\\n')
                if '--metadata-file' in args:
                    pathlib.Path(args[args.index('--metadata-file')+1]).write_text(json.dumps({'containerimage.digest':'sha256:'+'a'*64}))
                if '--target' in args and args[args.index('--target')+1] == 'cfs-digest-out':
                    dest = next(a.split('dest=',1)[1] for a in args if a.startswith('type=local,dest='))
                    pathlib.Path(dest).mkdir(parents=True)
                    (pathlib.Path(dest)/'digest').write_text('b'*128)
                '''))
            fake.chmod(0o755)
            result = subprocess.run(['bash', str(ROOT/'os/dev/build-board-image.sh'),
                'orangepi-3b-v2-1', 'registry.tilo.so/tilo/tilo-ci@sha256:'+'c'*64,
                'registry.tilo.so/tilo/tilo-vm:main-fixture-orangepi-3b-v2-1-signed'],
                cwd=ROOT, capture_output=True, text=True,
                env={**os.environ, 'PATH':str(root)+os.pathsep+os.environ['PATH'],
                     'BUILDER_NAME':'fixture', 'RPMS_REF':'fixture', 'BOOTC_HELPER_IMAGE':'fixture',
                     'TILO_SIGNED_DEVELOPMENT_BOARD':'1', 'REGISTRY_AUTH_FILE':str(auth),
                     'BASE_REF':'ignored-environment-override', 'CALLS':str(log)})
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(len(calls), 3)
            pinned = next(line.split('=',1)[1] for line in (ROOT/'os/base-snapshot.env').read_text().splitlines()
                          if line.startswith('BASE_REF='))
            for call in calls:
                self.assertIn('BASE_REF='+pinned, call)
                self.assertNotIn('BASE_REF=ignored-environment-override', call)

    def test_split_directory_preparation_is_repeatable_but_rejects_stale_kernel(self):
        recipe = (ROOT/'os/Containerfile.board').read_text()
        actual = recipe.split('FROM board-root AS split\nRUN ', 1)[1].split('&& bootc container split-kernel-and-rootfs', 1)[0]
        actual = actual.strip().removesuffix('\\').rstrip()
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp)/'kernel'
            shell = actual.replace('/kernel', str(output))
            for _ in range(2):
                result = subprocess.run(['bash', '-c', shell], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(list(output.iterdir()), [])
            stale = output/'old-kernel'
            stale.write_bytes(b'preserve unexpected input')
            result = subprocess.run(['bash', '-c', shell], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(stale.read_bytes(), b'preserve unexpected input')

    def make_root(self, root):
        for directory in ['usr/lib/tilo', 'usr/share/tilo', 'etc/containers']:
            (root/directory).mkdir(parents=True)
        (root/'usr/lib/tilo/image-flavour').write_text('dev\n')
        (root/'usr/share/tilo/cosign.pub').write_bytes((ROOT/'keys/cosign.pub').read_bytes())
        original = json.loads((ROOT/'os/etc/containers/policy.json').read_text())
        original['transports']['docker'][policy.REPOSITORY] = [{'type': 'insecureAcceptAnything'}]
        (root/'etc/containers/policy.json').write_text(json.dumps(original))
        return original

    def test_signed_board_restores_sealed_kernel_before_board_initramfs(self):
        recipe = (ROOT/'os/Containerfile.board').read_text()
        maintenance_case = recipe.split('case "$TILO_SIGNED_DEVELOPMENT_BOARD" in', 1)[1].split('esac', 1)[0]
        actual = maintenance_case.split('1)', 1)[1].split('&& dnf install', 1)[0].strip()
        # Run the actual sealed-base restoration commands against a private
        # fixture root. No fake restorer hides the inherited /kernel failure.
        for layout in ['sealed', 'wrong-kernel', 'empty-kernel', 'extra-kernel']:
            with self.subTest(layout=layout), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                modules = root/'usr/lib/modules/7.2.8'
                modules.mkdir(parents=True)
                boot = root/'boot/EFI/Linux'
                boot.mkdir(parents=True)
                inherited = boot/'7.2.8.efi'
                inherited.write_bytes(b'old generic UKI fixture')
                split = root/('kernel/7.2.7' if layout == 'wrong-kernel' else 'kernel/7.2.8')
                split.mkdir(parents=True)
                original = b'' if layout == 'empty-kernel' else b'exact production kernel'
                (split/'vmlinuz').write_bytes(original)
                if layout == 'extra-kernel':
                    (root/'kernel/7.2.9').mkdir()
                shell = actual.rstrip().removesuffix('&&').rstrip().removesuffix('\\').rstrip()
                for path in ['/usr/lib/modules', '/kernel', '/boot/EFI/Linux']:
                    shell = shell.replace(path, str(root)+path)
                result = subprocess.run(['bash', '-c', shell], capture_output=True, text=True,
                                        env={**os.environ, 'TILO_HARDWARE_PROFILE': 'orangepi-3b-v2-1'})
                if layout != 'sealed':
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual((split/'vmlinuz').read_bytes(), original)
                    self.assertTrue(inherited.exists())
                    continue
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((modules/'vmlinuz').read_bytes(), original)
                self.assertEqual(list(boot.iterdir()), [])
                self.assertFalse((root/'kernel').exists())
                (root/'kernel').mkdir()
                self.assertEqual(list((root/'kernel').iterdir()), [])

    def test_maintenance_flavour_changes_only_signed_scope_and_marker(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_root(root)
            (root/'usr/lib/tilo/image-flavour').unlink()
            original = json.loads((ROOT/'os/etc/containers/policy.json').read_text())
            path = root/'etc/containers/policy.json'
            path.write_text(json.dumps(original))
            key = (root/'usr/share/tilo/cosign.pub').read_bytes()
            policy.maintenance(root)
            expected = copy.deepcopy(original)
            expected['transports']['docker'][policy.REPOSITORY] = policy.REQUIREMENT
            self.assertEqual(json.loads(path.read_text()), expected)
            self.assertEqual((root/'usr/lib/tilo/image-flavour').read_text(), 'dev\n')
            self.assertEqual((root/'usr/share/tilo/cosign.pub').read_bytes(), key)

    def test_maintenance_rejects_vm_or_unsigned_base_before_writes(self):
        for invalid in ['vm-flavour', 'vm-route', 'unsigned-policy', 'key-link']:
            with self.subTest(invalid=invalid), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                self.make_root(root)
                flavour = root/'usr/lib/tilo/image-flavour'
                flavour.unlink()
                path = root/'etc/containers/policy.json'
                path.write_bytes((ROOT/'os/etc/containers/policy.json').read_bytes())
                if invalid == 'vm-flavour':
                    flavour.write_text('dev\n')
                elif invalid == 'vm-route':
                    item = root/policy.VM_ONLY_PATHS[0]
                    item.parent.mkdir(parents=True, exist_ok=True)
                    item.write_bytes(b'VM route fixture')
                elif invalid == 'unsigned-policy':
                    original = json.loads(path.read_text())
                    original['transports']['docker'][policy.REPOSITORY] = [{'type': 'insecureAcceptAnything'}]
                    path.write_text(json.dumps(original))
                else:
                    key = root/'usr/share/tilo/cosign.pub'
                    key.rename(key.with_name('saved.pub'))
                    key.symlink_to('saved.pub')
                before = path.read_bytes()
                with self.assertRaises(ValueError):
                    policy.maintenance(root)
                self.assertEqual(path.read_bytes(), before)
                self.assertEqual(flavour.exists(), invalid == 'vm-flavour')

    def test_signed_publish_uses_production_payload_without_vm_publication(self):
        compile_step = WORKFLOW.split('      - name: Start the dev binaries compile\n', 1)[1].split('\n      - name:', 1)[0]
        self.assertIn("needs.plan.outputs.signed-development-board != 'true'", compile_step)
        step = WORKFLOW.split('        id: dev-publish\n', 1)[1].split('\n      - name:', 1)[0]
        actual = textwrap.dedent(step.split('        run: |\n', 1)[1])
        # Exercise the actual workflow shell, mocking only external build/sign tools.
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            tools = root/'os/dev'
            tools.mkdir(parents=True)
            log = root/'calls'
            for name, body in {
                'build-board-image.sh': 'printf "%s\n" "$*" >> "$CALLS"; echo orangepi-3b-v2-1: >&2; echo sha256:fixture',
                'sign-development-board.sh': 'printf "signed %s\n" "$*" >> "$CALLS"',
                'publish-dev-disk.sh': 'exit 91', 'move-tag-forward.sh': 'exit 92',
            }.items():
                file = tools/name
                file.write_text('#!/bin/sh\n'+body+'\n')
                file.chmod(0o755)
            fake = root/'docker'
            fake.write_text('#!/bin/sh\nexit 93\n')
            fake.chmod(0o755)
            result = subprocess.run(['bash', '-c', actual], cwd=root, capture_output=True, text=True,
                env={**os.environ, 'PATH': str(root)+os.pathsep+os.environ['PATH'],
                     'TILO_SIGNED_DEVELOPMENT_BOARD': '1', 'DEV_BASE_DIGEST': 'sha256:production',
                     'SOURCE_DATE_ISO': '2026-10-07T00:00:00Z', 'DEV_SERIES': 'main-fixture',
                     'BOARDS': '["orangepi-3b-v2-1"]', 'RUNNER_TEMP': str(root),
                     'GITHUB_SHA': 'a'*40, 'TILO_IMAGE_SOURCE': 'a'*40, 'GITHUB_REF': 'refs/heads/main', 'CALLS': str(log),
                     'TILO_DEV_COMPILE_LOG': ''})
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = log.read_text().splitlines()
            self.assertEqual(calls[0], 'orangepi-3b-v2-1 registry.tilo.so/tilo/tilo-ci@sha256:production registry.tilo.so/tilo/tilo-vm:main-fixture-orangepi-3b-v2-1-signed')
            self.assertEqual(calls[1], 'signed registry.tilo.so/tilo/tilo-vm@sha256:fixture orangepi-3b-v2-1')

    def test_policy_only_strengthens_existing_dev_scope_with_same_key(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            original = self.make_root(root)
            key = (root/'usr/share/tilo/cosign.pub').read_bytes()
            policy.strengthen(root)
            actual = json.loads((root/'etc/containers/policy.json').read_text())
            expected = copy.deepcopy(original)
            expected['transports']['docker'][policy.REPOSITORY] = [{'type': 'sigstoreSigned', 'keyPath': '/usr/share/tilo/cosign.pub', 'signedIdentity': {'type': 'matchRepository'}}]
            self.assertEqual(actual, expected)
            self.assertEqual((root/'usr/share/tilo/cosign.pub').read_bytes(), key)

    def test_signed_board_excludes_new_vm_root_endpoint_and_input_routes(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_root(root)
            for name in policy.VM_ONLY_PATHS:
                p = root/name
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_bytes(b'generic VM image fixture')
            retained = root/'etc/ssh/sshd_config'
            retained.parent.mkdir(parents=True)
            retained.write_bytes(b'existing SSH configuration fixture')
            policy.strengthen(root)
            self.assertTrue(all(not (root/name).exists() for name in policy.VM_ONLY_PATHS))
            self.assertEqual(retained.read_bytes(), b'existing SSH configuration fixture')
            self.assertEqual((root/'usr/share/tilo/cosign.pub').read_bytes(), (ROOT/'keys/cosign.pub').read_bytes())

    def test_board_parser_accepts_only_exact_signed_policy_and_immutable_key(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.make_root(root)
            policy.strengthen(root)
            original_lstat = Path.lstat

            def image_lstat(path, *args, **kwargs):
                # Model OCI root ownership only; actual modes, symlinks and file
                # contents remain the on-disk fixture used by the real parser.
                result = list(original_lstat(path, *args, **kwargs))
                result[4] = 0
                return os.stat_result(result)

            ref = policy.REPOSITORY + ':main-fixture-orangepi-3b-v2-1-signed'
            with patch.object(Path, 'lstat', image_lstat):
                self.assertEqual(hardware.validate_target_imgref(ref, root=root), ref)
                key = root/'usr/share/tilo/cosign.pub'
                key.chmod(0o666)
                with self.assertRaisesRegex(ValueError, 'not group/world writable'):
                    hardware.validate_target_imgref(ref, root=root)
                key.chmod(0o644)
                path = root/'etc/containers/policy.json'
                p = json.loads(path.read_text())
                p['transports']['docker'][policy.REPOSITORY][0]['keyPath'] = '/other/key'
                path.write_text(json.dumps(p))
                with self.assertRaisesRegex(ValueError, 'development repository policy'):
                    hardware.validate_target_imgref(ref, root=root)
                with self.assertRaisesRegex(ValueError, 'signature-required'):
                    hardware.validate_target_imgref(ref)

    def test_policy_rejects_production_or_redirected_inputs_without_write(self):
        for invalid in ['production', 'key-link', 'policy-link', 'already-rejects']:
            with self.subTest(invalid=invalid), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                self.make_root(root)
                path = root/'etc/containers/policy.json'
                if invalid == 'production':
                    (root/'usr/lib/tilo/image-flavour').write_text('production\n')
                elif invalid == 'key-link':
                    key = root/'usr/share/tilo/cosign.pub'
                    key.rename(key.with_name('saved.pub'))
                    key.symlink_to('saved.pub')
                elif invalid == 'policy-link':
                    path.rename(path.with_name('saved.json'))
                    path.symlink_to('saved.json')
                else:
                    p = json.loads(path.read_text())
                    p['transports']['docker'][policy.REPOSITORY] = [{'type': 'reject'}]
                    path.write_text(json.dumps(p))
                before = path.read_bytes()
                with self.assertRaises(ValueError):
                    policy.strengthen(root)
                self.assertEqual(path.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()

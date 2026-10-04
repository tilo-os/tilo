import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / 'dist/macos/collection.py'
ROOT = SCRIPT.parents[2]


class CommandTests(unittest.TestCase):
    def test_plan_is_read_only_and_reports_exclusions(self):
        result = subprocess.run([sys.executable, str(SCRIPT), 'plan'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(result.stdout)
        self.assertIn('calculator', [a['name'] for a in plan['apps']])
        self.assertTrue(next(a for a in plan['excluded'] if a['name'] == 'containers')['reason'])
        self.assertEqual(plan['notarized'], False)

    def test_unknown_app_fails(self):
        result = subprocess.run([sys.executable, str(SCRIPT), 'plan', '--apps', 'no-such-app'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('unknown app', result.stderr)



sys.path.insert(0, str(SCRIPT.parent))
import collection as c
from unittest.mock import patch
import plistlib


class SafetyTests(unittest.TestCase):
    def test_collision_case_insensitive(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name, display in [('one', 'Same'), ('two', 'same')]:
                folder = root / 'apps' / name
                folder.mkdir(parents=True)
                (folder / 'Cargo.toml').write_text(f'[package]\nname="{name}"\n')
                (folder / 'tilo-app.toml').write_text(f'[package]\nid="so.tilo.{name}"\nname="{display}"\nversion="1.0.0"\nruntime="r"\n[entrypoint]\nbin="{name}"\n')
            with self.assertRaisesRegex(ValueError, 'collision'):
                c.make_plan(root, '', 'arm64', 'development')

    def test_transaction_failure_retry_and_resume_tamper(self):
        with tempfile.TemporaryDirectory(prefix='collection space ') as temp:
            out = Path(temp)
            with self.assertRaisesRegex(RuntimeError, 'failure'):
                with c.transaction(out, 'built', False, {}) as work:
                    (work / 'unfinished').write_text('x')
                    raise RuntimeError('failure')
            self.assertFalse((out / 'built').exists())
            self.assertEqual(list(out.glob('.built-*')), [])
            with c.transaction(out, 'built', False, {}) as work:
                (work / 'app').write_text('complete')
                c.write_receipt(work, {}, 'built')
            with c.transaction(out, 'built', True, {}) as work:
                self.assertIsNone(work)
            (out / 'built/app').write_text('changed')
            with self.assertRaisesRegex(ValueError, 'changed'):
                with c.transaction(out, 'built', True, {}):
                    pass

    def test_missing_ambiguous_and_wrong_identity(self):
        from collection_native import select_identity
        dev = 'A' * 40 + ' "Apple Development: Test (TEAM123456)"'
        dist = 'B' * 40 + ' "Developer ID Application: Test (TEAM123456)"'
        for identities in ['', dev]:
            with self.assertRaisesRegex(ValueError, 'identity'):
                select_identity(identities, None, 'distribution')
        with self.assertRaisesRegex(ValueError, 'ambiguous'):
            select_identity(dist + '\n' + dist.replace('B' * 40, 'C' * 40), None, 'distribution')
        with self.assertRaisesRegex(ValueError, 'identity'):
            select_identity(dev, 'ad-hoc', 'distribution')
        self.assertEqual(select_identity(dist, 'B' * 40, 'distribution')['team'], 'TEAM123456')
        self.assertEqual(select_identity('', 'ad-hoc', 'development')['sha1'], '-')

    def test_external_symlink_rejected(self):
        from collection_native import inspect_bundle
        with tempfile.TemporaryDirectory() as temp:
            app = Path(temp) / 'Spaces & $(touch BAD).app'
            app.mkdir()
            (app / 'escape').symlink_to('/usr/bin/true')
            with self.assertRaisesRegex(ValueError, 'symlink'):
                inspect_bundle(app, {}, 'arm64')

    def test_subprocess_quoting_and_error_propagation(self):
        from collection_native import run
        weird = 'spaces & $(touch BAD); "quotes"'
        self.assertEqual(run([sys.executable, '-c', 'import sys; print(sys.argv[1])', weird]).strip(), weird)
        with self.assertRaisesRegex(ValueError, 'exit 7'):
            run([sys.executable, '-c', 'raise SystemExit(7)'])

    def test_distribution_offline_rejected_before_tools(self):
        result = subprocess.run([sys.executable, str(SCRIPT), 'sign', '--mode', 'distribution', '--offline'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('secure timestamps', result.stderr)

class StageTests(unittest.TestCase):
    def test_build_tools_unstripped_without_changing_app_release_profile(self):
        import platform
        from types import SimpleNamespace
        from collection_stages import build_environment
        with tempfile.TemporaryDirectory(prefix='build environment ') as temp:
            work = Path(temp)
            guard, cli = work / 'rustc-budget.py', work / 'tilo-app'
            guard.touch()
            cli.touch()
            args = SimpleNamespace(compiler_guard=guard, tilo_app=cli, arch=platform.machine())
            inherited = {'CARGO_PROFILE_RELEASE_STRIP': 'symbols',
                         'CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_STRIP': 'debuginfo'}
            with patch.dict('os.environ', inherited, clear=True), patch('collection_stages.shutil.which', return_value='/tools/cargo'):
                env = build_environment(args, work)
            self.assertEqual(env['CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_STRIP'], 'none')
            self.assertEqual(env['CARGO_PROFILE_RELEASE_STRIP'], 'symbols')

    def test_sign_inside_out_and_no_deep_sign(self):
        from collection_native import sign_bundle
        with tempfile.TemporaryDirectory() as temp:
            policy = Path(temp) / 'App.entitlements'
            policy.write_bytes(plistlib.dumps({}))
            calls = []
            with patch('collection_native.run', side_effect=lambda cmd, **kw: calls.append(cmd)):
                sign_bundle(Path(temp) / 'App with spaces.app', policy, {'sha1': '-'}, True)
            self.assertEqual(len(calls), 2)
            self.assertTrue(str(calls[0][-1]).endswith('MacOS/tilo-runtime-mcp'))
            self.assertTrue(str(calls[1][-1]).endswith('.app'))
            self.assertFalse(any('--deep' in cmd for cmd in calls))

    def test_debug_or_jit_entitlements_rejected(self):
        from collection_native import read_entitlements
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'policy.plist'
            for key in ['com.apple.security.get-task-allow', 'com.apple.security.cs.allow-jit']:
                path.write_bytes(plistlib.dumps({key: True}))
                with self.assertRaisesRegex(ValueError, 'unreviewed'):
                    read_entitlements(path)

    def test_notary_requires_explicit_destination_approval(self):
        from collection_stages import execute
        args = c.parser().parse_args(['notarize', '--mode', 'distribution'])
        with self.assertRaisesRegex(ValueError, 'submit-to-apple'):
            execute(args, {})

    def test_missing_bundle_does_not_leave_partial_stage(self):
        from collection_stages import execute
        with tempfile.TemporaryDirectory() as temp:
            args = c.parser().parse_args(['build', '--reuse-bundles', '--apps', 'calculator', '--out', temp])
            plan = c.make_plan(ROOT, args.apps, args.arch, args.mode)
            with patch('collection_stages.disk_floor'), self.assertRaisesRegex(ValueError, 'missing app'):
                execute(args, plan)
            self.assertFalse((Path(temp) / 'built').exists())

    def test_external_dylib_is_rejected(self):
        from collection_native import load_paths
        with patch('collection_native.run', side_effect=['', '/a:\n\t/opt/homebrew/lib/foo.dylib (compatibility version 1.0.0, current version 1.0.0)']):
            with self.assertRaisesRegex(ValueError, 'external or unresolved'):
                load_paths(Path('/a/app'), Path('/a/app'), Path('/a'), 'arm64')

class NotaryTests(unittest.TestCase):
    def test_retry_waits_existing_submission_without_upload(self):
        from collection_stages import submit_dmg
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path = root / 'fixture.dmg'
            path.write_bytes(b'fixture')
            journal = root / 'journal.json'
            calls = []
            def service(cmd, **kwargs):
                calls.append(cmd)
                if 'submit' in cmd:
                    return json.dumps({'id': 'test-id', 'status': 'In Progress'})
                raise ValueError('bounded wait timed out')
            with patch('collection_stages.run', side_effect=service):
                with self.assertRaisesRegex(ValueError, 'timed out'):
                    submit_dmg(path, 'existing-profile', journal)
            self.assertEqual(len([x for x in calls if 'submit' in x]), 1)
            calls.clear()
            with patch('collection_stages.run', side_effect=lambda cmd, **kwargs: calls.append(cmd) or json.dumps({'id': 'test-id', 'status': 'Accepted'})):
                result = submit_dmg(path, 'existing-profile', journal)
            self.assertEqual(result['status'], 'Accepted')
            self.assertFalse(any('submit' in x for x in calls))

    def test_uncertain_upload_is_not_repeated(self):
        from collection_stages import submit_dmg
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'fixture.dmg'
            path.write_bytes(b'fixture')
            journal = Path(temp) / 'journal.json'
            with patch('collection_stages.run', side_effect=ValueError('connection interrupted')):
                with self.assertRaisesRegex(ValueError, 'interrupted'):
                    submit_dmg(path, 'profile', journal)
            with patch('collection_stages.run') as runner:
                with self.assertRaisesRegex(ValueError, 'uncertain'):
                    submit_dmg(path, 'profile', journal)
                runner.assert_not_called()

class BundleTests(unittest.TestCase):
    def fixture(self, directory):
        app = Path(directory) / 'App with spaces.app'
        (app / 'Contents/MacOS').mkdir(parents=True)
        (app / 'Contents/Resources').mkdir()
        manifest = app / 'Contents/Resources/tilo-app.toml'
        manifest.write_text('fixture')
        entry = {'id': 'so.tilo.fixture', 'version': '1.0.0', 'binary': 'fixture', 'manifest_sha256': c.digest(manifest)}
        info = {'CFBundleIdentifier': entry['id'], 'CFBundleVersion': '1.0.0',
                'CFBundleShortVersionString': '1.0.0', 'CFBundlePackageType': 'APPL',
                'CFBundleExecutable': 'fixture', 'LSMinimumSystemVersion': '13.0'}
        (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
        for name in ['fixture', 'tilo-runtime-mcp']:
            path = app / 'Contents/MacOS' / name
            path.write_bytes(bytes.fromhex('cffaedfe') + b'fixture')
            path.chmod(0o755)
        return app, entry

    def test_unsigned_nested_shell_code_is_rejected(self):
        from collection_native import inspect_bundle
        with tempfile.TemporaryDirectory() as temp:
            app, entry = self.fixture(temp)
            (app / 'Contents/MacOS/unreviewed.sh').write_text('#!/bin/sh\n')
            with self.assertRaisesRegex(ValueError, 'nested code'):
                inspect_bundle(app, entry, 'arm64')

    def test_wrong_architecture_fails(self):
        from collection_native import inspect_bundle
        with tempfile.TemporaryDirectory() as temp:
            app, entry = self.fixture(temp)
            with patch('collection_native.run', return_value='x86_64'), self.assertRaisesRegex(ValueError, 'architecture mismatch'):
                inspect_bundle(app, entry, 'arm64')

    def test_missing_runtime_fails(self):
        from collection_native import inspect_bundle
        with tempfile.TemporaryDirectory() as temp:
            app, entry = self.fixture(temp)
            (app / 'Contents/MacOS/tilo-runtime-mcp').unlink()
            with self.assertRaisesRegex(ValueError, 'missing nested code'):
                inspect_bundle(app, entry, 'arm64')

    def test_newer_macho_deployment_floor_is_rejected(self):
        from collection_native import inspect_bundle
        with tempfile.TemporaryDirectory() as temp:
            app, entry = self.fixture(temp)
            with patch('collection_native.run', side_effect=['arm64', 'cmd LC_BUILD_VERSION\n minos 26.0\n']), self.assertRaisesRegex(ValueError, 'deployment'):
                inspect_bundle(app, entry, 'arm64')

class LineageTests(unittest.TestCase):
    def test_signed_resume_rejects_changed_build_input(self):
        from collection_stages import execute
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            (out / 'built').mkdir()
            (out / 'built/file').write_text('original')
            c.write_receipt(out / 'built', {}, 'built')
            (out / 'signed').mkdir()
            c.write_receipt(out / 'signed', {}, 'signed', {
                'identity': {'sha1': '-', 'name': 'ad-hoc', 'team': None}, 'offline': False,
                'built_receipt_sha256': c.digest(out / 'built/receipt.json')})
            (out / 'built/file').write_text('changed')
            args = c.parser().parse_args(['sign', '--identity', 'ad-hoc', '--resume', '--out', temp])
            with patch('collection_stages.disk_floor'), self.assertRaisesRegex(ValueError, 'changed'):
                execute(args, {})

class ClosureTests(unittest.TestCase):
    def test_text_file_cannot_satisfy_embedded_dylib_dependency(self):
        from collection_native import load_paths
        with tempfile.TemporaryDirectory() as temp:
            app, _ = BundleTests().fixture(temp)
            library = app / 'Contents/Resources/invalid.dylib'
            library.write_text('not a library')
            binary = app / 'Contents/MacOS/fixture'
            with patch('collection_native.run', side_effect=['', 'app:\n\t@loader_path/../Resources/invalid.dylib (compatibility version 1.0.0)']), self.assertRaisesRegex(ValueError, 'library'):
                load_paths(binary, binary, app, 'arm64')

    def test_system_path_traversal_is_not_allowlisted(self):
        from collection_native import load_paths
        with patch('collection_native.run', side_effect=['', 'app:\n\t/usr/lib/../../opt/evil.dylib (compatibility version 1.0.0)']), self.assertRaisesRegex(ValueError, 'library'):
            load_paths(Path('/app/main'), Path('/app/main'), Path('/app'), 'arm64')

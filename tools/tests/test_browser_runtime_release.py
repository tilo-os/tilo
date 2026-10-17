"""Exercise engine-only authority, native payload rejection and source coherence."""
import importlib.util
import os
import json
import re
import sys
from unittest.mock import patch
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'tools/apps/browser_runtime_release.py'
spec = importlib.util.spec_from_file_location('browser_runtime_release', SCRIPT)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class BrowserRuntimeRelease(unittest.TestCase):
    def test_native_proof_uses_selected_cargo_argv_after_source_admission(self):
        for arguments, environment, expected in (
                (['--cargo-command', '/native/cargo-driver', '--cargo-argument=--execute-native'],
                 {}, ['/native/cargo-driver', '--execute-native']),
                ([], {'CARGO': '/native/selected-cargo'}, ['/native/selected-cargo'])):
            with patch.object(sys, 'argv', ['release', 'proof-cargo', *arguments]), \
                    patch.dict(os.environ, environment, clear=True), \
                    patch.object(release, 'source_proof', return_value=('a' * 40, 'commit-object')) as admission, \
                    patch.object(release.subprocess, 'run') as run:
                release.main()
                admission.assert_called_once_with()
                run.assert_called_once_with([*expected, 'run', '--locked', '-p', 'runtime-bundle',
                                            '--example', 'browser_runtime_release', '--',
                                            'a' * 40, 'commit-object'], check=True)
        with patch.object(sys, 'argv', ['release', 'proof-cargo']), \
                patch.object(release, 'source_proof', side_effect=ValueError('dirty source')), \
                patch.object(release.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'dirty source'):
                release.main()
            run.assert_not_called()

    def plan(self, overrides):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'output'
            environment = dict(os.environ, GITHUB_OUTPUT=str(output), ENGINE_RUNTIME_ONLY='true',
                               GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_REF='refs/heads/main',
                               GITHUB_SHA=release.head())
            for key in ('COORDINATOR_INTENT', 'REQUESTED_BUILDER', 'BASELINE_SHA', 'BASELINE_RUN_ID',
                        'BASELINE_EVIDENCE', 'BASELINE_RECEIPT_SHA256', 'COORDINATOR_POLICY_SHA256',
                        'VERIFY_FINAL_RECEIPT', 'REQUESTED_APPS', 'REQUESTED_COLLECTION'):
                environment.pop(key, None)
            environment.update(overrides)
            result = subprocess.run(['python3', str(SCRIPT), 'plan'], env=environment,
                                    capture_output=True, text=True)
            return result, output.read_text() if output.exists() else ''

    def test_only_explicit_exact_main_dispatch_enables_engine_release(self):
        result, output = self.plan({})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, 'engine_runtime_only=true\n')
        for overrides in ({'GITHUB_EVENT_NAME': 'push'}, {'GITHUB_EVENT_NAME': 'pull_request'},
                          {'GITHUB_REF': 'refs/heads/codex/task'}, {'GITHUB_REF': 'refs/tags/v1'},
                          {'GITHUB_SHA': 'a' * 40}, {'ENGINE_RUNTIME_ONLY': 'yes'}):
            with self.subTest(overrides=overrides):
                result, output = self.plan(overrides)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(output, '')
        result, output = self.plan({'ENGINE_RUNTIME_ONLY': 'false', 'GITHUB_EVENT_NAME': 'push'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, 'engine_runtime_only=false\n')

    def test_no_qualification_or_unrelated_app_inputs_are_accepted(self):
        for key in ('COORDINATOR_INTENT', 'REQUESTED_BUILDER', 'BASELINE_SHA', 'BASELINE_RUN_ID',
                    'BASELINE_EVIDENCE', 'BASELINE_RECEIPT_SHA256', 'COORDINATOR_POLICY_SHA256',
                    'VERIFY_FINAL_RECEIPT', 'REQUESTED_APPS', 'REQUESTED_COLLECTION'):
            with self.subTest(key=key):
                result, output = self.plan({key: 'unexpected'})
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(output, '')

    def test_runtime_source_manifest_and_os_recipe_must_agree(self):
        self.assertTrue(release.runtime_version(ROOT).endswith('-chromium'))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ('crates/sdk/ui-core/src/webview/chromium/runtime.rs',
                         'apps/browser/tilo-app.toml', 'os/Containerfile'):
                target = root / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text((ROOT / name).read_text())
            version = release.runtime_version(root)
            manifest = root / 'apps/browser/tilo-app.toml'
            manifest.write_text(manifest.read_text().replace(version, 'wrong-chromium'))
            with self.assertRaisesRegex(ValueError, 'Browser optional runtime differs'):
                release.runtime_version(root)

    def test_native_payload_rejects_foreign_machine_missing_elf_and_escape(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'engine'
            root.mkdir()
            with self.assertRaisesRegex(ValueError, 'no native ELF'):
                release.native_payload(root, 'aarch64')
            header = bytearray(20)
            header[:6] = b'\x7fELF\x02\x01'
            header[18:20] = (183).to_bytes(2, 'little')
            (root / 'host').write_bytes(header)
            release.native_payload(root, 'aarch64')
            with self.assertRaisesRegex(ValueError, 'foreign ELF'):
                release.native_payload(root, 'x86_64')
            outside = Path(directory) / 'outside'
            outside.write_bytes(header)
            (root / 'escape').symlink_to(outside)
            with self.assertRaisesRegex(ValueError, 'escapes'):
                release.native_payload(root, 'aarch64')

    def test_normal_publication_selects_the_engine_of_built_or_reused_browser(self):
        environment = dict(REQUESTED_PUBLISH='true', SELECTED_APPS='browser')
        self.assertTrue(release.engine_dependency(environment))
        self.assertFalse(release.engine_dependency(dict(environment, REQUESTED_PUBLISH='false')))
        self.assertFalse(release.engine_dependency(dict(environment, ENGINE_RUNTIME_ONLY='true')))
        self.assertFalse(release.engine_dependency(dict(environment, SIGNAL_PROVIDER_ONLY='true')))
        self.assertFalse(release.engine_dependency(dict(environment, SELECTED_APPS='notes')))
        self.assertTrue(release.engine_dependency(dict(environment, SELECTED_APPS='notes', COMPLETE='true')))
        self.assertFalse(release.engine_dependency(dict(environment, SELECTED_APPS='', COMPLETE='true')))

    def test_workflow_admits_engine_dependencies_after_authorization_and_confines_keys(self):
        workflow = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        plan = workflow.split('\n  plan:\n', 1)[1].split('\n  authorize-publish:', 1)[0]
        self.assertLess(plan.index('id: engine-mode'), plan.index('id: builder'))
        self.assertIn('engine_runtime_only: ${{ steps.engine-mode.outputs.engine_runtime_only }}', plan)
        for job in ('build', 'publish'):
            block = workflow.split('\n  ' + job + ':\n', 1)[1]
            block = re.split(r'\n  [a-z][a-z-]+:\n', block, maxsplit=1)[0]
            self.assertIn("needs.plan.outputs.engine_runtime_only != 'true'", block)
        engine = workflow.split('\n  build-engine-runtime:\n', 1)[1].split('\n  publish-engine-runtime:', 1)[0]
        self.assertIn("if: needs.plan.outputs.engine_runtime_only == 'true' || needs.plan.outputs.engine_runtime_required == 'true'", engine)
        publishing = workflow.split('\n  publish:\n', 1)[1].split('\n  build-signal-provider:', 1)[0]
        self.assertIn('needs: [plan, authorize-publish, build, publish-engine-runtime]', publishing)
        self.assertIn("needs.publish-engine-runtime.result == 'success'", publishing)
        self.assertIn("needs.plan.outputs.engine_runtime_required != 'true' && needs.publish-engine-runtime.result == 'skipped'", publishing)
        self.assertIn('tools/apps/browser_runtime_release.py dependencies', plan)
        self.assertIn('needs: [plan, authorize-publish]', engine)
        compile_step, sealing = engine.split('      - name: Seal the verified engine', 1)
        self.assertNotIn('secrets.', compile_step)
        self.assertIn('secrets.RUNTIME_BUNDLE_SIGNING_KEY', sealing)
        upload = sealing.split('      - uses: actions/upload-artifact', 1)[1]
        self.assertIn('dist/browser-runtime-release/package/**', upload)
        self.assertNotIn('/work/', upload)
        publisher = (ROOT / 'tools/apps/publish-browser-runtime.sh').read_text()
        self.assertLess(publisher.index('browser_runtime_release.py verify'), publisher.index('jwt=$(curl'))
        self.assertIn('tools/apps/publish-runtime-package.sh', publisher)
        self.assertIn('( $GITHUB_EVENT_NAME == push || $GITHUB_EVENT_NAME == workflow_dispatch ) && $GITHUB_REF == refs/heads/main', publisher)

    def test_snapshot_admission_matches_actual_git_tree_and_rejects_source_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            (root / 'source.rs').write_text('fn main() {}\n')
            (root / 'a').mkdir()
            (root / 'a/tool').write_text('#!/bin/sh\ntrue\n')
            (root / 'a/tool').chmod(0o755)
            (root / 'a.txt').write_text('directory ordering\n')
            (root / 'alias').symlink_to('source.rs')
            subprocess.run(['git', '-C', str(root), 'add', 'source.rs', 'a', 'a.txt', 'alias'], check=True)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=Proof',
                            '-c', 'user.email=proof@example.test', 'commit', '-qm', 'source proof'], check=True)
            revision = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
            tree = subprocess.check_output(['git', '-C', str(root), 'write-tree'], text=True).strip()
            commit = subprocess.check_output(['git', '-C', str(root), 'cat-file', 'commit', revision])
            source = release.Source(revision, commit)
            self.assertEqual(release.snapshot_tree(root).hex(), tree)
            with patch.dict(os.environ, {'REQUESTED_PUBLISH': 'false'}, clear=True):
                self.assertEqual(release.verify_source(source, root), revision)
                (root / 'source.rs').write_text('changed\n')
                with self.assertRaisesRegex(ValueError, 'source tree differs'):
                    release.verify_source(source, root)
                (root / 'source.rs').write_text('fn main() {}\n')
                (root / 'a/tool').chmod(0o644)
                with self.assertRaisesRegex(ValueError, 'source tree differs'):
                    release.verify_source(source, root)
                (root / 'a/tool').chmod(0o755)
                (root / 'alias').unlink()
                (root / 'alias').symlink_to('a/tool')
                with self.assertRaisesRegex(ValueError, 'source tree differs'):
                    release.verify_source(source, root)
                (root / 'alias').unlink()
                (root / 'alias').symlink_to('source.rs')
                (root / 'a.txt').unlink()
                with self.assertRaisesRegex(ValueError, 'source tree differs'):
                    release.verify_source(source, root)
                (root / 'a.txt').write_text('directory ordering\n')
                (root / 'unexpected.rs').write_text('not in admitted commit\n')
                with self.assertRaisesRegex(ValueError, 'source tree differs'):
                    release.verify_source(source, root)
                (root / 'unexpected.rs').unlink()
                with self.assertRaisesRegex(ValueError, 'commit object differs'):
                    release.verify_source(release.Source(revision, commit + b'tampered'), root)
                with self.assertRaisesRegex(ValueError, 'commit object differs'):
                    release.verify_source(release.Source('a' * 40, commit), root)
                (root / 'target').mkdir()
                (root / 'target/output').write_text('known generated build output\n')
                self.assertEqual(release.verify_source(source, root), revision)

    def test_local_canonical_source_view_excludes_only_git_ignored_host_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            (root / '.gitignore').write_text('__pycache__/\n')
            (root / 'source.py').write_text('print("source")\n')
            subprocess.run(['git', '-C', str(root), 'add', '.gitignore', 'source.py'], check=True)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=Proof',
                            '-c', 'user.email=proof@example.test', 'commit', '-qm', 'source view'], check=True)
            tree = subprocess.check_output(['git', '-C', str(root), 'write-tree'], text=True).strip()
            revision = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
            commit = subprocess.check_output(['git', '-C', str(root), 'cat-file', 'commit', revision])
            (root / '__pycache__').mkdir()
            (root / '__pycache__/source.pyc').write_bytes(b'generated, not mirrored')
            self.assertEqual(release.tracked_source_tree(root, revision).hex(), tree)
            with patch.dict(os.environ, {'REQUESTED_PUBLISH': 'false'}, clear=True):
                with self.assertRaisesRegex(ValueError, 'source tree differs'):
                    release.verify_source(release.Source(revision, commit), root)
            (root / 'source.py').write_text('changed source\n')
            self.assertNotEqual(release.tracked_source_tree(root, revision).hex(), tree)

    def test_snapshot_proof_refuses_publication_keys_and_ci_before_compiler_children(self):
        source = release.Source('a' * 40, b'not admitted')
        for variable in ('GITHUB_EVENT_NAME', 'GITEA_EVENT_NAME', 'FORGEJO_EVENT_NAME',
                         'GITHUB_ACTIONS', 'GITEA_ACTIONS', 'FORGEJO_ACTIONS',
                         'RUNTIME_BUNDLE_IDENTITY_FILE', 'TAPP_PUBLISHER_IDENTITY_FILE',
                         'RUNTIME_BUNDLE_SIGNING_KEY', 'TAPP_PUBLISHER_IDENTITY',
                         'TILO_ENGINE_VERIFIER_BIN_DIR'):
            with self.subTest(variable=variable):
                with patch.dict(os.environ, {'REQUESTED_PUBLISH': 'false', variable: 'true'}, clear=True):
                    with patch.object(release.subprocess, 'run') as children:
                        with self.assertRaisesRegex(ValueError, 'snapshot proof refuses'):
                            release.compile_engine(ROOT / 'target/refused-engine-proof', 'aarch64', source)
                        children.assert_not_called()
        with patch.dict(os.environ, {'REQUESTED_PUBLISH': 'true'}, clear=True):
            with self.assertRaisesRegex(ValueError, 'cannot publish'):
                release.verify_source(source)
        result = subprocess.run(['python3', str(SCRIPT), 'compile', '--source-commit', 'eA=='],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('exclusive to the nonpublishing native proof', result.stderr)
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'refused'
            result = subprocess.run(['python3', str(SCRIPT), 'native-proof', '--out', str(out),
                                     '--arch', 'aarch64', '--source', 'a' * 40, '--source-commit', 'eA=='],
                                    env=dict(os.environ, REQUESTED_PUBLISH='false'),
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('must stay inside target', result.stderr)
            self.assertFalse(out.exists())

    def test_cross_architecture_verification_executes_only_host_tools(self):
        # The fake verifier isolates executable routing; the explicit native fixture
        # separately proves the actual signed archive and thin-object reconstruction.
        source = release.head()
        key = 'b' * 64
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            host_bin = root / 'host-bin'
            host_bin.mkdir()
            trace = root / 'executed'
            cli = host_bin / 'tilo-app-package'
            cli.write_text('#!' + sys.executable + '\nimport sys\nfrom pathlib import Path\n'
                           + f'with Path({str(trace)!r}).open("a") as output: output.write(sys.argv[1] + "\\n")\n'
                           + 'if sys.argv[1] == "inspect": print(Path(sys.argv[-1]).read_text())\n')
            cli.chmod(0o755)
            for architecture in ('aarch64', 'x86_64'):
                out = root / architecture
                (out / 'bin').mkdir(parents=True)
                foreign = out / 'bin/tilo-app-package'
                foreign.write_text('#!/bin/sh\nexit 97\n')
                foreign.chmod(0o755)
                package = out / 'package'
                package.mkdir()
                proof = release.provenance(architecture, source)
                version = '0.1.9+g' + source
                proof.update(package_version=version, app_publisher=key)
                tapp = package / 'engine.tapp'
                isa = 'v3' if architecture == 'x86_64' else 'armv8-a'
                tapp.write_text('--- manifest.toml ---\n[package]\n'
                                + f'id = "{release.PACKAGE}"\nversion = "{version}"\n'
                                + f'provides = "{proof["runtime"]}"\nkind = "runtime"\n'
                                + f'[package.isa]\n{architecture} = "{isa}"\n'
                                + '[entrypoint]\ntype = "cli"\nbin = "lib/chromium/tilo-chromium-host"\nsplash-budget = 150\n'
                                + '--- payload (0 files) ---\n')
                proof['files'] = {tapp.name: release.digest(tapp)}
                (package / 'provenance.json').write_text(json.dumps(proof))
                with patch.dict(os.environ, TILO_ENGINE_VERIFIER_BIN_DIR=str(host_bin)):
                    release.verify_package(out, architecture, source, '9', key)
                    with self.assertRaisesRegex(ValueError, 'publisher differs'):
                        release.verify_package(out, architecture, source, '9', 'c' * 64)
                    if architecture == 'x86_64':
                        lock_path = ROOT / 'scripts/browser/cef-lock.json'
                        lock = json.loads(lock_path.read_text())
                        lock['platforms']['linux64']['source_patch_sha256'] = 'd' * 64
                        read_text = Path.read_text

                        def read_locked_source(path, *args, **kwargs):
                            if path == lock_path:
                                return json.dumps(lock)
                            return read_text(path, *args, **kwargs)

                        with patch.object(Path, 'read_text', read_locked_source):
                            # Source binding is checked before the verifier or any upload.
                            with self.assertRaisesRegex(ValueError, 'provenance differs'):
                                release.verify_package(out, architecture, source, '9', key)
                            proof['cef_source_patch_sha256'] = 'e' * 64
                            (package / 'provenance.json').write_text(json.dumps(proof))
                            with self.assertRaisesRegex(ValueError, 'provenance differs'):
                                release.verify_package(out, architecture, source, '9', key)
                            lock['platforms']['linux64']['source_patch_sha256'] = 'latest'
                            with self.assertRaisesRegex(ValueError, 'invalid rebuilt CEF source patch'):
                                release.verify_package(out, architecture, source, '9', key)
                        proof.pop('cef_source_patch_sha256')
                        (package / 'provenance.json').write_text(json.dumps(proof))
                    for kind in ('linked-tapp', 'extra-directory'):
                        with self.subTest(kind=kind):
                            if kind == 'linked-tapp':
                                saved = out / 'saved.tapp'
                                tapp.rename(saved)
                                tapp.symlink_to(saved)
                                invalid = tapp
                            else:
                                invalid = package / 'unrecorded'
                                invalid.mkdir()
                            try:
                                with self.assertRaisesRegex(ValueError, 'nonregular member'):
                                    release.verify_package(out, architecture, source, '9', key)
                            finally:
                                if kind == 'linked-tapp':
                                    invalid.unlink()
                                    saved.rename(tapp)
                                else:
                                    invalid.rmdir()
                    baseline = tapp.read_text()
                    for invalid_host in ('bin = \"missing-host\"', 'bin = \"../host\"', ''):
                        tapp.write_text(baseline.replace('bin = \"lib/chromium/tilo-chromium-host\"', invalid_host))
                        proof['files'] = {tapp.name: release.digest(tapp)}
                        (package / 'provenance.json').write_text(json.dumps(proof))
                        with self.assertRaisesRegex(ValueError, 'declare its executable host'):
                            release.verify_package(out, architecture, source, '9', key)
                    other = 'x86_64' if architecture == 'aarch64' else 'aarch64'
                    other_isa = 'v3' if other == 'x86_64' else 'armv8-a'
                    for invalid in (f'{architecture} = "v4"' if architecture == 'x86_64'
                                    else f'{architecture} = "v8.2-a"',
                                    f'{other} = "{other_isa}"',
                                    f'{architecture} = "{isa}"\n{other} = "{other_isa}"'):
                        tapp.write_text(baseline.replace(f'{architecture} = "{isa}"', invalid))
                        proof['files'] = {tapp.name: release.digest(tapp)}
                        (package / 'provenance.json').write_text(json.dumps(proof))
                        with self.assertRaisesRegex(ValueError, 'architecture differs'):
                            release.verify_package(out, architecture, source, '9', key)
            self.assertEqual(trace.read_text().splitlines(), ['verify', 'inspect'] * 14)

    def test_publication_wrong_event_stops_before_tools_credentials_or_network(self):
        for event, ref in (('pull_request', 'refs/heads/main'), ('push', 'refs/heads/task'),
                           ('workflow_dispatch', 'refs/heads/task')):
            result = subprocess.run(['bash', str(ROOT / 'tools/apps/publish-browser-runtime.sh')],
                                    env={'PATH': os.environ['PATH'], 'GITHUB_EVENT_NAME': event,
                                         'GITHUB_REF': ref}, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stderr, '')


if __name__ == '__main__':
    unittest.main()

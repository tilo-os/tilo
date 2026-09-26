import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1] / "apps"))
import create_app_receipt as producer
import final_app_receipt as final


class ReceiptProductionPathTests(unittest.TestCase):
    def test_pinned_receipt_adapter_runs_before_upload_and_writes_unpublished_receipt(self):
        with self.subTest(phase='actual adapter writes unpublished receipt and public policy'):
            # Only canonical CLI cryptographic execution is mocked. The real archive,
            # checksums, index, metadata adapter, signer policy and receipt writer run.
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                tree = root / 'collection'
                (tree / 'tapp').mkdir(parents=True)
                runtime = tree / 'runtime/tilo-runtime/1'
                runtime.mkdir(parents=True)
                pkg, bundle_tool = root / 'tilo-app-package', root / 'tilo-runtime-bundle'
                pkg.write_bytes(b'package executable fixture')
                bundle_tool.write_bytes(b'runtime executable fixture')
                signers = root / 'signers.json'
                signers.write_text(json.dumps({'app_publisher': '1' * 64, 'runtime_publisher': '2' * 64}))
                manifest = b'[package]\nid="dev.test.app"\nversion="1"\nruntime="tilo-runtime/1"\n[package.isa]\nx86_64="v3"\n'
                tapp = tree / 'tapp/app-1-x86_64.tapp'
                with tarfile.open(tapp, 'w') as tar:
                    member = tarfile.TarInfo('manifest.toml')
                    member.size = len(manifest)
                    tar.addfile(member, io.BytesIO(manifest))
                (runtime / 'bundle.toml').write_text(f'version="1"\narch="x86_64"\nroot_hash="{"3" * 64}"\npublisher_hex="{"2" * 64}"\nsignature_hex="{"4" * 128}"\n')
                (tree / 'runtime/runtime-bundle-publisher.hex').write_text('2' * 64)
                index = {'format': 1, 'source': 'a' * 40, 'architecture': 'x86_64', 'apps': [
                    {'file': tapp.name, 'id': 'dev.test.app', 'version': '1', 'runtime': 'tilo-runtime/1', 'sha256': producer.channel.sha(tapp)}]}
                (tree / 'index.json').write_text(json.dumps(index))
                paths = sorted(path for path in tree.rglob('*') if path.is_file())
                (tree / 'SHA256SUMS').write_text(''.join(producer.channel.sha(path) + '  ' + path.relative_to(tree).as_posix() + '\n' for path in paths))
                archive = tree / 'tilo-apps-x86_64.tar.gz'
                with tarfile.open(archive, 'w:gz') as tar:
                    for name in ('tapp', 'runtime', 'index.json', 'SHA256SUMS'):
                        tar.add(tree / name, arcname=name)
                (tree / (archive.name + '.sha256')).write_text(producer.channel.sha(archive) + '  ' + archive.name + '\n')
                seen = []

                def execute(command, **kwargs):
                    seen.append(command)
                    self.assertEqual(set(kwargs['env']), {'PATH', 'LANG', 'LC_ALL'})
                    self.assertNotIn('ACTIONS_ID_TOKEN_REQUEST_TOKEN', kwargs['env'])
                    self.assertNotIn('LD_PRELOAD', kwargs['env'])
                    if command[0] == str(bundle_tool):
                        self.assertIn('--require-signature', command)
                        self.assertEqual(command[command.index('--require-publisher') + 1], '2' * 64)
                        self.assertEqual(command[command.index('--require-arch') + 1], 'x86_64')
                        output = ''
                    elif command[1] == 'verify':
                        self.assertEqual(command[2:4], ['--trusted-key', '1' * 64])
                        output = 'ok: tapp:' + '5' * 64 + ' verifies and publisher is trusted (full)\n'
                    else:
                        self.assertEqual(command[1], 'inspect')
                        output = 'package-id: tapp:' + '5' * 64 + '\npublisher-identity: ed25519:' + '1' * 64 + '\nsigned: yes\n\n--- manifest.toml ---\n'
                    return subprocess.CompletedProcess(command, 0, output, '')

                with patch.object(subprocess, 'run', side_effect=execute):
                    result = producer.create(tree, pkg, bundle_tool, signers, 'a' * 40, 'x86_64')
                self.assertEqual(len(seen), 3)
                self.assertEqual(result['publication'], {'status': 'unpublished'})
                self.assertEqual(result['verification']['backend'], 'canonical-cli-v1')
                self.assertEqual(json.loads((tree / 'publication-receipt.json').read_text()), result)
                policy = json.loads((tree / 'verification-policy.json').read_text())
                self.assertEqual(policy['app_publishers'], ['1' * 64])
                self.assertEqual(policy['canonical_binaries']['tilo_pkg_sha256'], hashlib.sha256(pkg.read_bytes()).hexdigest())
                policy_path = root / 'independently-approved-policy.json'
                policy_path.write_text(json.dumps(policy))
                policy_hash = producer.channel.sha(policy_path)
                seen.clear()
                with patch.dict(os.environ, {'ACTIONS_ID_TOKEN_REQUEST_TOKEN': 'secret', 'LD_PRELOAD': 'bad.so'}), patch.object(subprocess, 'run', side_effect=execute):
                    merged = final.verify(tree, pkg, bundle_tool, policy_path, policy_hash, 'a' * 40, 'x86_64')
                self.assertEqual(len(seen), 3)  # Verify again; never copy the build receipt.
                self.assertEqual(merged['collection'], result['collection'])
                self.assertEqual(merged['publication'], {'status': 'unpublished'})
                # Reused members are covered by fresh verification, not only new app inputs.
                original = tapp.read_bytes()
                tapp.write_bytes(original + b'tampered reused member')
                with patch.object(subprocess, 'run') as never:
                    with self.assertRaises(ValueError):
                        final.verify(tree, pkg, bundle_tool, policy_path, policy_hash, 'a' * 40, 'x86_64')
                    never.assert_not_called()
                tapp.write_bytes(original)

                receipt_path = tree / 'final-publication-receipt.json'
                receipt_hash = producer.channel.sha(receipt_path)
                url = 'https://code.tilo.so/api/packages/tilo/generic/tilo-app-collections/0.0.0-' + 'a' * 40 + '/tilo-apps-x86_64.tar.gz'
                output = tree / 'published-receipt.json'
                bound = final.bind(receipt_path, receipt_hash, archive, url, policy_path, policy_hash, output)
                self.assertEqual(bound['publication']['status'], 'published')
                self.assertEqual(bound['publication']['upload_receipt_sha256'], producer.channel.sha(output.with_suffix('.upload.json')))
                manifest_raw = json.dumps({'schema': producer.channel.CHANNEL_SCHEMA, 'generation': 1,
                    'source_sha': 'a' * 40, 'architectures': {'x86_64': bound}}).encode()
                selected = producer.channel.select(manifest_raw, hashlib.sha256(manifest_raw).hexdigest(), 'b' * 40, 'x86_64', policy)
                self.assertTrue(selected['usable_for_download'])
                with self.assertRaisesRegex(ValueError, 'fallback forbidden'):
                    producer.channel.select(manifest_raw, hashlib.sha256(manifest_raw).hexdigest(), 'b' * 40, 'aarch64', policy)
                wrong = root / 'wrong-readback'
                wrong.write_bytes(b'wrong')
                with self.assertRaisesRegex(ValueError, 'readback mismatch'):
                    final.bind(receipt_path, receipt_hash, wrong, url, policy_path, policy_hash, output)
                with self.assertRaisesRegex(ValueError, 'trusted digest mismatch'):
                    final.bind(receipt_path, '0' * 64, archive, url, policy_path, policy_hash, output)
                policy_path.write_text(json.dumps(dict(policy, runtime_publisher='9' * 64)))
                with patch.object(subprocess, 'run') as never:
                    with self.assertRaisesRegex(ValueError, 'trusted digest mismatch'):
                        final.verify(tree, pkg, bundle_tool, policy_path, policy_hash, 'a' * 40, 'x86_64')
                    never.assert_not_called()
                policy_path.write_text(json.dumps(policy))
                pkg.write_bytes(b'replaced executable')
                with patch.object(subprocess, 'run') as never:
                    with self.assertRaisesRegex(ValueError, 'binary digest mismatch'):
                        final.verify(tree, pkg, bundle_tool, policy_path, policy_hash, 'a' * 40, 'x86_64')
                    never.assert_not_called()
        with self.subTest(phase='unpinned adapter is not imported'):
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                marker = root / 'executed'
                (root / 'canonical_app_verifier.py').write_text(f"from pathlib import Path\nPath({str(marker)!r}).write_text('executed')\n")
                policy = root / 'policy.json'
                policy.write_text(json.dumps({'canonical_binaries': {'tilo_pkg_sha256': '1' * 64,
                    'tilo_runtime_bundle_sha256': '2' * 64}, 'verifier_sha256': '3' * 64}))
                with patch.object(final, '__file__', str(root / 'final_app_receipt.py')):
                    with self.assertRaisesRegex(ValueError, 'trusted verifier digest mismatch'):
                        final.preflight(root / 'pkg', root / 'runtime', policy, producer.channel.sha(policy))
                self.assertFalse(marker.exists())
        with self.subTest(phase='workflow runs receipt before upload even without publication'):
            root = Path(__file__).parents[2]
            workflow = (root / '.forgejo/workflows/release-apps.yml').read_text()
            step = workflow.split('- name: Generate unpublished receipt with canonical verifiers', 1)[1].split('\n      - ', 1)[0]
            self.assertNotIn('if:', step)
            self.assertIn('python3 tools/apps/create_app_receipt.py', step)
            self.assertIn('--signers dist/app-release/work/receipt-signers.json', step)
            self.assertIn('dist/app-release/publication-receipt.json', workflow)
            self.assertIn('dist/app-release/verification-policy.json', workflow)
            self.assertNotIn('dist/app-release/work/**', workflow)
            self.assertIn('    branches: [main]', workflow)
            publish = workflow.split('\n  publish:\n', 1)[1]
            self.assertIn('fromJSON(needs.plan.outputs.matrix).include[0].runner', publish)
            # The digest itself is owned by release-apps.yml; require only that the
            # publisher runs in the immutable public release-tools image.
            self.assertRegex(
                publish,
                r'image: registry\.tilo\.so/tilo/tilo-release-tools@sha256:[0-9a-f]{64}\n',
            )
            self.assertIn('vars.TILO_APP_FINAL_RECEIPT_POLICY_SHA256', publish)
            self.assertLess(publish.index('tools/apps/final_app_receipt.py'), publish.index('bundle_upload='))
            self.assertIn('tools/upload_immutable_packages.sh "$bundle_upload"', publish)
            self.assertIn('bind-readback', publish)
            self.assertIn('tilo-apps-$arch.receipt.json', publish)


if __name__ == '__main__':
    unittest.main()

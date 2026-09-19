import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import create_app_receipt as producer


class ReceiptProductionPathTests(unittest.TestCase):
    def test_actual_adapter_writes_unpublished_receipt_and_public_policy(self):
        # Only canonical CLI cryptographic execution is mocked. The real archive,
        # checksums, index, metadata adapter, signer policy and receipt writer run.
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            tree = root / 'collection'
            (tree / 'qpkg').mkdir(parents=True)
            runtime = tree / 'runtime/tilo-runtime/1'
            runtime.mkdir(parents=True)
            pkg, bundle_tool = root / 'tilo-pkg', root / 'tilo-runtime-bundle'
            pkg.write_bytes(b'package executable fixture')
            bundle_tool.write_bytes(b'runtime executable fixture')
            signers = root / 'signers.json'
            signers.write_text(json.dumps({'app_publisher': '1' * 64, 'runtime_publisher': '2' * 64}))
            manifest = b'[package]\nid="dev.test.app"\nversion="1"\nruntime="tilo-runtime/1"\n[package.isa]\nx86_64="v3"\n'
            qpkg = tree / 'qpkg/app-1-x86_64.qpkg'
            with tarfile.open(qpkg, 'w') as tar:
                member = tarfile.TarInfo('manifest.toml')
                member.size = len(manifest)
                tar.addfile(member, io.BytesIO(manifest))
            (runtime / 'bundle.toml').write_text(f'version="1"\narch="x86_64"\nroot_hash="{"3" * 64}"\npublisher_hex="{"2" * 64}"\nsignature_hex="{"4" * 128}"\n')
            (tree / 'runtime/runtime-bundle-publisher.hex').write_text('2' * 64)
            index = {'format': 1, 'source': 'a' * 40, 'architecture': 'x86_64', 'apps': [
                {'file': qpkg.name, 'id': 'dev.test.app', 'version': '1', 'runtime': 'tilo-runtime/1', 'sha256': producer.channel.sha(qpkg)}]}
            (tree / 'index.json').write_text(json.dumps(index))
            paths = sorted(path for path in tree.rglob('*') if path.is_file())
            (tree / 'SHA256SUMS').write_text(''.join(producer.channel.sha(path) + '  ' + path.relative_to(tree).as_posix() + '\n' for path in paths))
            archive = tree / 'tilo-apps-x86_64.tar.gz'
            with tarfile.open(archive, 'w:gz') as tar:
                for name in ('qpkg', 'runtime', 'index.json', 'SHA256SUMS'):
                    tar.add(tree / name, arcname=name)
            (tree / (archive.name + '.sha256')).write_text(producer.channel.sha(archive) + '  ' + archive.name + '\n')
            seen = []

            def execute(command, **kwargs):
                seen.append(command)
                if command[0] == str(bundle_tool):
                    self.assertIn('--require-signature', command)
                    self.assertEqual(command[command.index('--require-publisher') + 1], '2' * 64)
                    self.assertEqual(command[command.index('--require-arch') + 1], 'x86_64')
                    output = ''
                elif command[1] == 'verify':
                    self.assertEqual(command[2:4], ['--trusted-key', '1' * 64])
                    output = 'ok: qpkg:' + '5' * 64 + ' verifies and publisher is trusted\n'
                else:
                    self.assertEqual(command[1], 'inspect')
                    output = 'package-id: qpkg:' + '5' * 64 + '\npublisher-identity: ed25519:' + '1' * 64 + '\nsigned: yes\n\n--- manifest.toml ---\n'
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

    def test_workflow_runs_receipt_before_upload_even_without_publication(self):
        root = Path(__file__).parents[2]
        workflow = (root / '.forgejo/workflows/release-apps.yml').read_text()
        step = workflow.split('- name: Generate unpublished receipt with canonical verifiers', 1)[1].split('- uses:', 1)[0]
        self.assertNotIn('if:', step)
        self.assertIn('python3 tools/create_app_receipt.py', step)
        self.assertIn('--signers dist/app-release/work/receipt-signers.json', step)
        self.assertIn('dist/app-release/publication-receipt.json', workflow)
        self.assertIn('dist/app-release/verification-policy.json', workflow)
        self.assertNotIn('dist/app-release/work/**', workflow)
        self.assertIn('# branches: [main]', workflow)


if __name__ == '__main__':
    unittest.main()

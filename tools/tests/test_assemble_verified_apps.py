import hashlib
import gzip
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).parents[1]))
sys.path.append(str(Path(__file__).parents[1] / 'apps'))
import assemble_verified_apps as assembly
import test_app_channel_manifest as fixtures


class AssemblyTests(unittest.TestCase):
    def test_incremental_merge_reuses_exact_catalog_and_refuses_runtime_change(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            built, baseline = root / 'built', root / 'baseline'
            for tree in (built, baseline):
                (tree / 'tapp').mkdir(parents=True)
                (tree / 'runtime/tilo-runtime/1').mkdir(parents=True)
                (tree / 'runtime/tilo-runtime/1/bundle.toml').write_text('root_hash="same"')
            (built / 'tapp/a-1-x86_64.tapp').write_bytes(b'new-a')
            (baseline / 'tapp/a-1-x86_64.tapp').write_bytes(b'old-a')
            (baseline / 'tapp/b-1-x86_64.tapp').write_bytes(b'old-b')
            (baseline / 'tapp/obsolete-1-x86_64.tapp').write_bytes(b'old')
            plan = {'catalog': [{'name': 'a', 'version': '1'}, {'name': 'b', 'version': '1'}]}
            reused = assembly.merge(plan, built, baseline, root / 'merged', 'x86_64', ['a'])
            self.assertEqual(reused, ['b-1-x86_64.tapp'])
            self.assertEqual({p.name for p in (root / 'merged').iterdir()}, {'a-1-x86_64.tapp', 'b-1-x86_64.tapp'})
            self.assertEqual((root / 'merged/a-1-x86_64.tapp').read_bytes(), b'new-a')
            # Baseline b has the exact required version, but b was selected:
            # silently reusing its old signed bytes would lose a source change.
            with self.assertRaisesRegex(ValueError, 'selected app missing.*b'):
                assembly.merge(plan, built, baseline, root / 'selected-missing', 'x86_64', ['a', 'b'])
            self.assertFalse((root / 'selected-missing').exists())
            with self.assertRaisesRegex(ValueError, 'unique subset'):
                assembly.merge(plan, built, baseline, root / 'outside-scope', 'x86_64', ['not-in-collection'])
            with self.assertRaisesRegex(ValueError, 'unique subset'):
                assembly.merge(plan, built, baseline, root / 'duplicate-selection', 'x86_64', ['a', 'a'])

            (built / 'runtime/tilo-runtime/1/bundle.toml').write_text('root_hash="changed"')
            with self.assertRaisesRegex(ValueError, 'full rebuild'):
                assembly.merge(plan, built, baseline, root / 'bad', 'x86_64', ['a'])
            with self.assertRaisesRegex(ValueError, 'without verified baseline'):
                assembly.merge(plan, built, None, root / 'missing', 'x86_64', ['a'])

    def test_tar_rejects_traversal_links_duplicates_and_resource_expansion(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            for i, kind in enumerate(('traversal', 'link', 'duplicate', 'expansion')):
                archive = root / f'{i}.tar.gz'
                with tarfile.open(archive, 'w:gz') as tar:
                    member = tarfile.TarInfo('../escape' if kind == 'traversal' else 'tapp/a')
                    if kind == 'link':
                        member.type = tarfile.SYMTYPE
                        member.linkname = '/outside'
                    if kind == 'expansion':
                        member.size = assembly.MAX_EXPANDED + 1
                        tar.fileobj.write(member.tobuf())  # Header rejects before missing payload is read.
                    else:
                        tar.addfile(member)
                    if kind == 'duplicate':
                        tar.addfile(member)
                with self.subTest(kind=kind), self.assertRaises(ValueError):
                    assembly.extract_tar(archive, root / f'out{i}')
            self.assertFalse((root / 'escape').exists())

    def test_baseline_fetch_pins_proof_run_artifact_receipt_and_archive(self):
        fixture = fixtures.ChannelReaderTests()
        fixture.setUp()
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            tree = root / 'tree'
            (tree / 'tapp').mkdir(parents=True)
            (tree / 'runtime').mkdir()
            (tree / 'tapp/app-1-x86_64.tapp').write_bytes(b'package')
            (tree / 'runtime/bundle').write_bytes(b'runtime')
            (tree / 'index.json').write_text('{}')
            files = sorted(p for p in tree.rglob('*') if p.is_file())
            (tree / 'SHA256SUMS').write_text(''.join(assembly.channel.sha(p) + '  ' + p.relative_to(tree).as_posix() + '\n' for p in files))
            archive = root / 'tilo-apps-x86_64.tar.gz'
            with tarfile.open(archive, 'w:gz') as tar:
                for name in ('tapp', 'runtime', 'index.json', 'SHA256SUMS'):
                    tar.add(tree / name, arcname=name)
            receipt = fixture.receipt
            receipt['collection'].update(sha256=assembly.channel.sha(archive), bytes=archive.stat().st_size)
            raw = json.dumps(receipt).encode()
            desired = {'source_sha': 'a' * 40, 'builder_image': 'builder', 'collection': 'default',
                       'architectures': ['x86_64'], 'policy_sha256': '9' * 64}
            proof = {'schema': 'tilo.coordinator-verification/v1', 'backend': 'canonical-final-artifact-v1',
                     'run_id': 101, 'desired': desired, 'receipts': {'x86_64': hashlib.sha256(raw).hexdigest()}}
            payload = io.BytesIO()
            with zipfile.ZipFile(payload, 'w') as zipper:
                zipper.writestr(archive.name, archive.read_bytes())
                zipper.writestr('final-publication-receipt.json', raw)
                zipper.writestr('assembly-provenance.json', '{}')
            calls = []
            class API:
                head = 'a' * 40
                def get(self, path, output=None):
                    calls.append(path)
                    if output:
                        output.write_bytes(payload.getvalue())
                    elif path.endswith('/101'):
                        return {'id': 101, 'commit_sha': self.head, 'status': 'success', 'workflow_id': 'release-apps.yml', 'trigger_event': 'workflow_dispatch'}
                    else:
                        return [{'id': 202, 'run_id': 101, 'name': 'final-app-collection-x86_64', 'expired': False}]
            def fetch(api, work, expected=None):
                work.mkdir()
                return assembly.baseline_tree(api, proof, expected or assembly.canonical_hash(proof), 'a' * 40,
                    'builder', 'default', 'x86_64', '9' * 64, work, fixture.policy)
            result = fetch(API(), root / 'work')
            self.assertEqual((result / 'tapp/app-1-x86_64.tapp').read_bytes(), b'package')
            self.assertEqual(calls, ['/actions/runs/101', '/actions/runs/101/artifacts?page=1&limit=50', '/actions/artifacts/202/zip'])
            with self.assertRaisesRegex(ValueError, 'proof digest'):
                fetch(API(), root / 'bad-proof', '0' * 64)
            api = API()
            api.head = 'b' * 40
            with self.assertRaisesRegex(ValueError, 'provenance'):
                fetch(api, root / 'bad-run')
            proof['receipts']['x86_64'] = '0' * 64
            with self.assertRaisesRegex(ValueError, 'trusted digest'):
                fetch(API(), root / 'bad-receipt')

    def test_workflow_is_nonpublishing_and_uses_only_task_token_for_read(self):
        text = (Path(__file__).parents[2] / '.forgejo/workflows/release-apps.yml').read_text()
        job = text.split('\n  qualify-collection:\n')[1].split('\n  publish:\n')[0]
        self.assertIn("needs.plan.outputs.publish == 'false'", job)
        self.assertIn('FORGEJO_ARTIFACT_TOKEN: ${{ github.token }}', job)
        self.assertNotIn('enable-openid-connect', job)
        self.assertNotIn('put_immutable', job)
        self.assertNotIn('cargo ', job)
        self.assertIn('SELECTED_APPS: ${{ needs.plan.outputs.apps }}', job)
        self.assertIn('--selected-apps "$SELECTED_APPS"', job)
        self.assertIn('assembly-provenance.json', job)
        self.assertIn('final-publication-receipt.json', job)


if __name__ == '__main__':
    unittest.main()

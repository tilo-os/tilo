import copy
from contextlib import redirect_stdout
import base64
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile
sys.path.insert(0, str(Path(__file__).parents[1]))
sys.path.append(str(Path(__file__).parents[1] / 'apps'))
import accept_app_artifacts as accept
import app_packaging_product as product
import assemble_verified_apps as assembler
import test_app_channel_manifest as fixtures


class AcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        fixture = fixtures.ChannelReaderTests()
        fixture.setUp()
        self.policy = self.root / 'platform-policy.json'
        self.policy.write_text(json.dumps(fixture.policy))
        self.policy_sha = accept.channel.sha(self.policy)
        self.platform_policy = {'enabled': True, 'qualification_only': True, 'final_receipt_policy_sha256': self.policy_sha}
        self.intent = {'id': '11111111-1111-1111-1111-111111111111', 'baseline': None, 'baseline_evidence': None,
            'desired': {'source_sha': 'a' * 40, 'builder_image': 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'b' * 64,
                        'collection': 'default', 'architectures': ['x86_64'], 'policy_sha256': product.core.receipt_key(self.platform_policy)}}
        self.run = {'id': 100, 'status': 'success', 'repository': {'id': 23, 'full_name': 'tilo/tilo'},
            'workflow_id': 'release-apps.yml', 'trigger_event': 'workflow_dispatch', 'commit_sha': 'a' * 40,
            'event_payload': json.dumps({'ref': 'a' * 40, 'inputs': accept.dispatch_inputs(self.intent)})}
        self.job = {'id': 200, 'repo_id': 23, 'run_id': 100, 'status': 'success', 'name': 'qualify-collection',
                    'attempt': 2, 'task_id': 300, 'handle': 'current-task-handle'}
        archive = io.BytesIO()
        with tarfile.open(fileobj=archive, mode='w:gz') as tar:
            for name, raw in [('index.json', b'{}'), ('tapp/app.tapp', b'signed-package'), ('runtime/bundle', b'signed-runtime')]:
                member = tarfile.TarInfo(name)
                member.size = len(raw)
                tar.addfile(member, io.BytesIO(raw))
        self.archive = archive.getvalue()
        self.receipt = fixture.receipt
        self.receipt['collection'].update(sha256=hashlib.sha256(self.archive).hexdigest(), bytes=len(self.archive))
        raw = json.dumps(self.receipt).encode()
        self.receipt_sha = hashlib.sha256(raw).hexdigest()
        self.artifact = {'id': 400, 'run_id': 100, 'name': 'final-app-collection-x86_64', 'expired': False, 'size_in_bytes': len(self.archive) + len(raw) + 2}
        zipped = io.BytesIO()
        with zipfile.ZipFile(zipped, 'w') as archive:
            archive.writestr('tilo-apps-x86_64.tar.gz', self.archive)
            archive.writestr('final-publication-receipt.json', raw)
            archive.writestr('assembly-provenance.json', '{}')
        self.zip = zipped.getvalue()
        self.log = (f"2026-09-19T10:00:00.123Z TILO_COORDINATOR_FINAL {self.intent['id']} 2 x86_64 {self.receipt_sha} {self.receipt['collection']['sha256']} {len(self.archive)}\n").encode()
        owner = self
        class API:
            policy = owner.platform_policy
            @property
            def policy_sha(self):
                return product.core.receipt_key(self.policy)
            job_reads = 0
            def qualify_receipt(self, desired):
                assert desired == owner.intent['desired']
            def run(self, run_id):
                return copy.deepcopy(owner.run)
            def call(self, path):
                if path.endswith('/jobs'):
                    self.job_reads += 1
                    result = copy.deepcopy(owner.job)
                    if owner.stale and self.job_reads > 1:
                        result.update(attempt=3, task_id=301, handle='new-task')
                    return [result]
                return [copy.deepcopy(owner.artifact)]
            def raw_api(self, path, maximum):
                assert path == 'repos/tilo/tilo/actions/jobs/200/logs'
                return owner.log
        class Read:
            def get(self, path, output):
                assert path == '/actions/artifacts/400/zip'
                output.write_bytes(owner.zip)
        self.stale = False
        self.api = API()
        self.callback = accept.FinalArtifactAcceptance(self.api, Read(), self.policy, self.policy_sha, work_parent=self.root)

    def invoke(self, replay=None):
        with patch.object(accept.bundle, 'fetch'), patch.object(accept.final, 'verify', side_effect=replay or (lambda *a: copy.deepcopy(self.receipt))):
            return self.callback(self.intent, copy.deepcopy(self.run))

    def test_acceptance_requires_current_complete_canonically_replayed_evidence(self):
        with self.subTest(phase='success requires current log digest and canonical replay'):
            self.doCleanups()
            self.setUp()
            result = self.invoke()
            self.assertEqual(result['receipts'], {'x86_64': self.receipt_sha})
            self.assertEqual(result['qualifier']['task_id'], 300)
            self.assertEqual(result['qualifier']['handle'], 'current-task-handle')
            self.assertEqual(result['artifacts'], {'x86_64': 400})
            self.assertEqual(self.api.job_reads, 2)
        with self.subTest(phase='payload scratch is released between architectures'):
            self.doCleanups()
            self.setUp()
            receipts, archives, artifacts, markers = {}, {}, [], []
            for number, architecture in enumerate(('x86_64', 'aarch64'), 400):
                receipt = copy.deepcopy(self.receipt)
                receipt['architecture'] = architecture
                for app in receipt['packages']:
                    app['file'] = app['file'].replace('x86_64', architecture)
                    app['isa'] = 'v3' if architecture == 'x86_64' else 'armv8-a'
                raw = json.dumps(receipt).encode()
                digest = hashlib.sha256(raw).hexdigest()
                zipped = io.BytesIO()
                with zipfile.ZipFile(zipped, 'w') as archive:
                    archive.writestr(f'tilo-apps-{architecture}.tar.gz', self.archive)
                    archive.writestr('final-publication-receipt.json', raw)
                    archive.writestr('assembly-provenance.json', '{}')
                receipts[architecture], archives[number] = receipt, zipped.getvalue()
                artifacts.append(dict(self.artifact, id=number, name=f'final-app-collection-{architecture}',
                                      size_in_bytes=len(archives[number])))
                markers.append(f"TILO_COORDINATOR_FINAL {self.intent['id']} 2 {architecture} {digest} "
                               f"{receipt['collection']['sha256']} {len(self.archive)}\n")
            self.intent['desired']['architectures'] = list(receipts)
            self.run['event_payload'] = json.dumps({'ref': self.run['commit_sha'], 'inputs': accept.dispatch_inputs(self.intent)})
            self.log = ''.join(markers).encode()
            call = self.api.call
            self.api.call = lambda path: copy.deepcopy(artifacts) if '/artifacts?' in path else call(path)
            previous = []

            def download(path, output):
                if previous:
                    self.assertFalse(previous[-1].exists(), 'previous payload retained during next download')
                output.write_bytes(archives[int(path.split('/')[-2])])

            def replay(tree, *args):
                architecture = args[-1]
                archive = tree / f'tilo-apps-{architecture}.tar.gz'
                self.assertTrue(archive.samefile(tree.parent / 'payload' / archive.name))
                previous.append(tree.parent)
                return copy.deepcopy(receipts[architecture])

            self.callback.read.get = download
            result = self.invoke(replay)
            self.assertEqual(set(result['receipts']), set(receipts))
            self.assertEqual(len(previous), 2)
            self.assertTrue(all(not path.exists() for path in previous))
        with self.subTest(phase='stale attempt or log is rejected'):
            self.doCleanups()
            self.setUp()
            self.stale = True
            with self.assertRaisesRegex(ValueError, 'changed during'):
                self.invoke()
            self.stale = False
            self.log = self.log.replace(b' 2 x86_64 ', b' 1 x86_64 ')
            with self.assertRaisesRegex(ValueError, 'digest markers'):
                self.invoke()
        with self.subTest(phase='swapped artifact cannot authenticate itself with provenance'):
            self.doCleanups()
            self.setUp()
            self.receipt['source_sha'] = 'd' * 40
            raw = json.dumps(self.receipt).encode()
            swapped = io.BytesIO()
            with zipfile.ZipFile(swapped, 'w') as archive:
                archive.writestr('tilo-apps-x86_64.tar.gz', self.archive)
                archive.writestr('final-publication-receipt.json', raw)
                archive.writestr('assembly-provenance.json', json.dumps({'source_sha': 'a' * 40, 'run_id': 100}))
            self.zip = swapped.getvalue()
            with self.assertRaisesRegex(ValueError, 'trusted digest mismatch'):
                self.invoke()
        with self.subTest(phase='partial architecture coverage rejected'):
            self.doCleanups()
            self.setUp()
            self.intent['desired']['architectures'].append('aarch64')
            self.run['event_payload'] = json.dumps({'ref': 'a' * 40, 'inputs': accept.dispatch_inputs(self.intent)})
            with self.assertRaisesRegex(ValueError, 'digest markers'):
                self.invoke()
        with self.subTest(phase='canonical failure and changed verified payload rejected'):
            self.doCleanups()
            self.setUp()
            def failed(*args):
                raise ValueError('canonical signature failed')
            with self.assertRaisesRegex(ValueError, 'canonical signature failed'):
                self.invoke(failed)
            def changed(*args):
                receipt = copy.deepcopy(self.receipt)
                receipt['packages'][0]['sha256'] = 'f' * 64
                return receipt
            with self.assertRaisesRegex(ValueError, 'canonical replay differs'):
                self.invoke(changed)
        with self.subTest(phase='final policy mismatch and rotation cannot advance baseline'):
            self.doCleanups()
            self.setUp()
            self.platform_policy['final_receipt_policy_sha256'] = 'f' * 64
            with self.assertRaisesRegex(ValueError, 'not bound'):
                self.invoke()
            self.platform_policy['final_receipt_policy_sha256'] = self.policy_sha
            def rotate(*args):
                self.platform_policy['final_receipt_policy_sha256'] = 'f' * 64
                return copy.deepcopy(self.receipt)
            with self.assertRaisesRegex(ValueError, 'binding changed'):
                self.invoke(rotate)
        with self.subTest(phase='wrong dispatch and foreign run artifact rejected'):
            self.doCleanups()
            self.setUp()
            self.run['event_payload'] = json.dumps({'ref': 'a' * 40, 'inputs': {}})
            with self.assertRaisesRegex(ValueError, 'trusted dispatch'):
                self.invoke()
            self.run['event_payload'] = json.dumps({'ref': 'a' * 40, 'inputs': accept.dispatch_inputs(self.intent)})
            self.artifact['run_id'] = 999
            with self.assertRaisesRegex(ValueError, 'artifact metadata'):
                self.invoke()

    def test_product_api_and_qualifier_use_bounded_logs_and_mark_only_after_verify(self):
        with self.subTest(phase='actual product api wires bounded log transport'):
            self.doCleanups()
            self.setUp()
            root = Path(product.__file__).resolve().parents[1]
            trusted = {path: (root / path).read_bytes() for path in product.REQUIRED_PATHS}
            self.platform_policy['trusted_files'] = {path: hashlib.sha256(raw).hexdigest() for path, raw in trusted.items()}
            self.intent['desired']['policy_sha256'] = product.core.receipt_key(self.platform_policy)
            self.run['event_payload'] = json.dumps({'ref': 'a' * 40, 'inputs': accept.dispatch_inputs(self.intent)})
            actual = product.ProductForgejo(self.platform_policy)
            def request(path):
                if path == 'repos/tilo/tilo':
                    return {'id': 23, 'full_name': 'tilo/tilo'}
                if '/contents/' in path:
                    source = path.split('/contents/', 1)[1].split('?', 1)[0]
                    if source == '':
                        return [{'name': '.cargo', 'type': 'dir'}]
                    if source == '.cargo':
                        return [{'name': 'config.toml', 'type': 'file'}]
                    return {'type': 'file', 'content': base64.b64encode(trusted[source]).decode()}
                if path.endswith('/runs/100'):
                    return copy.deepcopy(self.run)
                return self.api.call(path)
            self.callback.api = actual
            with patch.object(actual, 'call', side_effect=request), patch.object(product, 'bounded_output', return_value=self.log) as network:
                result = self.invoke()
            self.assertEqual(result['receipts']['x86_64'], self.receipt_sha)
            network.assert_called_once_with(['fgj', '--hostname', 'code.tilo.so', 'api',
                                            'repos/tilo/tilo/actions/jobs/200/logs'], 8 * 1024**2)
        with self.subTest(phase='qualifier emits marker only after actual verify returns'):
            self.doCleanups()
            self.setUp()
            plan = self.root / 'plan.json'
            plan.write_text('{"catalog": []}')
            output = self.root / 'output'
            output.mkdir()
            args = ['assembler', '--built', str(self.root), '--plan', str(plan), '--output', str(output),
                    '--work', str(self.root / 'work'), '--pkg-binary', 'pkg', '--runtime-binary', 'runtime',
                    '--trust-policy', str(self.policy), '--source-sha', 'a' * 40,
                    '--builder-image', self.intent['desired']['builder_image'], '--collection', 'default',
                    '--architecture', 'x86_64', '--trust-policy-sha256', self.policy_sha,
                    '--coordinator-policy-sha256', self.intent['desired']['policy_sha256'],
                    '--intent', self.intent['id'], '--run-id', '100', '--source-epoch', '0',
                    '--qualifier-attempt', '2', '--selected-apps', '']
            def success(*unused):
                (output / 'final-publication-receipt.json').write_text(json.dumps(self.receipt))
                return self.receipt
            for failed in (True, False):
                args[args.index('--work') + 1] = str(self.root / ('failure-work' if failed else 'success-work'))
                log = io.StringIO()
                with patch.object(sys, 'argv', args), patch.object(assembler.final, 'preflight', return_value=({}, None)), \
                     patch.object(assembler.subprocess, 'run'), patch.object(assembler.final, 'verify',
                        side_effect=ValueError('canonical failure') if failed else success), redirect_stdout(log):
                    if failed:
                        with self.assertRaisesRegex(ValueError, 'canonical failure'):
                            assembler.main()
                    else:
                        assembler.main()
                if failed:
                    self.assertNotIn('TILO_COORDINATOR_FINAL', log.getvalue())
                else:
                    bound = accept.markers(log.getvalue().encode(), self.intent['id'], 2, ['x86_64'])
                    self.assertEqual(bound['x86_64'][0], self.receipt_sha)


if __name__ == '__main__':
    unittest.main()

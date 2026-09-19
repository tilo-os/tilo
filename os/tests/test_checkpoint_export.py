import json
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import checkpoint_export as diagnostics
import signing_spool as spool

WRAPPER = diagnostics.WRAPPER


class DiagnosticTests(unittest.TestCase):
    def invoke(self, source):
        result = subprocess.run([sys.executable, '-B', '-c', WRAPPER, source, '{}'],
                                capture_output=True, check=True, timeout=10)
        self.assertEqual(result.stderr, b'')
        return result.stdout, json.loads(result.stdout)

    def test_exception_exposes_only_fixed_locations_not_values_or_artifact_output(self):
        raw, result = self.invoke("import sys\nsys.stdout.buffer.write(b'ARTIFACT')\nraise ValueError('SENSITIVE ERROR VALUE')")
        self.assertFalse(result['validated'])
        self.assertEqual(result['exception_type'], 'ValueError')
        self.assertEqual(result['locations'][-1], {'file': '<checkpoint-export>', 'line': 3, 'function': '<module>'})
        self.assertNotIn(b'ARTIFACT', raw)
        self.assertNotIn(b'SENSITIVE', raw)

    def test_success_discards_frame_and_reports_only_public_input_identity(self):
        raw, result = self.invoke("import sys\npurpose='target-uki'\nresult={'input_sha256':'a'*64,'input_bytes':256}\nsys.stdout.buffer.write(b'PRIVATE-LARGE-PAYLOAD')\nsys.stdout.buffer.flush()")
        self.assertEqual(result, {'validated': True, 'purpose': 'target-uki', 'input_sha256': 'a' * 64, 'input_bytes': 256,
                                 'checkpoint_metadata': {'input_sha256': 'a' * 64, 'input_bytes': 256}})
        self.assertNotIn(b'PAYLOAD', raw)


class CheckpointPersistenceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.operation = self.directory(self.root / 'operation')
        self.run = self.directory(self.operation / 'run-1-attempt-1')
        self.checkpoints = self.directory(self.run / 'checkpoints')
        self.spool = types.SimpleNamespace(
            MAX_INPUT=spool.MAX_INPUT, MAX_JSON=spool.MAX_JSON,
            canonical=spool.canonical,
            digest=spool.digest, context=spool.context,
            private_directory=lambda path: Path(path),
            document=lambda path: json.loads(Path(path).read_bytes()),
            snapshot=lambda path, limit: self.snapshot(path, limit),
            write_new=lambda path, data: self.write_new(path, data),
            sync_directory=lambda path: None,
            request_metadata=spool.request_metadata,
        )
        request = b'{}'
        self.write(self.operation / 'request.json', request)
        self.write(self.operation / 'operation.json', {
            'request_sha256': spool.digest(request),
        })

    @staticmethod
    def directory(path):
        path.mkdir(mode=0o700)
        path.chmod(0o700)
        return path

    @staticmethod
    def snapshot(path, limit):
        data = Path(path).read_bytes()
        if not 0 < len(data) <= limit:
            raise ValueError('bounded test file required')
        return data

    @staticmethod
    def write_new(path, data):
        with Path(path).open('xb') as output:
            output.write(data)

    def write(self, path, value):
        self.spool.write_new(
            path, value if isinstance(value, bytes) else self.spool.canonical(value))

    def context(self):
        return {
            'schema': 1, 'job_id': 'job-1', 'pod_uid': 'pod-1',
            'container_id': 'container-1', 'image_id': 'sha256:' + '1' * 64,
            'revision': '2' * 40, 'architecture': 'aarch64',
            'manifest_sha256': '3' * 64, 'target': 'lenovo-83ed',
        }

    def test_export_publishes_only_authenticated_metadata(self):
        metadata = {'input_sha256': '4' * 64, 'input_bytes': 257}
        binding = {'context_sha256': '5' * 64}
        inspected = ({'validated': True, 'local_metadata_validated': True},
                     metadata, self.spool, binding)

        with mock.patch.object(diagnostics, '_inspect', return_value=inspected):
            result = diagnostics.export_metadata(
                self.operation, self.run, 'live-uki', self.spool)

        destination = self.checkpoints / 'live-uki'
        self.assertEqual(set(path.name for path in destination.iterdir()), {'receipt.json'})
        self.assertEqual(self.spool.document(destination / 'receipt.json'), {
            'input_sha256': '4' * 64, 'input_bytes': 257,
            'transport_context_sha256': '5' * 64,
        })
        self.assertEqual(result['directory'], str(destination))
        self.assertTrue(result['metadata_only'])
        self.assertFalse(result['signing_authorized'])
        self.assertFalse((self.checkpoints / '.live-uki.ready').exists())

        with mock.patch.object(diagnostics, '_inspect', return_value=inspected):
            with self.assertRaisesRegex(ValueError, 'fresh checkpoint output required'):
                diagnostics.export_metadata(self.operation, self.run, 'live-uki', self.spool)

    def prepare_materialization(self, payload=b'exact unsigned UKI'):
        context = self.context()
        context_sha256 = spool.context(context)
        checkpoint = self.directory(self.checkpoints / 'target-uki')
        self.write(checkpoint / 'receipt.json', {
            'input_sha256': spool.digest(payload), 'input_bytes': len(payload),
            'transport_context_sha256': context_sha256,
            'purpose': 'target-uki', 'signing_authorized': False,
            'private_keys_used': False,
        })
        self.write(self.run / 'binding.json', {'context_sha256': context_sha256})
        self.write(self.run / 'signing-context.json', context)
        received = self.directory(self.run / 'received')
        request = self.directory(received / ('a' * 32))
        request_metadata = {
            'schema': 1, 'request_id': 'a' * 32,
            'context_sha256': context_sha256, 'operation': 'sign-efi',
            'input_sha256': spool.digest(payload), 'input_bytes': len(payload),
        }
        self.write(request / 'request.json', request_metadata)
        self.write(request / 'input', payload)
        return checkpoint, request, request_metadata

    def test_materialize_copies_exact_admitted_request_and_records_identity(self):
        checkpoint, request, request_metadata = self.prepare_materialization()

        result = diagnostics.materialize(
            self.operation, self.run, 'target-uki', request, self.spool)

        self.assertEqual((checkpoint / 'input.efi').read_bytes(), b'exact unsigned UKI')
        record = self.spool.document(checkpoint / 'materialization.json')
        self.assertEqual(record, {
            'schema': 1, 'purpose': 'target-uki', 'request_id': 'a' * 32,
            'context_sha256': request_metadata['context_sha256'],
            'input_sha256': request_metadata['input_sha256'],
            'input_bytes': request_metadata['input_bytes'],
            'signing_authorized': False,
        })
        self.assertTrue(result['materialized'])
        self.assertFalse(result['signing_authorized'])

    def test_materialize_rejects_request_that_differs_from_checkpoint(self):
        checkpoint, request, request_metadata = self.prepare_materialization()
        request_metadata['input_sha256'] = '9' * 64
        (request / 'request.json').unlink()
        self.write(request / 'request.json', request_metadata)

        with self.assertRaisesRegex(ValueError, 'fetched request differs'):
            diagnostics.materialize(
                self.operation, self.run, 'target-uki', request, self.spool)

        self.assertEqual(set(path.name for path in checkpoint.iterdir()), {'receipt.json'})

    def test_materialize_rejects_receipt_from_another_transport_context(self):
        checkpoint, request, _ = self.prepare_materialization()
        receipt = self.spool.document(checkpoint / 'receipt.json')
        receipt['transport_context_sha256'] = '8' * 64
        (checkpoint / 'receipt.json').unlink()
        self.write(checkpoint / 'receipt.json', receipt)

        with self.assertRaisesRegex(ValueError, 'fetched request differs'):
            diagnostics.materialize(
                self.operation, self.run, 'target-uki', request, self.spool)

        self.assertEqual(set(path.name for path in checkpoint.iterdir()), {'receipt.json'})

    def test_materialize_rejects_request_id_that_differs_from_directory(self):
        checkpoint, request, request_metadata = self.prepare_materialization()
        request_metadata['request_id'] = 'b' * 32
        (request / 'request.json').unlink()
        self.write(request / 'request.json', request_metadata)

        with self.assertRaisesRegex(ValueError, 'fetched request differs'):
            diagnostics.materialize(
                self.operation, self.run, 'target-uki', request, self.spool)

        self.assertEqual(set(path.name for path in checkpoint.iterdir()), {'receipt.json'})

    def test_materialize_rejects_request_directory_outside_receive_storage(self):
        checkpoint, request, _ = self.prepare_materialization()
        outside = self.directory(self.run / ('c' * 32))

        with self.assertRaisesRegex(ValueError, 'outside admitted receive storage'):
            diagnostics.materialize(
                self.operation, self.run, 'target-uki', outside, self.spool)

        self.assertEqual(set(path.name for path in checkpoint.iterdir()), {'receipt.json'})

    def prepare_recompute(self):
        context = self.context()
        context_sha256 = spool.context(context)
        self.write(self.run / 'binding.json', {'context_sha256': context_sha256})
        self.write(self.run / 'signing-context.json', context)
        chain = [{'name': 'checkpoint', 'sha256': '6' * 64, 'receipt': {'stage': 'reseal'}}]
        values = {}
        for purpose, payload in (('live-uki', b'live unsigned UKI'),
                                 ('target-uki', b'target unsigned UKI')):
            checkpoint = self.directory(self.checkpoints / purpose)
            value = {
                'purpose': purpose, 'input_sha256': spool.digest(payload),
                'input_bytes': len(payload), 'checkpoint_chain': chain,
                'transport_context_sha256': context_sha256,
            }
            self.write(checkpoint / 'receipt.json', value)
            values[purpose] = (checkpoint, payload, value)
        live, payload, value = values['live-uki']
        request_id = 'a' * 32
        self.write(live / 'input.efi', payload)
        self.write(live / 'materialization.json', {
            'schema': 1, 'purpose': 'live-uki', 'request_id': request_id,
            'context_sha256': context_sha256,
            'input_sha256': value['input_sha256'], 'input_bytes': value['input_bytes'],
            'signing_authorized': False,
        })
        received = self.directory(self.run / 'received')
        fetched = self.directory(received / request_id)
        self.write(fetched / 'request.json', {
            'schema': 1, 'request_id': request_id, 'operation': 'sign-efi',
            'context_sha256': context_sha256,
            'input_sha256': value['input_sha256'], 'input_bytes': value['input_bytes'],
        })
        exporter = types.SimpleNamespace(
            validate_metadata=lambda value, request, binding, purpose, request_sha, local_spool:
                value if value['purpose'] == purpose else (_ for _ in ()).throw(ValueError()))
        return values, exporter, {'context_sha256': context_sha256}

    def test_recompute_admits_materialized_live_and_metadata_only_target(self):
        values, exporter, binding = self.prepare_recompute()
        live, target = diagnostics.admit_composefs_recompute(
            self.run, {}, binding, '7' * 64, exporter, self.spool)
        self.assertEqual(live, values['live-uki'][2])
        self.assertEqual(target, values['target-uki'][2])

    def test_recompute_rejects_unmaterialized_live(self):
        values, exporter, binding = self.prepare_recompute()
        (values['live-uki'][0] / 'input.efi').unlink()
        with self.assertRaises((OSError, ValueError)):
            diagnostics.admit_composefs_recompute(
                self.run, {}, binding, '7' * 64, exporter, self.spool)

    def test_recompute_rejects_materialized_target_before_sequential_request(self):
        values, exporter, binding = self.prepare_recompute()
        self.write(values['target-uki'][0] / 'input.efi', values['target-uki'][1])
        with self.assertRaisesRegex(ValueError, 'metadata-only'):
            diagnostics.admit_composefs_recompute(
                self.run, {}, binding, '7' * 64, exporter, self.spool)

    def test_recompute_rejects_receive_context_mismatch(self):
        values, exporter, binding = self.prepare_recompute()
        request = self.run / 'received' / ('a' * 32) / 'request.json'
        value = self.spool.document(request)
        value['context_sha256'] = '8' * 64
        request.unlink()
        self.write(request, value)
        with self.assertRaisesRegex(ValueError, 'receive metadata differs'):
            diagnostics.admit_composefs_recompute(
                self.run, {}, binding, '7' * 64, exporter, self.spool)

    def test_recompute_admits_explicit_legacy_checkpoint_layout(self):
        values, exporter, binding = self.prepare_recompute()
        for purpose in ('live-uki', 'target-uki'):
            source = self.checkpoints / purpose
            source.rename(self.run / ('checkpoint-' + purpose))
        self.checkpoints.rmdir()
        live, target = diagnostics.admit_composefs_recompute(
            self.run, {}, binding, '7' * 64, exporter, self.spool, legacy=True)
        self.assertEqual(live, values['live-uki'][2])
        self.assertEqual(target, values['target-uki'][2])


if __name__ == '__main__':
    unittest.main()

"""Operator admission tests; no CI workers, registry pulls, or private keys."""
import copy
import importlib.util
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
from test_finalization_driver import request
import checkpoint_export as checkpoint
SPEC = importlib.util.spec_from_file_location('operator_cli', Path(__file__).resolve().parents[1] / 'dev/finalization-operator.py')
o = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(o)
s = o.spool


def context(job='old'):
    return dict(schema=1, job_id=job, pod_uid='pod-' + job, container_id='1' * 64,
                image_id='sha256:' + '2' * 64, revision='a' * 40,
                architecture='aarch64', manifest_sha256='e' * 64,
                target='code.podesta.ai/qos/tilo-development:new-aarch64')


def metadata(ctx, nonce, data):
    return dict(schema=1, request_id=nonce, context_sha256=s.context(ctx),
                operation='sign-efi', input_sha256=s.digest(data), input_bytes=len(data))


class OperatorTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir='/root', prefix='tilo-operator-test-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.q = request()
        self.receipt = s.canonical({'schema': 1, 'purpose': 'tilo-retained-development-raw-rootfs',
                                    'hardware_profile': None})
        self.q['receipt_sha256'] = s.digest(self.receipt)
        self.inputs = self.root / 'inputs'
        self.inputs.mkdir(mode=0o700)
        self.put(self.inputs / 'request.json', s.canonical(self.q))
        self.put(self.inputs / 'receipt.json', self.receipt)
        for folder in ('old', 'new'):
            (self.inputs / folder).mkdir(mode=0o700)
            for name in o.stage.PUBLIC:
                self.put(self.inputs / folder / name, b'fixture ' + name.encode())
        self.archive()
        self.args = SimpleNamespace(operation=self.root / 'operation',
            request=self.inputs / 'request.json', receipt=self.inputs / 'receipt.json',
            original_public=self.inputs / 'old', new_public=self.inputs / 'new',
            helper_archive=self.inputs / 'helpers.tar', archive_sha256=s.digest(self.archive_raw),
            tools_image_id='sha256:' + '2' * 64, hardware_profile='generic',
            initial_free_bytes=2**30, forgejo_host='code.podesta.ai', repository='qos/qos',
            dispatch_ref='feature/device-support')

    @staticmethod
    def put(path, data):
        path.write_bytes(data)
        path.chmod(0o600)

    def archive(self, extra=()):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode='w', format=tarfile.PAX_FORMAT,
                          pax_headers={'comment': self.q['helpers_revision']}) as tar:
            for name in (*o.stage.CODE, *extra):
                item = tarfile.TarInfo('os/dev/' + name)
                data = b'# fixture ' + name.encode()
                item.size = len(data)
                tar.addfile(item, io.BytesIO(data))
        self.archive_raw = output.getvalue()
        self.put(self.inputs / 'helpers.tar', self.archive_raw)

    def init(self):
        o.initialize(self.args)
        return o.load(self.args.operation)

    def event(self, c):
        run = dict(id=23, index_in_repo=7, workflow_id='development-finalize.yml',
                   trigger_event='workflow_dispatch', commit_sha=c['helper_revision'], status='running',
                   event_payload=json.dumps({'ref': 'refs/heads/' + c['dispatch_ref'], 'inputs': {
                       'request': s.canonical(self.q).decode(), 'request_sha256': c['request_sha256'],
                       'helpers_revision': c['helper_revision'], 'restore_run_id': '',
                       'restore_run_attempt': '', 'restore_receipt_sha256': '',
                       'restore_stage': '5'}}))
        job = dict(id=9, name='finalize', run_id=23, attempt=2, task_id=42, status='running')
        return run, job

    def test_initialize_is_exclusive_and_every_snapshot_is_pinned(self):
        c, q = self.init()
        self.assertEqual(q, self.q)
        self.assertEqual(c['hardware_profile'], 'generic')
        with self.assertRaises(FileExistsError):
            o.initialize(self.args)
        self.put(self.args.operation / 'new-public/efi-signing.crt', b'changed')
        with self.assertRaisesRegex(ValueError, 'snapshot changed'):
            o.load(self.args.operation)

    def test_initialized_generic_operation_checkpoint_handoff_and_materialization(self):
        config, _ = self.init()
        directory = o.run_path(self.args.operation, 23, 2)
        directory.mkdir(mode=0o700)
        for name in ('received', 'responses', 'checkpoints'):
            (directory / name).mkdir(mode=0o700)
        ctx = context('qos/qos/23/2/42')
        binding = dict(schema=1, namespace='agent-sandboxes', pod='fixture', container='dind',
            pod_uid=ctx['pod_uid'], container_id='containerd://fixture', image='fixture:image',
            image_id='sha256:' + '3' * 64, restart_count=0, pod_spec_sha256='4' * 64,
            context_sha256=s.context(ctx), endpoint=str(o.stage.BRIDGE / 'signing-spool-endpoint.py'),
            docker=dict(id=ctx['container_id'], image_id=ctx['image_id'],
                        started_at='fixture', restart_count=0))
        self.put(directory / 'binding.json', s.canonical(binding))
        self.put(directory / 'signing-context.json', s.canonical(ctx))

        handoff = o.handoff(self.args.operation, directory, config)
        self.assertEqual(handoff['checkpoints'], str(directory / 'checkpoints'))
        self.assertTrue(Path(handoff['checkpoints']).is_dir())
        with self.assertRaisesRegex(ValueError, 'no authenticated checkpoint exporter'):
            checkpoint.export_metadata(
                self.args.operation, directory, 'target-uki', s)
        self.assertEqual(list((directory / 'checkpoints').iterdir()), [])

        payload = b'generic operation unsigned UKI fixture'
        checkpoint_metadata = {'schema': 1, 'purpose': 'target-uki',
                               'input_sha256': s.digest(payload), 'input_bytes': len(payload),
                               'signing_authorized': False, 'private_keys_used': False}
        inspected = ({'validated': True, 'local_metadata_validated': True},
                     checkpoint_metadata, s, binding)
        with patch.object(checkpoint, '_inspect', return_value=inspected):
            exported = checkpoint.export_metadata(
                self.args.operation, directory, 'target-uki', s)
        self.assertEqual(exported['directory'], str(directory / 'checkpoints/target-uki'))

        nonce = 'b' * 32
        received = directory / 'received' / nonce
        received.mkdir(mode=0o700)
        request_metadata = metadata(ctx, nonce, payload)
        self.put(received / 'request.json', s.canonical(request_metadata))
        self.put(received / 'input', payload)
        result = checkpoint.materialize(
            self.args.operation, directory, 'target-uki', received, s)
        self.assertTrue(result['materialized'])
        self.assertEqual(
            (directory / 'checkpoints/target-uki/input.efi').read_bytes(), payload)

    def test_initialize_binds_operation_profile_to_retained_receipt(self):
        self.args.hardware_profile = 'lenovo-83ed'
        with self.assertRaisesRegex(ValueError, 'profile differs'):
            o.initialize(self.args)
        self.assertFalse(self.args.operation.exists())

        retained = json.loads(self.receipt)
        retained['hardware_profile'] = 'lenovo-83ed'
        self.receipt = s.canonical(retained)
        self.put(self.inputs / 'receipt.json', self.receipt)
        self.q['receipt_sha256'] = s.digest(self.receipt)
        self.put(self.inputs / 'request.json', s.canonical(self.q))
        c, _ = self.init()
        self.assertEqual(c['hardware_profile'], 'lenovo-83ed')

    def test_initialize_rejects_missing_or_invalid_retained_profile(self):
        for receipt in ({'schema': 1, 'purpose': 'tilo-retained-development-raw-rootfs'},
                        {'schema': 1, 'purpose': 'tilo-retained-development-raw-rootfs',
                         'hardware_profile': '../board'}):
            raw = s.canonical(receipt)
            self.put(self.inputs / 'receipt.json', raw)
            self.q['receipt_sha256'] = s.digest(raw)
            self.put(self.inputs / 'request.json', s.canonical(self.q))
            with self.subTest(receipt=receipt), self.assertRaises(ValueError):
                o.initialize(self.args)
            self.assertFalse(self.args.operation.exists())

    def test_local_operator_change_does_not_silently_change_operation(self):
        self.init()
        with patch.object(o, 'operator_identity', return_value='0' * 64), self.assertRaisesRegex(ValueError, 'implementation changed'):
            o.load(self.args.operation)

    def test_archive_traversal_and_wrong_archive_are_rejected_before_operation_creation(self):
        self.archive(('../../escape',))
        self.args.archive_sha256 = s.digest(self.archive_raw)
        with self.assertRaises(ValueError):
            o.initialize(self.args)
        self.assertFalse(self.args.operation.exists())
        self.args.archive_sha256 = '0' * 64
        with self.assertRaisesRegex(ValueError, 'archive hash differs'):
            o.initialize(self.args)

    def test_event_admission_rejects_wrong_ref_commit_input_bytes_and_attempt(self):
        c, _ = self.init()
        run, job = self.event(c)
        raw = s.canonical(self.q)
        self.assertEqual(o.admit_job(c, raw, run, [job], 2), job)
        for change in ('ref', 'commit', 'bytes', 'attempt', 'ambiguous'):
            r, j = copy.deepcopy(run), copy.deepcopy(job)
            jobs = [j]
            event = json.loads(r['event_payload'])
            if change == 'ref': event['ref'] = 'refs/heads/main'
            if change == 'commit': r['commit_sha'] = '0' * 40
            if change == 'bytes': event['inputs']['request'] += '\n'
            if change == 'attempt': j['attempt'] = 1
            if change == 'ambiguous': jobs.append(copy.deepcopy(j))
            r['event_payload'] = json.dumps(event)
            with self.subTest(change=change), self.assertRaises(ValueError):
                o.admit_job(c, raw, r, jobs, 2)

    def test_event_admission_accepts_exact_restore_identity_and_rejects_partial_or_implicit_restore(self):
        c, _ = self.init()
        run, job = self.event(c)
        raw = s.canonical(self.q)
        event = json.loads(run['event_payload'])
        event['inputs'].update(restore_run_id='41', restore_run_attempt='3',
                               restore_receipt_sha256='a' * 64, restore_stage='2')
        run['event_payload'] = json.dumps(event)
        self.assertEqual(o.admit_job(c, raw, run, [job], 2), job)
        mutations = {
            'partial': {'restore_run_attempt': ''},
            'zero-run': {'restore_run_id': '0'},
            'bad-digest': {'restore_receipt_sha256': 'A' * 64},
            'bad-stage': {'restore_stage': '3'},
            'extra': {'unexpected': ''},
        }
        for label, mutation in mutations.items():
            changed = copy.deepcopy(event)
            changed['inputs'].update(mutation)
            candidate = copy.deepcopy(run)
            candidate['event_payload'] = json.dumps(changed)
            with self.subTest(label=label), self.assertRaises(ValueError):
                o.admit_job(c, raw, candidate, [job], 2)

    def test_event_admission_normalizes_only_omitted_empty_restore_identity(self):
        c, _ = self.init()
        run, job = self.event(c)
        raw = s.canonical(self.q)
        event = json.loads(run['event_payload'])
        restore_fields = ('restore_run_id', 'restore_run_attempt', 'restore_receipt_sha256')
        for omitted in (restore_fields, restore_fields[:1], restore_fields[1:]):
            changed = copy.deepcopy(event)
            for field in omitted:
                changed['inputs'].pop(field)
            candidate = copy.deepcopy(run)
            candidate['event_payload'] = json.dumps(changed)
            with self.subTest(omitted=omitted):
                self.assertEqual(o.admit_job(c, raw, candidate, [job], 2), job)

        hostile = {
            'nonempty-run-only': {'restore_run_id': '41'},
            'nonempty-attempt-only': {'restore_run_attempt': '3'},
            'nonempty-receipt-only': {'restore_receipt_sha256': 'a' * 64},
        }
        for label, supplied in hostile.items():
            changed = copy.deepcopy(event)
            for field in restore_fields:
                changed['inputs'].pop(field)
            changed['inputs'].update(supplied)
            candidate = copy.deepcopy(run)
            candidate['event_payload'] = json.dumps(changed)
            with self.subTest(label=label), self.assertRaisesRegex(
                    ValueError, 'dispatch restore inputs differ'):
                o.admit_job(c, raw, candidate, [job], 2)

    def test_event_admission_rejects_omitted_or_untyped_restore_stage(self):
        c, _ = self.init()
        run, job = self.event(c)
        raw = s.canonical(self.q)
        event = json.loads(run['event_payload'])
        for value in (None, 5):
            changed = copy.deepcopy(event)
            if value is None:
                changed['inputs'].pop('restore_stage')
            else:
                changed['inputs']['restore_stage'] = value
            candidate = copy.deepcopy(run)
            candidate['event_payload'] = json.dumps(changed)
            with self.subTest(value=value), self.assertRaises(ValueError):
                o.admit_job(c, raw, candidate, [job], 2)

    def test_event_admission_rejects_nondefault_restore_stage_on_fresh_run(self):
        c, _ = self.init()
        run, job = self.event(c)
        event = json.loads(run['event_payload'])
        event['inputs']['restore_stage'] = '2'
        run['event_payload'] = json.dumps(event)
        with self.assertRaisesRegex(ValueError, 'fresh dispatch restore defaults differ'):
            o.admit_job(c, s.canonical(self.q), run, [job], 2)

    def test_status_decodes_transport_bytes_and_rejects_changed_task(self):
        c, _ = self.init()
        run, job = self.event(c)
        directory = o.run_path(self.args.operation, 23, 2)
        directory.mkdir(mode=0o700)
        self.put(directory / 'job.json', s.canonical(job))
        self.put(directory / 'staging-receipt.json', b'{}')
        client = Mock()
        client.transfer.return_value = s.canonical({'requests': ['a' * 32]})
        with patch.object(o, 'actual_job', return_value=(run, job)), patch.object(o, 'client_for', return_value=client):
            self.assertEqual(o.status(self.args.operation, c, 23, 2)['requests'], ['a' * 32])
            client.transfer.side_effect = TimeoutError('transport lost')
            with self.assertRaises(TimeoutError):
                o.status(self.args.operation, c, 23, 2)
            job['task_id'] += 1
            with self.assertRaisesRegex(ValueError, 'task changed'):
                o.status(self.args.operation, c, 23, 2)

    def test_wait_ignores_serviced_requests_and_timeout_is_observation_only(self):
        state = {'requests': ['a' * 32], 'job_status': 'running'}
        with patch.object(o, 'status', side_effect=lambda *a: copy.deepcopy(state)), patch.object(o.time, 'sleep') as sleep, patch.object(o.time, 'monotonic', side_effect=[0, 1, 2]), self.assertRaises(TimeoutError):
            o.wait_status(self.root, {}, 23, 2, 'request', 2, 1, ['a' * 32])
        sleep.assert_called_once_with(1)

    def test_service_reused_efi_sequences_exact_request_and_preserves_failures(self):
        nonce = 'b' * 32
        old_nonce = 'a' * 32
        directory = self.root / 'run-23-attempt-2'
        (directory / 'received').mkdir(parents=True)
        (directory / 'responses').mkdir()
        old_directory = self.root / 'old-run'
        observed = {'requests': [nonce], 'job_status': 'running'}
        fetched = {'request_id': nonce, 'input_sha256': '1' * 64,
                   'input_bytes': 125440, 'received': True}
        reused = {'request_id': nonce, 'output_sha256': '2' * 64,
                  'signature_reused': True, 'private_key_used': False}
        delivered = {'request_id': nonce, 'output_sha256': '2' * 64,
                     'delivered': True}
        client = Mock()
        client.fetch.return_value = fetched
        client.deliver.return_value = delivered

        with patch.object(o, 'wait_status', return_value=observed) as wait, \
                patch.object(o, 'client_for', return_value=client), \
                patch.object(checkpoint, 'materialize', return_value={
                    'request_id': nonce, 'input_sha256': '1' * 64}) as materialize, \
                patch.object(o, 'reuse_response', return_value=reused) as reuse:
            result = o.service_reused_efi(
                self.root, {}, directory, 23, 2, old_directory, old_nonce,
                'target-uki', 300, 5, ['c' * 32])

        wait.assert_called_once_with(
            self.root, {}, 23, 2, 'request', 300, 5, ['c' * 32])
        client.fetch.assert_called_once_with(nonce, directory / 'received' / nonce)
        materialize.assert_called_once_with(
            self.root, directory, 'target-uki', directory / 'received' / nonce, s)
        reuse.assert_called_once_with(
            self.root, directory, nonce, old_directory, old_nonce, 'target-uki')
        client.deliver.assert_called_once_with(directory / 'responses' / nonce)
        self.assertEqual(result, {
            'request_id': nonce, 'input_sha256': '1' * 64,
            'output_sha256': '2' * 64, 'purpose': 'target-uki', 'signature_reused': True,
            'private_key_used': False, 'delivered': True,
        })

        client.reset_mock()
        with patch.object(o, 'wait_status', return_value=observed), \
                patch.object(o, 'client_for', return_value=client), \
                patch.object(checkpoint, 'materialize', return_value={
                    'request_id': nonce, 'input_sha256': '1' * 64}), \
                patch.object(o, 'reuse_response', side_effect=ValueError('signature rejected')):
            with self.assertRaisesRegex(ValueError, 'signature rejected'):
                o.service_reused_efi(
                    self.root, {}, directory, 23, 2, old_directory, old_nonce,
                    'target-uki', 300, 5)
        client.fetch.assert_called_once()
        client.deliver.assert_not_called()

    def test_service_reused_efi_rejects_ambiguous_or_existing_state_before_fetch(self):
        directory = self.root / 'run-23-attempt-2'
        (directory / 'received').mkdir(parents=True)
        (directory / 'responses').mkdir()
        client = Mock()
        with patch.object(o, 'client_for', return_value=client):
            for requests in ([], ['a' * 32, 'b' * 32]):
                observed = {'requests': requests, 'job_status': 'running'}
                with self.subTest(requests=requests), \
                        patch.object(o, 'wait_status', return_value=observed), \
                        self.assertRaisesRegex(ValueError, 'exactly one'):
                    o.service_reused_efi(
                        self.root, {}, directory, 23, 2, self.root / 'old',
                        'c' * 32, 'live-uki', 300, 5)
            existing = directory / 'received' / ('d' * 32)
            existing.mkdir()
            observed = {'requests': ['d' * 32], 'job_status': 'running'}
            with patch.object(o, 'wait_status', return_value=observed), \
                    self.assertRaisesRegex(ValueError, 'state already exists'):
                o.service_reused_efi(
                    self.root, {}, directory, 23, 2, self.root / 'old',
                    'c' * 32, 'live-uki', 300, 5)
        client.fetch.assert_not_called()

    def test_stage_failure_preserves_reservation_and_cannot_retry(self):
        c, _ = self.init()
        directory = self.args.operation / 'run-23-attempt-2'
        directory.mkdir(mode=0o700)
        self.put(directory / 'signing-context.json', s.canonical(context()))
        with patch.object(o, 'client_for'), patch.object(o.stage, 'stage', side_effect=TimeoutError('receive lost')) as stage:
            with self.assertRaises(TimeoutError):
                o.stage_run(self.args.operation, c, directory)
            self.assertTrue((directory / 'stage-started.json').exists())
            self.assertFalse((directory / 'staging-receipt.json').exists())
            with self.assertRaisesRegex(ValueError, 'no automatic retry'):
                o.stage_run(self.args.operation, c, directory)
            stage.assert_called_once()

    def test_bound_admission_rejects_partial_changed_task_and_terminal_job(self):
        c, _ = self.init()
        run, job = self.event(c)
        directory = o.run_path(self.args.operation, 23, 2)
        directory.mkdir(mode=0o700)
        ctx = context('qos/qos/23/2/42')
        self.put(directory / 'signing-context.json', s.canonical(ctx))
        self.put(directory / 'binding.json', b'{}')
        with patch.object(o, 'actual_job', return_value=(run, job)), patch.object(o.stage, 'bound_context'):
            with self.assertRaises(FileNotFoundError):
                o.admit_bound(self.args.operation, c, 23, 2)
            self.put(directory / 'job.json', s.canonical(job))
            self.assertEqual(o.admit_bound(self.args.operation, c, 23, 2), directory)
            job['status'] = 'failure'
            with self.assertRaisesRegex(ValueError, 'not running'):
                o.admit_bound(self.args.operation, c, 23, 2)
            self.assertEqual(o.admit_bound(self.args.operation, c, 23, 2, require_running=False), directory)
            job['status'] = 'running'
            job['task_id'] += 1
            with self.assertRaisesRegex(ValueError, 'identity changed'):
                o.admit_bound(self.args.operation, c, 23, 2)

    def test_reuse_verifies_signature_before_publication_and_never_overwrites(self):
        self.init()
        old, new = context(), context('new')
        original, signed = b'unsigned target UKI', b'signed target UKI'
        directories = []
        for name, ctx, nonce in (('old', old, 'a' * 32), ('new', new, 'b' * 32)):
            directory = self.args.operation / name
            directory.mkdir(mode=0o700)
            (directory / 'received').mkdir(mode=0o700)
            (directory / 'responses').mkdir(mode=0o700)
            received = directory / 'received' / nonce
            received.mkdir(mode=0o700)
            self.put(directory / 'signing-context.json', s.canonical(ctx))
            binding = dict(schema=1, namespace='agent-sandboxes', pod='fixture', container='dind',
                pod_uid=ctx['pod_uid'], container_id='containerd://fixture', image='fixture:image',
                image_id='sha256:' + '3' * 64, restart_count=0, pod_spec_sha256='4' * 64,
                context_sha256=s.context(ctx), endpoint=str(o.stage.BRIDGE / 'signing-spool-endpoint.py'),
                docker=dict(id=ctx['container_id'], image_id=ctx['image_id'], started_at='fixture', restart_count=0))
            self.put(directory / 'binding.json', s.canonical(binding))
            request_value = metadata(ctx, nonce, original)
            self.put(received / 'request.json', s.canonical(request_value))
            self.put(received / 'input', original)
            checkpoint_dir = directory / 'checkpoints' / 'target-uki'
            checkpoint_dir.mkdir(parents=True, mode=0o700)
            self.put(checkpoint_dir / 'receipt.json', s.canonical({
                'purpose': 'target-uki', 'transport_context_sha256': s.context(ctx),
                'input_sha256': s.digest(original), 'input_bytes': len(original),
                'signing_authorized': False, 'private_keys_used': False,
            }))
            self.put(checkpoint_dir / 'materialization.json', s.canonical({
                'schema': 1, 'purpose': 'target-uki', 'request_id': nonce,
                'context_sha256': s.context(ctx), 'input_sha256': s.digest(original),
                'input_bytes': len(original), 'signing_authorized': False,
            }))
            self.put(checkpoint_dir / 'input.efi', original)
            directories.append(directory)
            if name == 'old':
                response_dir = directory / 'responses' / nonce
                response_dir.mkdir(mode=0o700)
                response = {k: request_value[k] for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256')}
                response.update(output_sha256=s.digest(signed), output_bytes=len(signed))
                self.put(response_dir / 'response.json', s.canonical(response))
                self.put(response_dir / 'output', signed)
        args = (self.args.operation, directories[1], 'b' * 32, directories[0], 'a' * 32, 'target-uki')
        destination = directories[1] / 'responses' / ('b' * 32)
        trust = {'identities': {'efi-signing.crt': '5' * 64}}
        verified = dict(original_sha256=s.digest(original), signed_sha256=s.digest(signed),
                        certificate_sha256='5' * 64, architecture='aarch64')
        current_receipt = directories[1] / 'checkpoints/target-uki/receipt.json'
        receipt = o.document(current_receipt)
        current_receipt.unlink()
        self.put(current_receipt, s.canonical(dict(receipt, input_sha256='0' * 64)))
        with patch.object(o.loader, 'verify') as verify, \
                self.assertRaisesRegex(ValueError, 'checkpoint receipt differs'):
            o.reuse_response(*args)
        verify.assert_not_called()
        current_receipt.unlink()
        self.put(current_receipt, s.canonical(receipt))
        with patch.object(o.stage.trust, 'validate', return_value=trust), patch.object(o.loader, 'verify', side_effect=ValueError('signature rejected')) as verify:
            with self.assertRaisesRegex(ValueError, 'signature rejected'):
                o.reuse_response(*args)
            self.assertFalse(destination.exists())
            verify.side_effect = None
            verify.return_value = dict(verified, signed_sha256='0' * 64)
            with self.assertRaisesRegex(ValueError, 'changed during verification'):
                o.reuse_response(*args)
            self.assertFalse(destination.exists())
            verify.return_value = verified
            self.assertFalse(o.reuse_response(*args)['private_key_used'])
            self.assertEqual((destination / 'output').read_bytes(), signed)
            self.assertEqual(o.document(destination / 'response.json')['context_sha256'], s.context(new))
            with self.assertRaises(FileExistsError):
                o.reuse_response(*args)


class LoaderReuseTests(unittest.TestCase):
    def setUp(self):
        self.old, self.new = context(), context('new')
        self.original, self.signed = b'exact selected EFI bytes', b'signed PE fixture'
        self.old_request = metadata(self.old, 'a' * 32, self.original)
        self.request = metadata(self.new, 'b' * 32, self.original)
        self.response = {k: self.old_request[k] for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256')}
        self.response.update(output_sha256=s.digest(self.signed), output_bytes=len(self.signed))

    def reuse(self, incoming=None, digest=None):
        return o.reuse_loader(self.old, self.new, self.old_request, self.request, self.response,
                             self.original, self.original if incoming is None else incoming,
                             self.signed, s.digest(self.original) if digest is None else digest)

    def test_rebinds_only_nonce_and_runtime_context(self):
        result = self.reuse()
        self.assertEqual(result, dict(self.response, request_id='b' * 32, context_sha256=s.context(self.new)))
        self.assertEqual(self.response['request_id'], 'a' * 32)

    def test_every_immutable_context_change_is_rejected_even_with_valid_request_context(self):
        for key, changed in {'revision': 'b' * 40, 'architecture': 'x86_64', 'manifest_sha256': 'f' * 64,
                             'target': 'other:target', 'image_id': 'sha256:' + '3' * 64}.items():
            with self.subTest(key=key):
                self.new = context('new')
                self.new[key] = changed
                self.request['context_sha256'] = s.context(self.new)
                with self.assertRaisesRegex(ValueError, 'immutable context'):
                    self.reuse()

    def test_same_size_changed_input_and_wrong_independent_hash_are_rejected(self):
        for incoming, digest in ((b'x' * len(self.original), None), (None, '0' * 64)):
            with self.subTest(incoming=incoming), self.assertRaisesRegex(ValueError, 'EFI bytes differ'):
                self.reuse(incoming, digest)

    def test_retained_request_and_response_must_be_bound_to_old_context(self):
        for field in ('context_sha256', 'request_id', 'input_sha256', 'operation', 'output_sha256', 'output_bytes'):
            saved = self.response[field]
            self.response[field] = 1 if field == 'output_bytes' else 'wrong'
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.reuse()
            self.response[field] = saved
        self.old_request['context_sha256'] = s.context(self.new)
        with self.assertRaisesRegex(ValueError, 'request context'):
            self.reuse()

    def test_pcr_signing_is_never_reused(self):
        self.old_request['operation'] = self.request['operation'] = 'sign-sha256-rsa-pkcs1'
        self.response['operation'] = 'sign-sha256-rsa-pkcs1'
        with self.assertRaisesRegex(ValueError, 'not selected EFI input'):
            self.reuse()

    def test_all_efi_purposes_use_the_same_exact_admission(self):
        for purpose in o.EFI_PURPOSES:
            with self.subTest(purpose=purpose):
                result = o.reuse_efi(
                    purpose, self.old, self.new, self.old_request, self.request,
                    self.response, self.original, self.original, self.signed,
                    s.digest(self.original))
                self.assertEqual(result['request_id'], 'b' * 32)
        with self.assertRaisesRegex(ValueError, 'unsupported EFI reuse purpose'):
            o.reuse_efi(
                'pcr-policy', self.old, self.new, self.old_request, self.request,
                self.response, self.original, self.original, self.signed,
                s.digest(self.original))


if __name__ == '__main__':
    unittest.main()

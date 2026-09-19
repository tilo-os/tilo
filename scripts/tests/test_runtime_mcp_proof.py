import importlib.util
import pathlib
import unittest
from unittest.mock import patch
import sys
import tempfile

spec = importlib.util.spec_from_file_location('proof', pathlib.Path(__file__).parents[1] / 'runtime-mcp-proof.py')
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)


class ProofTests(unittest.TestCase):
    def test_centered_crop_uses_dimensions(self):
        self.assertEqual(proof.square_region(48, 32), '0.16666666666666666,0,0.6666666666666666,1')
        self.assertEqual(proof.square_region(32, 48), '0,0.16666666666666666,1,0.6666666666666666')
        with self.assertRaises(ValueError):
            proof.square_region(0, 32)

    def test_resolve_uses_fresh_page_and_real_descriptor(self):
        calls = []
        def tool(name, args):
            calls.append((name, args))
            if name == 'qos_query':
                return {'revision': 'current', 'rows': [{'resource': 'photo:real', 'action': 'crop', 'descriptor': {'params': [{'name': 'region'}]}}], 'exhausted': True}
            return {'row': {'descriptor': {'params': [{'name': 'region'}]}}}
        target, descriptor = proof.resolve_action(tool, 7, 'crop')
        self.assertEqual(target, {'peer_id': 7, 'revision': 'current', 'resource': 'photo:real', 'action': 'crop'})
        self.assertEqual(calls[-1], ('qos_resolve', target))
        self.assertEqual(proof.single_argument(descriptor, 'value'), {'region': 'value'})

    def test_browses_resource_actions_instead_of_provider_search(self):
        def tool(name, args):
            if name == 'qos_resolve':
                return {'row': {'descriptor': {'params': []}}}
            self.assertNotIn('needle', args)
            parent = args.get('parent_resource', '')
            rows = [{'resource': 'photo', 'action': None, 'has_children': True}] if not parent else [{'resource': 'photo', 'action': 'crop', 'descriptor': {'params': []}}]
            return {'revision': 'r', 'rows': rows, 'exhausted': True}
        target, _ = proof.resolve_action(tool, 1, 'crop')
        self.assertEqual(target['resource'], 'photo')

    def test_profile_path_is_short_independent_of_evidence_path(self):
        with proof.short_profile() as profile:
            self.assertLess(len(str(profile / 'collab.sock').encode()), 100)
            self.assertTrue(profile.is_dir())
        self.assertFalse(profile.exists())

    def test_accepted_is_polled_without_reinvocation(self):
        calls = []
        updates = iter([{'state': 'progress'}, {'state': 'succeeded', 'output': {'resources': ['qos.content:actual']}}])
        def tool(name, args):
            calls.append(name)
            if name == 'qos_invoke':
                return {'invocation_id': 42, 'update': {'state': 'accepted'}}
            self.assertEqual(args, {'invocation_id': 42})
            return {'update': next(updates)}
        self.assertEqual(proof.invoke(tool, {}, {}, timeout=1, interval=0)['resources'], ['qos.content:actual'])
        self.assertEqual(calls, ['qos_invoke', 'qos_status', 'qos_status'])

    def test_failure_and_timeout_never_replay(self):
        for state in ('failed', 'cancelled', 'accepted'):
            calls = []
            def tool(name, args):
                calls.append(name)
                return {'invocation_id': 42, 'update': {'state': state}}
            with self.assertRaises(proof.ProofError):
                proof.invoke(tool, {}, {}, timeout=0, interval=0)
            self.assertEqual(calls, ['qos_invoke'])

    def test_stdio_client_records_exact_requests_and_tool_denial(self):
        host = """import json,sys
for line in sys.stdin:
 r=json.loads(line)
 result={'isError':True,'content':[{'type':'text','text':'denied'}]} if r['method']=='tools/call' else {'pong':True}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"""
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            client = proof.Client([sys.executable, '-u', '-c', host], root)
            try:
                self.assertEqual(client.request('ping', {}), {'pong': True})
                with self.assertRaisesRegex(proof.ProofError, 'denied'):
                    client.tool('qos_invoke', {'arguments': {}})
            finally:
                self.assertEqual(client.close(), 0)
            records = [proof.json.loads(line) for line in (root / 'transcript.jsonl').read_text().splitlines()]
            self.assertEqual(len(records), 4)
            self.assertEqual(records[2]['message']['method'], 'tools/call')
            self.assertEqual(records[2]['message']['params']['name'], 'qos_invoke')

    def test_cleanup_finds_known_child_after_it_is_reparented(self):
        with patch.object(proof.subprocess, 'check_output', return_value='201 1 S /private/binaries/photos\n999 1 S unrelated\n'):
            remaining = proof.processes_with_pids({101, 201})
        self.assertEqual([process['pid'] for process in remaining], [201])

    def test_invalid_args_observe_accepted_until_failed_without_replay(self):
        calls = []
        def tool(name, args):
            calls.append((name, args))
            return {'invocation_id': 7, 'update': {'state': 'failed', 'error': {'message': 'invalid argument region'}}}
        initial = {'isError': False, 'structuredContent': {'invocation_id': 7, 'update': {'state': 'accepted'}}}
        terminal = proof.expect_denial(tool, initial, timeout=1, interval=0)
        self.assertEqual(terminal['update']['state'], 'failed')
        self.assertEqual(calls, [('qos_status', {'invocation_id': 7})])
        with self.assertRaises(proof.ProofError):
            proof.expect_denial(tool, {'structuredContent': {'invocation_id': 8, 'update': {'state': 'succeeded', 'output': {}}}}, timeout=0)

    def test_stale_read_only_resolution_is_rediscovered(self):
        calls = []
        def tool(name, args):
            calls.append(name)
            if name == 'qos_query':
                return {'revision': str(len(calls)), 'rows': [{'resource': 'photo', 'action': 'crop', 'descriptor': {'params': []}}], 'exhausted': True}
            if len(calls) == 2:
                raise proof.StaleDiscovery('StaleRevision')
            return {'row': {'descriptor': {'params': []}}}
        target, _ = proof.resolve_action(tool, 1, 'crop', interval=0)
        self.assertEqual(target['revision'], '3')
        self.assertEqual(calls, ['qos_query', 'qos_resolve', 'qos_query', 'qos_resolve'])

    def test_waits_for_normal_publication_without_invoking(self):
        calls = []
        def tool(name, args):
            calls.append(name)
            if name == 'qos_resolve':
                return {'row': {'descriptor': {'params': []}}}
            rows = [] if len(calls) == 1 else [{'resource': 'photo', 'action': 'export', 'descriptor': {'params': []}}]
            return {'revision': 'r', 'rows': rows, 'exhausted': True}
        target, _ = proof.resolve_action(tool, 1, 'export', interval=0)
        self.assertEqual(target['action'], 'export')
        self.assertEqual(calls, ['qos_query', 'qos_query', 'qos_resolve'])

    def test_failure_evidence_includes_shutdown_verification_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / 'evidence'
            with patch.object(sys, 'argv', ['proof', '--output', str(output)]), patch.object(proof, 'run_proof', side_effect=proof.ProofError('cleanup verification failed')):
                with self.assertRaises(proof.ProofError):
                    proof.main()
            self.assertIn('cleanup verification failed', (output / 'failure.txt').read_text())

    def test_protocol_errors_and_mismatched_reply_are_rejected(self):
        with self.assertRaises(proof.ProofError):
            proof.decode_reply({'id': 2, 'result': {}}, 1)
        with self.assertRaises(proof.ProofError):
            proof.decode_reply({'id': 1, 'error': {'message': 'denied'}}, 1)
        self.assertEqual(proof.decode_reply({'jsonrpc': '2.0', 'id': 1, 'result': {'ok': True}}, 1), {'ok': True})

if __name__ == '__main__':
    unittest.main()

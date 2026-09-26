"""Negative controls for independent outcomes, evidence, bounded actions and cost."""
import copy
import base64
import io
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest import mock
import zlib
import usability as u


def png(path, uniform=False):
    def chunk(tag, data):
        return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', zlib.crc32(tag + data))
    pixels = b'\x00\x00\x00\x00' + (b'\x00\x00\x00' if uniform else b'\xff\x80\x30')
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 2, 1, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b''))


class Controls(unittest.TestCase):
    def setUp(self):
        self.tree = {'role': 'Text', 'name': 'Document-000.txt', 'value': 'Fixture document 0\n'}
        self.response = {'state': {'active_tabs': [{'title': 'Document-000.txt', 'content': {'AppPlaceholder': {'kind': {'Other': 'Editor'}}}}]}}
        self.action = {k: None for k in u.ACTION_SCHEMA['required']}
        self.action.update(op='done', modifiers=[], observation='Finished', expected_feedback='Document visible')

    def test_outcome_oracle_requires_exact_visible_persisted_result(self):
        with self.subTest(phase='exact outcome positive control'):
            self.setUp()
            self.assertTrue(u.oracle('open-file', self.response, self.tree))
        with self.subTest(phase='wrong body cannot pass'):
            self.setUp()
            self.tree['value'] = 'Fixture document 60\n'
            self.assertFalse(u.oracle('open-file', self.response, self.tree))
        with self.subTest(phase='wrong provider cannot pass'):
            self.setUp()
            self.response['state']['active_tabs'][0]['content']['AppPlaceholder']['kind'] = 'FileExplorer'
            self.assertFalse(u.oracle('open-file', self.response, self.tree))
        with self.subTest(phase='rename requires persisted name'):
            self.setUp()
            self.assertFalse(u.oracle('rename-workspace', {'state': {'active_workspace': 'Research'}}, {}))
            self.assertTrue(u.oracle('rename-workspace', {'state': {'active_workspace': 'Research'}, 'persisted_workspace_names': ['Research']}, {}))
        with self.subTest(phase='hidden parent is not visible feedback'):
            self.setUp()
            self.assertFalse(u.oracle('open-file', self.response, {'state': {'hidden': True}, 'children': [self.tree]}))
        with self.subTest(phase='offscreen document and modal cannot pass'):
            self.setUp()
            offscreen = {**self.tree, 'bounds': {'x0': 0, 'y0': 900, 'x1': 400, 'y1': 950}}
            self.assertFalse(u.oracle('open-file', self.response, {'viewport': [800, 700], 'root': offscreen}))
            self.assertFalse(u.oracle('open-file', self.response, {'children': [self.tree, {'role': 'Dialog', 'name': 'Blocking modal'}]}))

    def test_assessment_separates_verified_and_efficient_success(self):
        with self.subTest(phase='false completion cannot pass'):
            self.setUp()
            result = u.assess('open-file', {'state': {}}, self.tree, [{'op': 'done'}], ['a'], 24)
            self.assertFalse(result['verified_success'])
        with self.subTest(phase='noops cannot pass efficiency'):
            self.setUp()
            result = u.assess('open-file', self.response, self.tree, [{'op': 'observe'}] * 6, ['a'] * 7, 24)
            self.assertTrue(result['verified_success'])
            self.assertFalse(result['efficient_success'])
        with self.subTest(phase='excess route cannot pass efficiency'):
            self.setUp()
            result = u.assess('open-file', self.response, self.tree, [{'op': 'key'}] * 20, list(map(str, range(21))), 24)
            self.assertFalse(result['efficient_success'])
        with self.subTest(phase='token error cannot pass'):
            self.setUp()
            result = u.assess('open-file', self.response, self.tree, [], ['a'], 24, ['API token budget exceeded'])
            self.assertFalse(result['verified_success'])

    def test_action_validation_is_bounded_and_normalizes_named_keys(self):
        with self.subTest(phase='invalid actions'):
            self.setUp()
            for change in ({'op': 'execute'}, {'op': 'click', 'x': float('nan'), 'y': 1}, {'op': 'click', 'x': 800, 'y': 2}, {'text': 'hidden'}, {'modifiers': ['bad']}, {'op': 'type', 'text': 'x' * 257}):
                action = {**self.action, **change}
                with self.subTest(change=change), self.assertRaises(ValueError):
                    u.validate_action(action, (800, 700))
        with self.subTest(phase='extra field rejected'):
            self.setUp()
            with self.assertRaises(ValueError):
                u.validate_action({**self.action, 'script': 'not allowed'}, (800, 700))
        with self.subTest(phase='named key case normalization'):
            self.setUp()
            for raw, expected in [('BACKSPACE', 'Backspace'), ('ENTER', 'Enter'), ('ARROWUP', 'ArrowUp'), ('arrowdown', 'ArrowDown'), ('ArRoWlEfT', 'ArrowLeft'), ('RIGHT', 'Right')]:
                with self.subTest(key=raw):
                    command = u.validate_action({**self.action, 'op': 'key', 'key': raw}, (800, 700))
                    self.assertEqual(command['key'], expected)
            for invalid in ('PageDown', 'EXECUTE', 'CTRL+A', '\n'):
                with self.subTest(key=invalid), self.assertRaises(ValueError):
                    u.validate_action({**self.action, 'op': 'key', 'key': invalid}, (800, 700))

    def test_evidence_requires_complete_png_and_inspection(self):
        with self.subTest(phase='missing evidence cannot pass'):
            self.setUp()
            with self.assertRaises((KeyError, ValueError, FileNotFoundError)):
                u.evidence({'ok': True, 'state': {}})
        with self.subTest(phase='blank, invalid and valid png'):
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / 'capture.png'
                path.write_bytes(b'')
                with self.assertRaises(ValueError):
                    u.png_evidence(path)
                png(path, True)
                with self.assertRaises(ValueError):
                    u.png_evidence(path)
                png(path)
                self.assertEqual(u.png_evidence(path)[0], (2, 1))
        with self.subTest(phase='missing IEND rejected'):
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / 'capture.png'
                png(path)
                path.write_bytes(path.read_bytes()[:-12])
                with self.assertRaises(ValueError):
                    u.png_evidence(path)
        with self.subTest(phase='missing inspection cannot pass'):
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary)
                png(path / 'capture.png')
                (path / 'inspect.json').write_text('{}')
                with self.assertRaises(ValueError):
                    u.evidence({'ok': True, 'state': {}, 'screenshot': str(path / 'capture.png'), 'inspection': str(path / 'inspect.json')})

    def test_geometry_audit_measures_only_fully_visible_controls(self):
        with self.subTest(phase='geometry counts and small targets'):
            self.setUp()
            inspection = {'viewport': [100, 100], 'root': {'path': 'root', 'children': [{'path': 'button', 'rect': [1, 1, 12, 12], 'semantics': {'role': 'Button', 'name': 'Small'}}]}}
            result = u.geometry_audit(inspection)
            self.assertEqual(result['measured_controls'], 1)
            self.assertFalse(result['passed'])
            self.assertEqual(result['failures'][0]['property'], 'target_size_24')
        with self.subTest(phase='no geometry evidence does not pass'):
            self.setUp()
            self.assertFalse(u.geometry_audit({'viewport': [100, 100], 'root': {}})['passed'])
        with self.subTest(phase='hidden geometry and nested clip'):
            self.setUp()
            control = {'path': 'button', 'rect': [80, 80, 30, 30], 'semantics': {'role': 'Button', 'name': 'Button'}}
            tree = {'viewport': [200, 200], 'root': {'path': 'root', 'semantics': {'state': {'hidden': True}}, 'children': [control]}}
            self.assertEqual(u.geometry_audit(tree)['measured_controls'], 0)
            tree['root'] = {'path': 'root', 'child_clip': [0, 0, 50, 50], 'children': [{'path': 'inner', 'child_clip': [0, 0, 150, 150], 'children': [control]}]}
            self.assertEqual(u.geometry_audit(tree)['measured_controls'], 0)
        with self.subTest(phase='one pixel fragment is excluded not certified'):
            self.setUp()
            inspection = {'viewport': [100, 100], 'root': {'path': 'root', 'children': [{'path': 'button', 'rect': [99, 10, 30, 30], 'semantics': {'role': 'Button', 'name': 'Clipped'}}]}}
            result = u.geometry_audit(inspection)
            self.assertFalse(result['passed'])
            self.assertEqual(result['measured_controls'], 0)
            self.assertEqual(result['partially_clipped_controls'][0]['visible_rect'], [99, 10, 1, 30])

    def test_agent_attempts_and_report_keep_honest_accounting(self):
        with self.subTest(phase='last permitted action keeps verified outcome'):
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary)
                png(path / 'capture.png')
                (path / 'inspect.json').write_text(json.dumps(self.tree))
                response = {**self.response, 'ok': True, 'screenshot': str(path / 'capture.png'), 'inspection': str(path / 'inspect.json')}
                class FakeHost:
                    def __init__(self, *args):
                        self.current, self.frames, self.latency = response, [response], 0
                    def send(self, command):
                        self.frames.append(response)
                        return response
                    def close(self):
                        pass
                api = mock.Mock(model='test-model')
                action = {**self.action, 'op': 'observe'}
                api.action.return_value = (json.dumps(action), {'total_tokens': 10}, 0.1, 'completed', 'test-model')
                with mock.patch.object(u, 'Host', FakeHost):
                    result = u.agent_attempt(path / 'binary', path / 'run', 'open-file', 'baseline', api, {'steps': 1, 'tokens': 100, 'seconds': 30, 'api_seconds': 10})
                self.assertTrue(result['verified_success'])
                self.assertFalse(result['efficient_success'])
        with self.subTest(phase='interruption retains calls, actions, frames and journal'):
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary)
                png(path / 'capture.png')
                (path / 'inspect.json').write_text(json.dumps(self.tree))
                response = {**self.response, 'ok': True, 'sequence': 0, 'screenshot': str(path / 'capture.png'), 'inspection': str(path / 'inspect.json')}
                class FakeHost:
                    def __init__(self, *args):
                        self.current, self.frames, self.latency = response, [response], 0
                    def send(self, command):
                        self.frames.append(response)
                        return response
                    def close(self):
                        pass
                api = mock.Mock(model='test-model')
                action = {**self.action, 'op': 'observe'}
                api.action.side_effect = [(json.dumps(action), {'total_tokens': 10}, 0.1, 'completed', 'test-model'), KeyboardInterrupt()]
                with mock.patch.object(u, 'Host', FakeHost):
                    result = u.agent_attempt(path / 'binary', path / 'run', 'open-file', 'baseline', api, {'steps': 2, 'tokens': 100, 'seconds': 30, 'api_seconds': 10})
                self.assertTrue(result['interrupted'])
                self.assertEqual(result['classification'], 'execution_error')
                self.assertEqual(result['tokens'], 10)
                self.assertEqual(len(result['actions']), 1)
                self.assertEqual(len(result['frames']), 2)
                self.assertEqual(result['calls'][-1]['status'], 'interrupted_usage_unknown')
                self.assertIsNone(result['calls'][-1]['usage'])
                stored = json.loads((path / 'run' / 'result.json').read_text())
                self.assertTrue(stored['interrupted'])
                journal = [json.loads(line) for line in (path / 'run' / 'journal.jsonl').read_text().splitlines()]
                self.assertEqual([entry['event'] for entry in journal], ['api_call', 'validated_action', 'input_dispatched', 'input_completed', 'api_call'])
        with self.subTest(phase='repeat attempts start with fresh history and host'):
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary)
                png(path / 'capture.png')
                (path / 'inspect.json').write_text(json.dumps(self.tree))
                response = {**self.response, 'ok': True, 'screenshot': str(path / 'capture.png'), 'inspection': str(path / 'inspect.json')}
                hosts = []
                class FakeHost:
                    def __init__(self, *args):
                        self.current, self.frames, self.latency = response, [response], 0
                        hosts.append(self)
                    def send(self, command):
                        self.frames.append(response)
                        return response
                    def close(self):
                        pass
                histories = []
                def respond(goal, screenshot, history, remaining):
                    histories.append(copy.deepcopy(history))
                    return json.dumps(self.action), {'total_tokens': 10}, 0.1, 'completed', 'test-model'
                api = mock.Mock(model='test-model')
                api.action.side_effect = respond
                with mock.patch.object(u, 'Host', FakeHost):
                    for trial in range(2):
                        u.agent_attempt(path / 'binary', path / str(trial), 'open-file', 'baseline', api, {'steps': 2, 'tokens': 100, 'seconds': 30, 'api_seconds': 10})
                self.assertEqual(histories, [[], []])
                self.assertEqual(len(hosts), 2)
                self.assertIsNot(hosts[0], hosts[1])
        with self.subTest(phase='early failure does not shrink planned denominator'):
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary)
                runs = [{'kind': 'deterministic', 'variant': 'baseline', 'properties': [{'property': 'keyboard_entry', 'passed': True}], 'classification': 'success', 'directory': str(path)},
                        {'kind': 'deterministic', 'variant': 'compact', 'properties': [], 'classification': 'execution_error', 'directory': str(path)}]
                result = u.write_report(path, runs)
                self.assertEqual(result['properties']['keyboard_entry'], {'passed': 1, 'measured': 1, 'planned': 2, 'skipped': 1})

    def test_api_boundary_sends_only_goal_pixels_and_own_history(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)
            screenshot = path / 'capture.png'
            png(screenshot)
            private_marker = 'PRIVATE_ORACLE_AND_INSPECTION_SENTINEL'
            (path / 'capture.inspection.json').write_text(json.dumps({'state': private_marker, 'route': ['click secret target']}))
            goal = 'Open the requested document.'
            history = [{**self.action, 'op': 'observe'}]
            fake_key = 'test-credential-never-persist'
            response = {'status': 'completed', 'model': 'test-model', 'usage': {'total_tokens': 8}, 'output': [{'content': [{'type': 'output_text', 'text': json.dumps(self.action)}]}]}
            api = u.API(fake_key, 'test-model', 100, 5)
            with mock.patch.object(u.urllib.request, 'urlopen', return_value=io.StringIO(json.dumps(response))) as request_mock:
                result = api.action(goal, screenshot, history, 80)
            request = request_mock.call_args.args[0]
            body = json.loads(request.data)
            self.assertEqual(request.full_url, 'https://api.openai.com/v1/responses')
            self.assertEqual(request.get_method(), 'POST')
            self.assertIs(body['store'], False)
            self.assertEqual(body['max_output_tokens'], 80)
            self.assertEqual(request_mock.call_args.kwargs['timeout'], 5)
            self.assertEqual(request.get_header('Authorization'), 'Bearer ' + fake_key)
            self.assertEqual(body['input'], [
                {'role': 'system', 'content': u.SYSTEM},
                {'role': 'user', 'content': [
                    {'type': 'input_text', 'text': goal + '\nYour previous actions: ' + json.dumps(history)},
                    {'type': 'input_image', 'image_url': 'data:image/png;base64,' + base64.b64encode(screenshot.read_bytes()).decode(), 'detail': 'original'}]}])
            self.assertEqual(set(body), {'model', 'store', 'max_output_tokens', 'reasoning', 'input', 'text'})
            self.assertEqual(body['text']['format'], {'type': 'json_schema', 'name': 'desktop_action', 'strict': True, 'schema': u.ACTION_SCHEMA})
            serialized = request.data.decode()
            for forbidden in (fake_key, private_marker, str(path), 'persisted_workspace_names', 'Fixture document', 'active_tabs', 'click secret target'):
                self.assertNotIn(forbidden, serialized)
            self.assertEqual(result[1], {'total_tokens': 8})
            self.assertEqual(json.loads(result[0]), self.action)


if __name__ == '__main__':
    unittest.main()

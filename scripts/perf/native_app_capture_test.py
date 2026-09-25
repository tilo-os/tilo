import json
import unittest
import native_app_capture as capture
import collect_scratchpad_native_test as failure_tests


class CaptureGateTests(unittest.TestCase):
    def test_repeat_steps_expand_to_the_same_bounded_input_stream(self):
        scenario = capture.expand_scenario({
            'warmup_steps': 3,
            'steps': [
                {'kind': 'click', 'x': 80, 'y': 48},
                {'kind': 'repeat', 'count': 2, 'steps': [
                    {'kind': 'character', 'value': 'z'},
                    {'kind': 'key', 'key': 'backspace'},
                ]},
            ],
        })
        self.assertEqual(len(scenario['steps']), 5)
        self.assertEqual(scenario['steps'][1:], [
            {'kind': 'character', 'value': 'z'},
            {'kind': 'key', 'key': 'backspace'},
        ] * 2)
        for invalid in [
            {'kind': 'repeat', 'count': 0, 'steps': [{'kind': 'key'}]},
            {'kind': 'repeat', 'count': 10_001, 'steps': [{'kind': 'key'}]},
            {'kind': 'repeat', 'count': 1, 'steps': []},
            {'kind': 'repeat', 'count': 1, 'steps': [{'kind': 'repeat'}]},
        ]:
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                capture.expand_scenario({'steps': [invalid]})

    def test_drain_waits_for_worker_after_an_auxiliary_handoff(self):
        def row(prefix, **fields):
            return prefix+' '+json.dumps(fields)
        lines=[row(prefix,cpu_frame_id=1,worker_id=1,worker_frame_ix=0)
               for prefix in ('qos-native-send','qos-native-worker','qos-native-present-call',
                              'qos-native-worker-handoff')]
        lines.append(row('qos-native-worker-handoff',cpu_frame_id=None,worker_id=2,worker_frame_ix=0))
        self.assertFalse(capture.diagnostics_drained(lines,{1}))
        lines.append(row('qos-native-worker',cpu_frame_id=None,worker_id=2,worker_frame_ix=0))
        self.assertTrue(capture.diagnostics_drained(lines,{1}))

    def test_drain_waits_for_post_send_diagnostics_but_supports_legacy(self):
        def row(prefix, cpu):
            return prefix+' '+json.dumps(dict(cpu_frame_id=cpu))
        self.assertTrue(capture.diagnostics_drained([], {1,2}))
        lines=[row(prefix,cpu) for prefix in ('qos-native-send','qos-native-worker','qos-native-present-call') for cpu in (1,2)]
        self.assertTrue(capture.diagnostics_drained(lines,{1,2}))
        self.assertFalse(capture.diagnostics_drained(lines[:-1],{1,2}))
        lines.append(row('qos-native-main-stages',1))
        self.assertFalse(capture.diagnostics_drained(lines,{1,2}))
        lines.append(row('qos-native-main-stages',2))
        self.assertTrue(capture.diagnostics_drained(lines,{1,2}))
        lines.append(row('qos-native-worker-handoff',1))
        self.assertFalse(capture.diagnostics_drained(lines,{1,2}))
        lines.append(row('qos-native-worker-handoff',2))
        self.assertTrue(capture.diagnostics_drained(lines,{1,2}))

    def test_exit_after_last_effect_is_still_a_failure(self):
        source = '''import json
print('qos-native-effect {}', flush=True)
print('qos-native-frame '+json.dumps(dict(cpu_frame_id=1,input_ids=[0])), flush=True)
print('qos-display-frame '+json.dumps(dict(cpu_frame_id=1)), flush=True)
raise SystemExit(7)
'''
        failure, metadata = failure_tests.CollectorFailureTests().run_failure(source)
        self.assertIn('exited early: 7', failure['capture_error'])
        self.assertEqual(metadata['exit_code'], 7)

    def test_only_real_workload_steps_can_be_measured(self):
        cases = [('typing',dict(kind='character',value='x')),
                 ('typing',dict(kind='key',key='backspace')),
                 ('scroll',dict(kind='wheel',dx=0,dy=-3))]
        for mode, step in cases:
            scenario = dict(warmup_steps=1, steps=[dict(kind='hover',x=10,y=10),step])
            capture.validate_measured_workload(scenario,mode)
            for setup in [dict(kind='hover',x=10,y=10),dict(kind='click',x=10,y=10),
                          dict(kind='key',key='home')]:
                with self.subTest(mode=mode,setup=setup), self.assertRaises(ValueError):
                    capture.validate_measured_workload(dict(warmup_steps=0,steps=[setup]*1000+[step]),mode)
        with self.assertRaises(ValueError):
            capture.validate_measured_workload(dict(warmup_steps=0,steps=[dict(kind='wheel',dx=0,dy=0)]),'scroll')

    def test_requested_pointer_isolation_requires_actual_window_ack(self):
        window = dict(scale_factor=2,physical_width=1600,physical_height=1200)
        lines = lambda value: ['qos-native-window '+json.dumps(value)]
        self.assertEqual(capture.window_metadata(lines(window)),window)
        for marker in [None,False,1,'true']:
            sample=dict(window)
            if marker is not None: sample['pointer_passthrough']=marker
            with self.subTest(marker=marker), self.assertRaises(ValueError):
                capture.window_metadata(lines(sample),True)
        enabled=dict(window,pointer_passthrough=True,input_route_isolation=True)
        for marker in [None,False,1,'true']:
            sample=dict(enabled)
            if marker is None: sample.pop('input_route_isolation')
            else: sample['input_route_isolation']=marker
            with self.subTest(route_marker=marker), self.assertRaises(ValueError):
                capture.window_metadata(lines(sample),True)
        self.assertEqual(capture.window_metadata(lines(enabled),True),enabled)


if __name__ == '__main__':
    unittest.main()

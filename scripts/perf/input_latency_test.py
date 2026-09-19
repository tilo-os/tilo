import json
import unittest
from input_latency import summarize_input_latency


class InputLatencyTests(unittest.TestCase):
    def fixture(self):
        scenario = {'steps': [{'kind': 'wheel', 'dx': 0, 'dy': 1}]*3,
                    'interval_us': 1000, 'warmup_steps': 1}
        lines = []
        for i in range(3):
            row = dict(id=i, warmup=i == 0, input=scenario['steps'][i],
                       scheduled_us=i*1000, sent_us=i*1000+1,
                       dispatch_start_us=i*1000+10, lateness_us=10,
                       host_clock='mach_absolute_seconds',
                       sent_host_lo_s=100+i*.001, sent_host_hi_s=100+i*.001+.000001,
                       dispatch_host_lo_s=100.003+i*.001,
                       dispatch_host_hi_s=100.003+i*.001+.000002)
            lines.append('qos-native-input '+json.dumps(row))
        frames = [{'frame': 19, 'total_us': 100}, {'frame': 20, 'total_us': 100}]
        for cpu, ids, displayed in [(19, [0], 100.004), (20, [1, 2], 100.020)]:
            lines.append('qos-native-frame '+json.dumps(dict(cpu_frame_id=cpu, input_ids=ids)))
            lines.append('qos-render-frame '+json.dumps(dict(cpu_frame_id=cpu, outcome='Presented')))
            identity = dict(worker_id=1, worker_frame_ix=cpu, cpu_frame_id=cpu)
            lines.append('qos-display-request '+json.dumps(identity))
            lines.append('qos-display-frame '+json.dumps(dict(identity,
                presented_time_s=displayed, backend='metal', drawable_id=cpu)))
        return scenario, lines, frames

    def change(self, lines, prefix, index, **values):
        lines = list(lines)
        locations = [i for i,s in enumerate(lines) if s.startswith(prefix+' ')]
        at = locations[index]
        row = json.loads(lines[at].split(' ',1)[1]); row.update(values)
        lines[at] = prefix+' '+json.dumps(row)
        return lines

    def test_keeps_each_coalesced_input_and_final_dispatch_after_sender_finished(self):
        report = summarize_input_latency(*self.fixture())
        self.assertIsInstance(report, dict)
        self.assertTrue(report['latency_complete'])
        self.assertFalse(report['performance_certified'])
        self.assertEqual([r['input_id'] for r in report['inputs']], [1,2])
        self.assertEqual(report['coalesced_measured_inputs'], 2)
        for sample, sent, dispatch in zip(report['inputs'], [18.999,17.999], [15.998,14.998]):
            self.assertAlmostEqual(sample['sent_to_present_ms'][0], sent, places=7)
            self.assertAlmostEqual(sample['dispatch_to_present_ms'][0], dispatch, places=7)
            self.assertGreater(sample['sent_to_present_ms'][1], sample['sent_to_present_ms'][0])

    def test_mixed_warmup_frame_retains_only_measured_inputs(self):
        scenario, lines, frames = self.fixture()
        lines = self.change(lines, 'qos-native-frame', 0, input_ids=[0,1])
        lines = self.change(lines, 'qos-native-frame', 1, input_ids=[2])
        lines = self.change(lines, 'qos-display-frame', 0, presented_time_s=100.010)
        report = summarize_input_latency(scenario, lines, frames)
        self.assertIsInstance(report, dict)
        self.assertEqual([r['input_id'] for r in report['inputs']], [1,2])

    def test_missing_and_partial_clocks_are_distinguished(self):
        scenario, lines, frames = self.fixture()
        old=[]
        for line in lines:
            if line.startswith('qos-native-input '):
                row=json.loads(line.split(' ',1)[1])
                row={k:v for k,v in row.items() if 'host_' not in k}
                line='qos-native-input '+json.dumps(row)
            old.append(line)
        report=summarize_input_latency(scenario,old,frames)
        self.assertIsInstance(report, dict)
        self.assertFalse(report['available'])
        with self.assertRaises(ValueError):
            summarize_input_latency(scenario, [old[0],*lines[1:]], frames)

    def test_invalid_brackets_and_causal_order_are_rejected(self):
        scenario, lines, frames = self.fixture()
        for changes in [dict(sent_host_lo_s=float('nan')),dict(sent_host_hi_s=float('inf')),
                        dict(sent_host_lo_s=True),dict(sent_host_lo_s=0),
                        dict(sent_host_hi_s=99),dict(dispatch_host_lo_s=99),
                        dict(host_clock='other'),dict(dispatch_host_hi_s=100.002)]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                summarize_input_latency(scenario,self.change(lines,'qos-native-input',1,**changes),frames)

    def test_missing_zero_duplicate_and_foreign_feedback_are_rejected(self):
        scenario, lines, frames = self.fixture()
        variants=[lines[:-1], lines+[lines[-1]],
                  self.change(lines,'qos-display-frame',1,presented_time_s=0),
                  self.change(lines,'qos-display-frame',1,cpu_frame_id=999),
                  self.change(lines,'qos-native-frame',1,input_ids=[1,1]),
                  self.change(lines,'qos-native-frame',1,input_ids=[1,99])]
        for variant in variants:
            with self.assertRaises(ValueError): summarize_input_latency(scenario,variant,frames)

    def test_definite_negative_rejected_and_precision_overlap_unresolved(self):
        scenario, lines, frames = self.fixture()
        with self.assertRaises(ValueError):
            summarize_input_latency(scenario,self.change(lines,'qos-display-frame',1,presented_time_s=100.0045),frames)
        lines=self.change(lines,'qos-display-frame',1,presented_time_s=100.005001)
        report=summarize_input_latency(scenario,lines,frames)
        self.assertIsInstance(report, dict)
        self.assertFalse(report['latency_complete'])
        self.assertEqual(report['unresolved_input_ids'],[2])
        self.assertIsNone(report['dispatch_to_present_upper'])

    def test_large_host_epoch_keeps_numeric_uncertainty(self):
        scenario,lines,frames=self.fixture()
        epoch=2**30
        for i in range(3):
            lines=self.change(lines,'qos-native-input',i,
                sent_host_lo_s=epoch+i*.001,sent_host_hi_s=epoch+i*.001,
                dispatch_host_lo_s=epoch+.003+i*.001,dispatch_host_hi_s=epoch+.003+i*.001)
        for i,t in enumerate([epoch+.004,epoch+.020]):
            lines=self.change(lines,'qos-display-frame',i,presented_time_s=t)
        report=summarize_input_latency(scenario,lines,frames)
        self.assertIsInstance(report,dict)
        bounds=report['inputs'][0]['sent_to_present_ms']
        self.assertLess(bounds[0],19)
        self.assertGreater(bounds[1],19)

    def test_noncontiguous_frame_mapping_cannot_invent_latency(self):
        scenario,lines,frames=self.fixture()
        lines=self.change(lines,'qos-native-frame',0,input_ids=[0,2])
        lines=self.change(lines,'qos-native-frame',1,input_ids=[1])
        lines=self.change(lines,'qos-display-frame',0,presented_time_s=100.010)
        with self.assertRaises(ValueError): summarize_input_latency(scenario,lines,frames)

    def test_numeric_overflow_cannot_be_complete_latency(self):
        scenario,lines,frames=self.fixture()
        lines=self.change(lines,'qos-display-frame',1,presented_time_s=1e308)
        with self.assertRaises(ValueError): summarize_input_latency(scenario,lines,frames)

    def test_unsupported_platform_is_explicitly_unavailable(self):
        scenario,lines,frames=self.fixture()
        for i in range(3):
            lines=self.change(lines,'qos-native-input',i,host_clock='unavailable',
                sent_host_lo_s=None,sent_host_hi_s=None,
                dispatch_host_lo_s=None,dispatch_host_hi_s=None)
        report=summarize_input_latency(scenario,lines,frames)
        self.assertFalse(report['available'])
        self.assertFalse(report['latency_complete'])

    def test_clock_lifecycle_event_and_frame_reordering_fail(self):
        scenario,lines,frames=self.fixture()
        with self.assertRaises(ValueError):
            summarize_input_latency(scenario,lines+['qos-native-clock-event {"event":"suspend"}'],frames)
        indices=[i for i,s in enumerate(lines) if s.startswith('qos-native-frame ')]
        lines[indices[0]],lines[indices[1]]=lines[indices[1]],lines[indices[0]]
        with self.assertRaises(ValueError): summarize_input_latency(scenario,lines,frames)

if __name__=='__main__':unittest.main()

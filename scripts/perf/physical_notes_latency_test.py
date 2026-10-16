"""The existing latency analyser must reject unrelated or incomplete timelines."""
import copy
import json
import unittest
from input_latency import summarize_physical_notes_latency


class PhysicalNotesLatency(unittest.TestCase):
    def timeline(self):
        page = 'scratch-only'
        def state(n):
            return dict(app='so.tilo.notes', selected_page=page, main_page=page,
                        editor_binding_page=page, editor_focused=True, hash_algorithm='blake3',
                        body_hash=str(n)*64, rendered_body_hash=str(n)*64,
                        body_chars=n, rendered_body_chars=n, body_bytes=n,
                        caret=dict(byte_offset=n))
        session = '2bd78136-4c17-4853-9d7c-7442274a7059'
        result = []
        for n in range(1, 4):
            start = n*100_000_000
            result += [
                ('tilo-physical-delivery ', dict(wayland_serial=n+9, surface_id=42,
                                                clock_id=1, delivery_time_ns=start+1_000_000, input_time_ns=start)),
                ('tilo-physical-input ', dict(id=n, kind='key_pressed', wayland_serial=n+9, process_id=123, observer_session=session,
                                             before=state(n-1), after=state(n), clock='clock_monotonic_seconds',
                                             dispatch_lo_s=(start+2_000_000)/1e9, dispatch_hi_s=(start+2_100_000)/1e9)),
                ('tilo-physical-frame ', dict(cpu_frame_id=n, input_ids=[n], state=state(n), process_id=123, observer_session=session)),
                ('tilo-display-frame ', dict(backend='wayland', cpu_frame_id=n, clock_id=1, process_id=123, observer_session=session,
                                            flags=7, discarded=False, presented_time_ns=start+10_000_000)),
            ]
        return result

    def report(self, rows):
        return summarize_physical_notes_latency([prefix+json.dumps(row) for prefix,row in rows], 'scratch-only')

    def test_exact_changed_body_hardware_flip_is_required(self):
        report = self.report(self.timeline())
        self.assertTrue(report['latency_complete'])
        self.assertEqual([r['latency_ms'] for r in report['samples']], [10, 10, 10])
        self.assertNotIn('performance_certified', report)
        # An unrelated background worker's same local frame ID is not this
        # scratch buffer's presentation, even with the same sandbox PID.
        # The app worker lifetime UUID disambiguates actual producers.
        timeline = self.timeline()+[('tilo-display-frame ', dict(backend='wayland',
                     cpu_frame_id=1, process_id=123, observer_session='d18a8ac3-c453-437f-b6d6-3fef3f242154', clock_id=1, flags=7,
                     discarded=False, presented_time_ns=1))]
        self.assertTrue(self.report(timeline)['latency_complete'])

    def test_discarded_buffer_needs_an_exact_drawn_and_presented_successor(self):
        timeline = copy.deepcopy(self.timeline()[:4])
        first_feedback = timeline[3][1]
        first_feedback.clear()
        first_feedback.update(backend='wayland', cpu_frame_id=1, process_id=123,
                              observer_session=timeline[1][1]['observer_session'], discarded=True)
        redraw = copy.deepcopy(timeline[2][1])
        redraw['cpu_frame_id'] = 5
        shown = copy.deepcopy(self.timeline()[3][1])
        shown['cpu_frame_id'] = 5
        timeline += [('tilo-physical-frame ', redraw), ('tilo-display-frame ', shown)]
        report = self.report(timeline)
        self.assertEqual(report['samples'][0]['cpu_frame_id'], 5)
        self.assertEqual(report['samples'][0]['latency_ms'], 10)
        for mutation in ('missing_feedback', 'different_body', 'no_positive_feedback'):
            with self.subTest(mutation=mutation):
                invalid = copy.deepcopy(timeline)
                if mutation == 'missing_feedback':
                    invalid.pop(3)
                elif mutation == 'different_body':
                    invalid[2][1]['state']['body_hash'] = 'f' * 64
                else:
                    invalid[-1][1]['discarded'] = True
                with self.assertRaises(ValueError):
                    self.report(invalid)

    def test_indirect_missing_coalesced_wrong_note_and_foreign_effects_fail(self):
        for prefix, change in [
            ('tilo-physical-delivery ', dict(clock_id=0)),
            ('tilo-physical-delivery ', dict(input_time_ns=None)),
            ('tilo-physical-delivery ', dict(input_time_ns=999_000_000)),
            ('tilo-physical-input ', dict(dispatch_lo_s=None)),
            ('tilo-physical-input ', dict(wayland_serial=None)),
            ('tilo-physical-input ', dict(observer_session=None)),
            ('tilo-physical-input ', dict(observer_session='d18a8ac3-c453-437f-b6d6-3fef3f242154')),
            ('tilo-physical-frame ', dict(observer_session=None)),
            ('tilo-physical-frame ', dict(observer_session='d18a8ac3-c453-437f-b6d6-3fef3f242154')),
            ('tilo-display-frame ', dict(observer_session=None)),
            ('tilo-display-frame ', dict(observer_session='d18a8ac3-c453-437f-b6d6-3fef3f242154')),
            ('tilo-physical-input ', dict(kind='ime_commit')),
            ('tilo-physical-input ', dict(after={})),
            ('tilo-physical-frame ', dict(input_ids=[1, 2])),
            ('tilo-physical-frame ', dict(cpu_frame_id=99)),
            ('tilo-physical-frame ', dict(process_id=999)),
            ('tilo-physical-frame ', dict(state={})),
            ('tilo-display-frame ', dict(discarded=True)),
            ('tilo-display-frame ', dict(flags=1)),
            ('tilo-display-frame ', dict(flags=None)),
            ('tilo-display-frame ', dict(presented_time_ns=1)),
        ]:
            with self.subTest(prefix=prefix, change=change):
                timeline = copy.deepcopy(self.timeline())
                next(row for p,row in timeline if p == prefix).update(change)
                with self.assertRaises(ValueError):
                    self.report(timeline)
        for prefix in ['tilo-physical-delivery ', 'tilo-physical-input ',
                       'tilo-physical-frame ', 'tilo-display-frame ']:
            with self.subTest(missing=prefix):
                with self.assertRaises(ValueError):
                    self.report([(p,r) for p,r in self.timeline() if p != prefix])
                with self.assertRaises(ValueError):
                    self.report(self.timeline()+[next((p,r) for p,r in self.timeline() if p == prefix)])


if __name__ == '__main__':
    unittest.main()

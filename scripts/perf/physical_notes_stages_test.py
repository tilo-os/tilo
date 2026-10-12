import copy
import json
import unittest

import physical_notes_latency_test as physical_fixture
from main_stages import POINTS
from physical_notes_stages import summarize_physical_notes_stages


class PhysicalNotesStages(unittest.TestCase):
    def fixture(self):
        result = physical_fixture.PhysicalNotesLatency().timeline()
        for prefix, row in result:
            if prefix == 'tilo-display-frame ':
                row.update(worker_id=8, worker_frame_ix=row['cpu_frame_id'])
        for index in range(1, 4):
            origin = index / 10
            identity = dict(process_id=123,
                            observer_session='2bd78136-4c17-4853-9d7c-7442274a7059',
                            cpu_frame_id=index, host_clock='clock_monotonic_seconds')
            def stamps(**values):
                return {point + '_host_' + bound + '_s': origin + value
                        for point, value in values.items() for bound in ('lo', 'hi')}
            result.extend([
                ('tilo-native-send ', dict(identity, send_ok=True,
                                          **stamps(before=.005, after=.0055))),
                ('tilo-native-worker ', dict(identity, worker_id=8, worker_frame_ix=index,
                    outcome='Presented', **stamps(receive=.0051, render_start=.006, render_end=.009))),
                ('tilo-native-present-call ', dict(identity, worker_id=8, worker_frame_ix=index,
                                                  **stamps(before=.008, after=.0081))),
                ('tilo-native-main-stages ', dict(identity, **stamps(
                    **{point: .003 + number * .0001 for number, point in enumerate(POINTS)}))),
            ])
        return result

    def report(self, records):
        return summarize_physical_notes_stages(
            [prefix + json.dumps(row) for prefix, row in records], 'scratch-only')

    def test_complete_clock_and_producer_join_reports_cpu_stages(self):
        report = self.report(self.fixture())
        self.assertTrue(report['available'])
        self.assertTrue(report['main_stages_available'])
        self.assertFalse(report['performance_certified'])
        self.assertEqual(len(report['frames']), 3)
        for key, expected in [('kernel_to_delivery', 1), ('delivery_to_client', 1.1),
                              ('client_entry_to_send', 3), ('worker_render_cpu', 3),
                              ('present_call_cpu', .1), ('present_return_to_display', 1.9)]:
            self.assertAlmostEqual(report['stages'][key]['upper']['p50_ms'], expected)

    def test_foreign_process_lifetime_cannot_supply_missing_stage(self):
        for changes in [dict(process_id=999), dict(observer_session=
                        'd18a8ac3-c453-437f-b6d6-3fef3f242154')]:
            with self.subTest(changes=changes):
                records = self.fixture()
                row = next(row for prefix, row in records if prefix == 'tilo-native-worker ')
                extra = copy.deepcopy(row)
                extra.update(changes)
                self.assertTrue(self.report(records + [('tilo-native-worker ', extra)])['available'])
                row.update(changes)
                with self.assertRaises(ValueError):
                    self.report(records)

    def test_partial_duplicate_wrong_clock_or_identity_fails(self):
        for prefix, changes in [
            ('tilo-native-send ', dict(send_ok=False)),
            ('tilo-native-send ', dict(host_clock='mach_absolute_seconds')),
            ('tilo-native-worker ', dict(outcome='Occluded')),
            ('tilo-native-worker ', dict(worker_id=9)),
            ('tilo-native-worker ', dict(receive_host_lo_s=.01)),
            ('tilo-native-worker ', dict(render_end_host_hi_s=float('nan'))),
            ('tilo-native-present-call ', dict(worker_frame_ix=99)),
            ('tilo-native-present-call ', dict(before_host_lo_s=True)),
            ('tilo-native-main-stages ', dict(before_profiler_commit_host_lo_s=.001)),
            ('tilo-display-frame ', dict(discarded=True)),
        ]:
            with self.subTest(prefix=prefix, changes=changes):
                records = self.fixture()
                next(row for tag, row in records if tag == prefix).update(changes)
                with self.assertRaises(ValueError):
                    self.report(records)
        for prefix in ('tilo-native-send ', 'tilo-native-worker ',
                       'tilo-native-present-call ', 'tilo-native-main-stages '):
            with self.subTest(prefix=prefix):
                records = self.fixture()
                first = next(record for record in records if record[0] == prefix)
                with self.assertRaises(ValueError):
                    self.report(records + [first])
                records.remove(first)
                with self.assertRaises(ValueError):
                    self.report(records)

    def test_same_render_frame_cannot_be_reused_for_another_input(self):
        records = self.fixture()
        for prefix, row in records:
            if prefix in ('tilo-native-worker ', 'tilo-native-present-call ', 'tilo-display-frame '):
                row['worker_frame_ix'] = 1
        with self.assertRaises(ValueError):
            self.report(records)

    def test_main_stages_are_explicitly_unavailable_on_older_client(self):
        records = [record for record in self.fixture() if record[0] != 'tilo-native-main-stages ']
        report = self.report(records)
        self.assertFalse(report['main_stages_available'])
        self.assertFalse(report['performance_certified'])
        self.assertNotIn('profiler_commit', report['stages'])


class ForeignCaptureJoin(unittest.TestCase):
    def fixture(self):
        from foreign_capture import PREFIXES
        points = ('prepare_ns enqueue_ns worker_start_ns fence_start_ns fence_end_ns '
                  'native_import_end_ns copy_submit_ns copy_submit_return_ns copy_end_ns '
                  'staged_ns barrier_ns').split()
        imported = dict(event='imported', clock_id=1, surface_id=42, capture_token=8,
                        accepted=True, shared_gpu=True, barrier_return_ns=20500,
                        **{key: 10000 + index * 1000 for index, key in enumerate(points)})
        activated = dict(event='activated', clock_id=1, surface_id=42, capture_token=8,
                         capture_seq=3, accepted=True, activated_ns=21000)
        common = dict(clock_id=1, pipe_serial=41, surface_id=42, capture_seq=3,
                      composed_time_ns=22000, omitted_pixels=0)
        submitted = dict(common, event='submitted', event_time_ns=23500,
                         physical_presented_time_ns=None, physical_sequence=None)
        completed = dict(common, event='completed', event_time_ns=1000001,
                         physical_presented_time_ns=1000000, physical_sequence=789)
        pipe = dict(event='retired', clock_id=1, pipe_serial=41, enqueue_ns=23000,
                    render_start_ns=24000, render_end_ns=25000)
        return [(PREFIXES[0], imported), (PREFIXES[0], activated),
                (PREFIXES[1], submitted), (PREFIXES[1], completed), (PREFIXES[2], pipe)]

    def report(self, records, **changes):
        from foreign_capture import summarize_foreign_capture
        arguments = dict(
            lines=[tag + json.dumps(row) for tag, row in records],
            compositor_frames=[dict(cpu_frame_id=7, compositor_match_available=True, serial=41)],
            samples=[dict(input_id=1, cpu_frame_id=7)],
            displays={7: dict(presented_time_ns=1000000, sequence=789)},
            deliveries={9: dict(surface_id=42)}, events={1: dict(wayland_serial=9)},
            presents={7: dict(after_host_lo_s=.000001, after_host_hi_s=.000002)}, required=True)
        arguments.update(changes)
        return summarize_foreign_capture(**arguments)

    def test_exact_capture_ownership_separates_every_observed_wait(self):
        report = self.report(self.fixture())
        self.assertTrue(report['complete'])
        self.assertFalse(report['performance_certified'])
        frame = report['frames'][0]
        self.assertEqual((frame['capture_token'], frame['capture_seq'], frame['pipe_serial']), (8, 3, 41))
        self.assertAlmostEqual(frame['intervals_ms']['owned_copy_wait'][0], .001)
        self.assertAlmostEqual(frame['intervals_ms']['barrier_to_activation'][0], .0005)
        self.assertAlmostEqual(frame['intervals_ms']['barrier_to_activation'][1], .001)
        self.assertAlmostEqual(frame['intervals_ms']['present_return_to_capture_prepare'][0], .008)

    def test_one_capture_token_cannot_claim_two_activated_generations(self):
        fixture = self.fixture()
        another = dict(fixture[1][1], capture_seq=4)
        with self.assertRaises(ValueError):
            self.report(fixture + [(fixture[1][0], another)])

    def test_direct_physical_join_rejects_another_presenter_owner(self):
        fixture = self.fixture()
        another = dict(fixture[3][1], pipe_serial=99)
        with self.assertRaises(ValueError):
            self.report(fixture + [(fixture[3][0], another)], compositor_frames=None)

    def test_borrowed_buffer_import_has_no_fictional_capture_copy(self):
        fixture = self.fixture()
        fixture[0][1].update(borrowed_gpu=True, fence_waited=False,
                             copy_submit_ns=None, copy_submit_return_ns=None, copy_end_ns=None)
        report = self.report(fixture, compositor_frames=None)
        self.assertTrue(report['complete'])
        self.assertTrue(report['frames'][0]['borrowed_gpu'])
        self.assertIn('cached_native_import', report['stages'])
        self.assertNotIn('owned_copy_wait', report['stages'])
        fixture[0][1]['copy_end_ns'] = 19000
        with self.assertRaises(ValueError):
            self.report(fixture)

    def test_absent_partial_duplicate_stale_and_omitted_evidence_never_passes(self):
        fixture = self.fixture()
        for index in range(len(fixture)):
            with self.subTest(missing=index):
                with self.assertRaises(ValueError): self.report(fixture[:index] + fixture[index + 1:])
            with self.subTest(duplicate=index):
                with self.assertRaises(ValueError): self.report(fixture + [fixture[index]])
        for index, change in [(0, dict(shared_gpu=False)), (0, dict(copy_end_ns=None)),
                              (0, dict(fence_end_ns=1)), (1, dict(capture_token=9)),
                              (1, dict(accepted=False)), (2, dict(omitted_pixels=1)),
                              (2, dict(composed_time_ns=999)), (3, dict(surface_id=99)),
                              (3, dict(physical_sequence=788)), (4, dict(clock_id=0)), (4, dict(clock_id=True))]:
            with self.subTest(index=index, change=change):
                rows = self.fixture()
                rows[index][1].update(change)
                with self.assertRaises(ValueError): self.report(rows)
        with self.assertRaises(ValueError): self.report([])
        with self.assertRaises(ValueError): self.report(fixture, compositor_frames=[])
        self.assertFalse(self.report([], required=False)['available'])


if __name__ == '__main__':
    unittest.main()

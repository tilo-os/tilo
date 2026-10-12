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


if __name__ == '__main__':
    unittest.main()

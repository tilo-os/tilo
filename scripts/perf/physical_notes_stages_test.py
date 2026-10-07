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


class CompositorPhaseJoin(unittest.TestCase):
    def report(self, lines):
        from compositor_phases import summarize_compositor_phases
        samples = [dict(input_id=1, cpu_frame_id=7)]
        feedback = {7: dict(presented_time_ns=123000456000, sequence=789,
                            clock_id=1, discarded=False, flags=7)}
        return summarize_compositor_phases(lines, samples, feedback)

    def line(self, **changes):
        row = dict(serial=41, event='render-retired', revision=73, phase_timing='true',
                   queue_wait_us=5, render_us=50, total_us=55,
                   encode_submit_us=7, gpu_completion_wait_us=9, bump_settle_us=2,
                   backdrop_us=0, kms_retirement_us=19, cpu_copy_us=4,
                   readback_bytes=4096, output_bytes=2048,
                   physical_presented_time_ns=123000456000, physical_sequence=789)
        row.update(changes)
        return 'FRAME-LIFECYCLE ' + ' '.join(f'{key}={value}' for key, value in row.items())

    def test_exact_physical_outcome_matches_existing_costs_and_keeps_background_coverage(self):
        result = self.report([self.line(), self.line(serial=42, physical_sequence=790)])
        self.assertFalse(result['performance_certified'])
        self.assertTrue(result['complete'])
        self.assertEqual(result['coverage']['exact_outcome_matches'], 1)
        self.assertEqual(result['coverage']['unmatched_lifecycle_records'], 1)
        self.assertEqual(result['frames'][0]['serial'], 41)
        self.assertEqual(result['frames'][0]['revision'], 73)
        self.assertEqual(result['frames'][0]['byte_counts']['readback_bytes'], 4096)
        self.assertEqual(result['costs']['kms_retirement_us']['p50_ms'], .019)
        self.assertEqual(result['frames'][0]['readback_classification'], 'compositor_readback_reported')

    def test_measured_zero_readback_is_distinct_from_unsupported_or_missing(self):
        direct = self.report([self.line(readback_bytes=0, output_bytes=0, cpu_copy_us=0)])
        self.assertTrue(direct['complete'])
        self.assertEqual(direct['frames'][0]['readback_classification'], 'no_compositor_readback_reported')
        absent = self.report([self.line(phase_timing='false', encode_submit_us=0,
                                       gpu_completion_wait_us=0, bump_settle_us=0,
                                       kms_retirement_us=0, cpu_copy_us=0,
                                       readback_bytes=0, output_bytes=0)])
        self.assertFalse(absent['available'])
        self.assertEqual(absent['coverage']['exact_outcome_matches'], 1)
        self.assertNotIn('readback_classification', absent['frames'][0])
        self.assertFalse(self.report([])['complete'])

    def test_time_proximity_wrong_sequence_and_headless_do_not_match(self):
        for changes in [dict(physical_presented_time_ns=123000456001),
                        dict(physical_sequence=788),
                        dict(physical_presented_time_ns='none', physical_sequence='none')]:
            with self.subTest(changes=changes):
                result = self.report([self.line(**changes)])
                self.assertFalse(result['available'])
                self.assertEqual(result['coverage']['exact_outcome_matches'], 0)

    def test_partial_ambiguous_negative_unavailable_and_duration_evidence_fails(self):
        for changes in [dict(physical_sequence='none'), dict(serial=-1),
                        dict(cpu_copy_us=-1), dict(total_us=54),
                        dict(phase_timing='false'), dict(phase_timing='yes')]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.report([self.line(**changes)])
        for second in [self.line(), self.line(serial=42)]:
            with self.assertRaises(ValueError):
                self.report([self.line(), second])

    def test_untrusted_feedback_and_capture_bounds_fail(self):
        from compositor_phases import summarize_compositor_phases, capture_lines
        from unittest.mock import patch
        from tempfile import TemporaryDirectory
        from pathlib import Path
        for changes in [dict(clock_id=0), dict(discarded=True), dict(flags=1)]:
            display = dict(presented_time_ns=123000456000, sequence=789,
                           clock_id=1, discarded=False, flags=7)
            display.update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                summarize_compositor_phases([self.line()], [dict(input_id=1, cpu_frame_id=7)], {7: display})
        with patch('compositor_phases.MAX_RECORDS', 1), self.assertRaises(ValueError):
            self.report([self.line(), self.line(serial=42, physical_sequence=790)])
        with TemporaryDirectory() as directory:
            source = Path(directory) / 'capture.log'
            source.write_bytes(b'\xff')
            with self.assertRaisesRegex(ValueError, 'encoding'):
                capture_lines(source)
            source.write_bytes(b'12345')
            with patch('compositor_phases.MAX_CAPTURE_BYTES', 4), self.assertRaisesRegex(ValueError, 'byte bound'):
                capture_lines(source)

    def test_older_records_cannot_supply_a_physical_or_readback_join(self):
        line = self.line().split(' cpu_copy_us=')[0]
        result = self.report([line])
        self.assertFalse(result['complete'])
        self.assertEqual(result['coverage']['records_without_physical_outcome'], 1)
        partial = self.line().replace(' readback_bytes=4096', '')
        result = self.report([partial])
        self.assertEqual(result['coverage']['exact_outcome_matches'], 1)
        self.assertFalse(result['available'])

    def test_existing_verified_notes_pipeline_receives_compositor_join(self):
        fixture = PhysicalNotesStages().fixture()
        lines = []
        for prefix, row in fixture:
            if prefix == 'tilo-display-frame ':
                row['sequence'] = 788 + row['cpu_frame_id']
                lines.append(self.line(serial=40 + row['cpu_frame_id'],
                                       physical_presented_time_ns=row['presented_time_ns'],
                                       physical_sequence=row['sequence']))
        result = summarize_physical_notes_stages(
            [prefix + json.dumps(row) for prefix, row in fixture], 'scratch-only', lines)
        self.assertTrue(result['compositor']['complete'])
        self.assertEqual(result['compositor']['coverage']['verified_notes_frames'], 3)


if __name__ == '__main__':
    unittest.main()

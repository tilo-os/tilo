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

    def test_native_present_boundary_separates_wrapper_and_driver_wall_time(self):
        records = self.fixture()
        for prefix, row in records:
            if prefix == 'tilo-native-present-call ':
                row.update(native_before_host_lo_s=row['before_host_lo_s'] + .00002,
                           native_before_host_hi_s=row['before_host_hi_s'] + .00002,
                           native_after_host_lo_s=row['after_host_lo_s'] - .00001,
                           native_after_host_hi_s=row['after_host_hi_s'] - .00001)
        result = self.report(records)
        self.assertTrue(all(frame['native_present_samples_available'] for frame in result['frames']))
        self.assertAlmostEqual(result['stages']['present_wrapper_before_native']['upper']['p50_ms'], .02)
        self.assertAlmostEqual(result['stages']['native_present_cpu']['upper']['p50_ms'], .07)
        self.assertAlmostEqual(result['stages']['present_wrapper_after_native']['upper']['p50_ms'], .01)
        changed = copy.deepcopy(records)
        row = next(row for prefix, row in changed if prefix == 'tilo-native-present-call ')
        row['native_before_host_lo_s'] = row['before_host_lo_s'] - .001
        with self.assertRaises(ValueError):
            self.report(changed)
        changed = copy.deepcopy(records)
        row = next(row for prefix, row in changed if prefix == 'tilo-native-present-call ')
        del row['native_after_host_hi_s']
        with self.assertRaises(ValueError):
            self.report(changed)
        row = next(row for prefix, row in records if prefix == 'tilo-native-present-call ')
        for key in list(row):
            if key.startswith('native_'):
                row.pop(key)
        mixed = self.report(records)
        self.assertFalse(mixed['frames'][0]['native_present_samples_available'])
        self.assertNotIn('native_present_cpu', mixed['stages'])

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


class NativeGpuJoin(unittest.TestCase):
    fixture = PhysicalNotesStages.fixture
    report = PhysicalNotesStages.report

    def gpu(self, index=1, kind='passes', event='attempt', **changes):
        row = dict(process_id=123, observer_session='2bd78136-4c17-4853-9d7c-7442274a7059',
                   cpu_frame_id=index, worker_id=8, worker_frame_ix=index,
                   kind=kind, event=event, supported=True, pending=event == 'attempt',
                   matched_current=event == 'completed', missing_current=event != 'completed',
                   stale_sample=False, delayed=False, expected_current_submission_id=100 + index,
                   latest_submission_id=100 + index,
                   sample_submission_id=100 + index if event == 'completed' else None,
                   sample=None)
        if event == 'completed':
            row['sample'] = dict(pass_count=3, span_ns=1000.5, summed_pass_ns=1500.25) if kind == 'passes' else dict(
                stage_count=1, stages=[dict(stage_id=20, stage='fine_area', dispatch_count=2,
                                          duration_ns='900')])
        row.update(changes)
        return 'tilo-native-gpu-stages ', row

    def complete(self):
        return self.fixture() + [self.gpu(index, kind, event)
                                 for index in range(1, 4)
                                 for kind in ('passes', 'dispatches')
                                 for event in ('attempt', 'completed')]

    def test_exact_delayed_recording_is_associated_with_its_original_frame(self):
        records = self.complete()
        for prefix, row in records:
            if prefix == 'tilo-native-gpu-stages ' and row['event'] == 'completed':
                row.update(latest_submission_id=999, delayed=True)
        report = self.report(records)
        gpu = report['native_gpu']
        self.assertTrue(gpu['complete'])
        self.assertFalse(gpu['performance_certified'])
        self.assertEqual(gpu['clock'], 'device_relative_duration_ns')
        self.assertEqual(gpu['frames'][0]['kinds']['passes']['submission_id'], 101)
        self.assertTrue(gpu['frames'][0]['kinds']['passes']['delayed'])
        self.assertAlmostEqual(gpu['pass_costs']['span_ns']['p50_ms'], .0010005)
        self.assertAlmostEqual(gpu['pass_costs']['summed_pass_ns']['p50_ms'], .00150025)
        self.assertAlmostEqual(gpu['dispatch_costs'][0]['duration']['p50_ms'], .0009)
        self.assertEqual(report['stages'], self.report(self.fixture())['stages'])

    def test_internal_recording_range_keeps_samples_distinct_and_reports_query_gaps(self):
        records = self.complete()
        for prefix, row in records:
            if prefix == 'tilo-native-gpu-stages ':
                identity = row['expected_current_submission_id']
                row.update(recording_range_first=identity, recording_range_last=identity + 1)
                if row['event'] == 'attempt':
                    row['expected_current_submission_id'] = identity + 1
        gpu = self.report(records)['native_gpu']
        self.assertTrue(gpu['complete'])
        evidence = gpu['frames'][0]['kinds']['dispatches']
        self.assertEqual(evidence['measured_recordings'], 1)
        self.assertEqual(evidence['allocated_recordings'], 2)
        self.assertFalse(evidence['all_recordings_measured'])
        second = self.gpu(kind='dispatches', event='completed',
                          sample_submission_id=102, expected_current_submission_id=102,
                          latest_submission_id=102, recording_range_first=101,
                          recording_range_last=102)
        gpu = self.report(records + [second])['native_gpu']
        evidence = gpu['frames'][0]['kinds']['dispatches']
        self.assertEqual([sample['submission_id'] for sample in evidence['samples']], [101, 102])
        self.assertNotIn('sample', evidence)
        self.assertTrue(evidence['all_recordings_measured'])
        with self.assertRaisesRegex(ValueError, 'Ambiguous'):
            self.report(records + [second, second])
        for changes in (dict(recording_range_first=102), dict(recording_range_last=103)):
            bad = self.gpu(kind='dispatches', event='completed',
                           recording_range_first=101, recording_range_last=102, **{})
            bad[1].update(changes)
            other = [record for record in records if not (record[0] == 'tilo-native-gpu-stages '
                     and record[1]['kind'] == 'dispatches' and record[1]['event'] == 'completed'
                     and record[1]['cpu_frame_id'] == bad[1]['cpu_frame_id'])]
            self.assertFalse(self.report(other + [bad])['native_gpu']['complete'])

    def test_missing_unsupported_stale_partial_and_evicted_are_explicit(self):
        for changes, status in [(dict(supported=False), 'unsupported'),
                                (dict(stale_sample=True), 'stale_sample'),
                                (dict(event='owner_evicted'), 'owner_evicted')]:
            with self.subTest(status=status):
                gpu = self.report(self.fixture() + [self.gpu(**changes)])['native_gpu']
                self.assertFalse(gpu['available'])
                self.assertFalse(gpu['complete'])
                self.assertEqual(gpu['frames'][0]['kinds']['passes']['status'], status)
                self.assertEqual(gpu['pass_costs'], {})
        partial = self.report(self.fixture() + [self.gpu(), self.gpu(event='completed')])['native_gpu']
        self.assertTrue(partial['available'])
        self.assertFalse(partial['complete'])
        self.assertEqual(partial['frames'][0]['kinds']['dispatches']['status'], 'missing_current')
        self.assertFalse(self.report(self.fixture())['native_gpu']['available'])

    def test_other_lifetime_worker_or_cpu_cannot_supply_gpu_evidence(self):
        for changes in [dict(process_id=456), dict(observer_session='another-worker-lifetime'),
                        dict(worker_id=9), dict(worker_frame_ix=2), dict(cpu_frame_id=99)]:
            with self.subTest(changes=changes):
                gpu = self.report(self.fixture() + [self.gpu(**changes),
                                  self.gpu(event='completed', **changes)])['native_gpu']
                self.assertFalse(gpu['available'])
                self.assertEqual(gpu['coverage']['excluded_identity_records'], 2)
                lifetime = 'process_id' in changes or 'observer_session' in changes
                self.assertEqual(gpu['coverage']['excluded_other_lifetime_records'], 2 if lifetime else 0)
                self.assertEqual(gpu['coverage']['excluded_other_frame_records'], 0 if lifetime else 2)
        gpu = self.report(self.fixture() + [self.gpu(event='completed', cpu_frame_id=None,
                          worker_id=None, worker_frame_ix=None, stale_sample=True,
                          matched_current=False, missing_current=True)])['native_gpu']
        self.assertEqual(gpu['coverage']['unassigned_stale_samples'], 1)
        self.assertFalse(gpu['available'])

    def test_missing_or_mismatched_recording_owner_does_not_match(self):
        for extra in [[self.gpu(event='completed')],
                      [self.gpu(), self.gpu(event='completed', sample_submission_id=99)]]:
            gpu = self.report(self.fixture() + extra)['native_gpu']
            self.assertFalse(gpu['available'])
            self.assertEqual(gpu['frames'][0]['kinds']['passes']['status'], 'unmatched_recording')

    def test_duplicate_or_inconsistent_identity_is_rejected(self):
        for event in ('attempt', 'completed'):
            records = self.complete() + [self.gpu(event=event)]
            with self.subTest(event=event), self.assertRaisesRegex(ValueError, 'Ambiguous'):
                self.report(records)
        for changes in [dict(cpu_frame_id=True), dict(latest_submission_id=-1),
                        dict(latest_submission_id=999, delayed=False), dict(supported='true')]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.report(self.fixture() + [self.gpu(), self.gpu(event='completed', **changes)])

    def test_invalid_device_duration_and_partial_stage_payload_is_rejected(self):
        samples = [dict(pass_count=True, span_ns=1000, summed_pass_ns=1500),
                   dict(pass_count=3, span_ns=-1, summed_pass_ns=1500),
                   dict(pass_count=3, span_ns=float('nan'), summed_pass_ns=1500),
                   dict(pass_count=3, span_ns=float('inf'), summed_pass_ns=1500),
                   dict(pass_count=3, span_ns=True, summed_pass_ns=1500)]
        for sample in samples:
            with self.subTest(sample=sample), self.assertRaises(ValueError):
                self.report(self.fixture() + [self.gpu(), self.gpu(event='completed', sample=sample)])
        base = self.gpu(kind='dispatches', event='completed')[1]['sample']
        for changes in [dict(duration_ns='-1'), dict(duration_ns='1e3'), dict(duration_ns=900),
                        dict(duration_ns='9' * 40), dict(stage='private text'), dict(dispatch_count=True)]:
            sample = copy.deepcopy(base)
            sample['stages'][0].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.report(self.fixture() + [self.gpu(kind='dispatches'),
                            self.gpu(kind='dispatches', event='completed', sample=sample)])
        for sample in [dict(stage_count=2, stages=base['stages']),
                       dict(stage_count=2, stages=base['stages'] * 2)]:
            with self.subTest(sample=sample), self.assertRaises(ValueError):
                self.report(self.fixture() + [self.gpu(kind='dispatches'),
                            self.gpu(kind='dispatches', event='completed', sample=sample)])

    def test_gpu_evidence_cannot_rescue_uncertified_physical_feedback(self):
        records = self.complete()
        next(row for prefix, row in records if prefix == 'tilo-display-frame ').update(discarded=True)
        with self.assertRaises(ValueError):
            self.report(records)

    def test_gpu_evidence_is_bounded(self):
        from unittest.mock import patch
        with patch('native_gpu_evidence.MAX_ROWS', 1), self.assertRaisesRegex(ValueError, 'bound'):
            self.report(self.fixture() + [self.gpu(), self.gpu(event='completed')])


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

    def test_submission_boundaries_join_only_exact_serial_without_changing_costs(self):
        from compositor_submission_test import record
        result = self.report([self.line(), record(), record(42)])
        frame = result['frames'][0]
        self.assertEqual(frame['serial'], 41)
        self.assertTrue(frame['submission_cpu']['available'])
        self.assertEqual(frame['submission_cpu']['costs_us']['scene_record_us'], 3)
        self.assertEqual(frame['costs_us']['encode_submit_us'], 7)
        self.assertEqual(self.report([self.line()])['frames'][0]['submission_cpu']['status'], 'missing')
        with self.assertRaises(ValueError):
            self.report([self.line(), record(scene_record_us=8)])

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

    def test_gpu_percentiles_use_only_exact_complete_frames_and_keep_nested_intervals(self):
        from compositor_phases import summarize_compositor_phases
        from compositor_submission_test import gpu_record
        samples = [dict(input_id=index, cpu_frame_id=index) for index in range(1, 5)]
        feedback = {index: dict(presented_time_ns=100 + index, sequence=index,
                               clock_id=1, discarded=False, flags=7) for index in range(1, 5)}
        lines = []
        for index, duration in enumerate((0, 1000000, 2000000, 4000000), 1):
            lines.extend([self.line(serial=index, revision=9,
                physical_presented_time_ns=100 + index, physical_sequence=index),
                gpu_record(serial=index, first_recording=index, last_recording=index,
                           recording_ns=duration, post_render_ns=duration // 2)])
        # Valid background GPU work is never attributed to these input frames.
        lines.extend([self.line(serial=99, revision=9, physical_sequence=99),
                      gpu_record(serial=99, first_recording=99, last_recording=99,
                                 recording_ns=900000000, post_render_ns=800000000)])
        report = summarize_compositor_phases(lines, samples, feedback)
        self.assertTrue(report['gpu_available'])
        self.assertTrue(report['gpu_complete'])
        self.assertEqual(report['gpu_clock'], 'device_relative_duration_ns')
        self.assertEqual(report['coverage']['gpu_samples_matched'], 4)
        self.assertEqual(report['coverage']['unmatched_gpu_records'], 1)
        recording = report['gpu_costs']['recording_ns']
        self.assertEqual(recording['n'], 4)
        self.assertEqual(recording['min_ms'], 0)
        self.assertEqual(recording['p50_ms'], 1)
        self.assertEqual(recording['p95_ms'], 4)
        self.assertEqual(recording['p99_ms'], 4)
        self.assertEqual(report['gpu_costs']['post_render_ns']['p99_ms'], 2)
        self.assertAlmostEqual(report['costs']['gpu_completion_wait_us']['p99_ms'], .009)
        partial_lines = [line for line in lines if not line.startswith('FRAME-GPU-SUBMISSION serial=4 ')]
        partial = summarize_compositor_phases(partial_lines, samples, feedback)
        self.assertTrue(partial['gpu_available'])
        self.assertFalse(partial['gpu_complete'])
        self.assertEqual(partial['coverage']['gpu_samples_matched'], 3)
        self.assertEqual(partial['gpu_costs']['recording_ns']['n'], 3)
        self.assertEqual(partial['gpu_costs']['recording_ns']['p99_ms'], 2)

    def test_missing_partial_or_foreign_gpu_work_never_becomes_measured_zero(self):
        from compositor_submission_test import gpu_record
        for records in ([], [gpu_record(serial=42)],
                        [gpu_record(revision=73, status='incomplete',
                                    recording_count=2, last_recording=4)]):
            with self.subTest(records=records):
                report = self.report([self.line()] + records)
                self.assertTrue(report['complete'])
                self.assertFalse(report['gpu_available'])
                self.assertFalse(report['gpu_complete'])
                self.assertEqual(report['gpu_costs'], {})
                self.assertEqual(report['coverage']['gpu_samples_matched'], 0)
        with self.assertRaisesRegex(ValueError, 'another frame revision'):
            self.report([self.line(), gpu_record(revision=72)])

    def test_actual_render_decisions_are_optional_and_exact_outcome_owned(self):
        result = self.report([self.line(render_work='true', target_width=1920,
            target_height=1080, render_attempts=2, sparse_attempts=1,
            full_attempts=1, damage_admitted='false')])
        work = result['frames'][0]['render_work']
        self.assertEqual(work['sparse_attempts'], 1)
        self.assertEqual(work['full_attempts'], 1)
        self.assertFalse(work['damage_admitted'])
        self.assertIsNone(self.report([self.line()])['frames'][0]['render_work'])
        for changes in (dict(render_attempts=3), dict(target_width=0),
                        dict(damage_admitted='unknown'), dict(render_work='false')):
            with self.subTest(changes=changes):
                work = dict(render_work='true', target_width=1920, target_height=1080,
                            render_attempts=2, sparse_attempts=1, full_attempts=1,
                            damage_admitted='false')
                work.update(changes)
                with self.assertRaises(ValueError):
                    self.report([self.line(**work)])

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

    def test_encoder_finish_and_queue_submit_have_separate_exact_brackets(self):
        records = self.fixture()
        capture = records[0][1]
        capture.update(copy_encoder_finish_return_ns=16500, fence_ready=True)
        breakdown = self.report(records)['frames'][0]['copy_submit_breakdown']
        self.assertTrue(breakdown['available'])
        self.assertAlmostEqual(breakdown['encoder_finish_ms'], .0005)
        self.assertAlmostEqual(breakdown['queue_submit_ms'], .0005)
        for value in (15999, 17001):
            capture['copy_encoder_finish_return_ns'] = value
            with self.assertRaisesRegex(ValueError, 'encoder finish'):
                self.report(records)
        capture['copy_encoder_finish_return_ns'] = 16500
        capture['fence_ready'] = False
        with self.assertRaisesRegex(ValueError, 'fence'):
            self.report(records)
        self.assertFalse(self.report(self.fixture())['frames'][0]['copy_submit_breakdown']['available'])

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


class PhysicalSettingsStages(unittest.TestCase):
    def fixture(self, hover=False):
        baseline = PhysicalNotesStages().fixture()
        family = (physical_fixture.PhysicalSettingsHoverLatency if hover
                  else physical_fixture.PhysicalSettingsLatency)().timeline()
        for prefix, row in family:
            if prefix == 'tilo-display-frame ':
                row.update(worker_id=8, worker_frame_ix=row['cpu_frame_id'])
        return family + [(prefix, row) for prefix, row in baseline
                         if prefix.startswith('tilo-native-')
                         and (not hover or row['cpu_frame_id'] == 1)]

    def report(self, records, hover=False):
        from physical_notes_stages import summarize_physical_settings_stages
        return summarize_physical_settings_stages(
            [prefix + json.dumps(row) for prefix, row in records], 'bluetooth', hover=hover)

    def test_keyboard_and_pointer_keep_exact_cpu_and_physical_frame_join(self):
        for hover, count in ((False, 3), (True, 1)):
            with self.subTest(hover=hover):
                result = self.report(self.fixture(hover), hover)
                self.assertEqual(len(result['frames']), count)
                self.assertEqual(result['observer_session'], '2bd78136-4c17-4853-9d7c-7442274a7059')
                self.assertAlmostEqual(result['stages']['kernel_to_delivery']['upper']['p50_ms'], 1)
                self.assertAlmostEqual(result['stages']['worker_render_cpu']['upper']['p50_ms'], 3)
                self.assertEqual(result['latency']['p50_ms'], 10)
                self.assertFalse(result['performance_certified'])

    def test_unchanged_render_missing_gpu_or_foreign_worker_never_qualify(self):
        from physical_notes_stages import require_gpu_stages
        for hover in (False, True):
            records = self.fixture(hover)
            for prefix, row in records:
                if prefix == 'tilo-physical-input ':
                    row['before'] = copy.deepcopy(row['after'])
            with self.subTest(hover=hover), self.assertRaises(ValueError):
                self.report(records, hover)
            records = self.fixture(hover)
            next(row for prefix, row in records if prefix == 'tilo-native-worker ')['observer_session'] = 'other'
            with self.assertRaises(ValueError):
                self.report(records, hover)
            with self.assertRaisesRegex(ValueError, 'coverage'):
                require_gpu_stages(self.report(self.fixture(hover), hover))

    def test_settings_cli_uses_strict_native_pipeline_without_notes_metadata(self):
        import subprocess
        import sys
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as directory:
            capture = Path(directory)
            capture.joinpath('timeline.log').write_text('\n'.join(
                prefix + json.dumps(row) for prefix, row in self.fixture(True)))
            output = capture / 'report.json'
            script = Path(__file__).with_name('physical_notes_stages.py')
            args = [sys.executable, str(script), '--capture', str(capture), '--out', str(output),
                    '--settings-hover', 'bluetooth']
            result = subprocess.run(args, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(json.loads(output.read_text())['frames']), 1)
            required = args.copy()
            required[required.index('--out') + 1] = str(capture / 'gpu-report.json')
            result = subprocess.run(required + ['--require-gpu-stages'],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Incomplete exact native/compositor GPU or capture coverage', result.stderr)
            self.assertFalse((capture / 'gpu-report.json').exists())

    def test_pointer_capture_joins_wire_identity_not_nonexistent_key_serial(self):
        fixture = ForeignCaptureJoin()
        event = dict(kind='pointer_motion', pointer_id=17, pointer_enter_serial=29,
                     pointer_motion_ordinal=1, pointer_time_ms=31, wayland_serial=None)
        result = fixture.report(fixture.fixture(),
                                deliveries={(17, 29, 1, 31): dict(surface_id=42)}, events={1: event})
        self.assertTrue(result['complete'])
        self.assertEqual(result['frames'][0]['capture_seq'], 3)
        event['pointer_motion_ordinal'] = 2
        with self.assertRaises(KeyError):
            fixture.report(fixture.fixture(), deliveries={(17, 29, 1, 31): dict(surface_id=42)},
                           events={1: event})

    def test_required_gpu_path_accepts_only_same_frame_capture_and_recording(self):
        from physical_notes_stages import summarize_physical_settings_stages, require_gpu_stages
        from compositor_submission_test import gpu_record
        records = self.fixture(True)
        next(row for prefix,row in records if prefix == 'tilo-display-frame ')['sequence'] = 1
        records += [NativeGpuJoin().gpu(1, kind, event)
                    for kind in ('passes', 'dispatches') for event in ('attempt', 'completed')]
        phase = CompositorPhaseJoin().line(physical_presented_time_ns=110000000,
                                           physical_sequence=1, revision=9)
        capture = ForeignCaptureJoin().fixture()
        for prefix, row in capture:
            for key in list(row):
                if key.endswith('_ns') and type(row[key]) is int:
                    row[key] += 108190000
            if prefix == 'tilo-compositor-feedback ':
                row.update(physical_sequence=1)
                if row['event'] == 'completed':
                    row.update(physical_presented_time_ns=110000000,event_time_ns=110000001)
        lines = [prefix + json.dumps(row) for prefix, row in records]
        compositor = [phase, gpu_record()] + [prefix + json.dumps(row) for prefix, row in capture]
        report = summarize_physical_settings_stages(lines, 'bluetooth', compositor, True, hover=True)
        require_gpu_stages(report)
        self.assertTrue(report['foreign_capture']['complete'])
        self.assertTrue(report['native_gpu']['complete'])
        self.assertEqual(report['compositor']['frames'][0]['submission_gpu']['renderer_generation'], 7)
        self.assertEqual(report['compositor']['gpu_costs']['recording_ns']['n'], 1)
        self.assertEqual(report['compositor']['gpu_costs']['recording_ns']['p99_ms'], 30 * 1e-6)
        for gpu in (None, gpu_record(status='incomplete', recording_count=2, last_recording=4),
                    gpu_record(serial=42)):
            altered = [phase] + ([gpu] if gpu is not None else []) + compositor[2:]
            incomplete = summarize_physical_settings_stages(lines, 'bluetooth', altered, True, hover=True)
            self.assertFalse(incomplete['compositor']['gpu_complete'])
            self.assertEqual(incomplete['compositor']['gpu_costs'], {})
            with self.assertRaisesRegex(ValueError, 'Incomplete exact'):
                require_gpu_stages(incomplete)
        with self.assertRaises(ValueError):
            summarize_physical_settings_stages(lines, 'bluetooth',
                [phase.replace('physical_sequence=1', 'physical_sequence=2')] + compositor[1:], True, hover=True)

    def test_actual_cli_refuses_missing_samples_synthetic_origins_and_missing_frames(self):
        import subprocess
        import sys
        import tempfile
        from pathlib import Path
        script = Path(__file__).with_name('physical_notes_stages.py')
        with tempfile.TemporaryDirectory() as directory:
            capture = Path(directory)
            for case in ('empty', 'synthetic', 'missing_frame'):
                records = self.fixture(True)
                if case == 'empty':
                    records = []
                elif case == 'synthetic':
                    next(row for prefix, row in records if prefix == 'tilo-physical-pointer-delivery ')['input_time_ns'] = None
                else:
                    records = [(prefix,row) for prefix,row in records if prefix != 'tilo-physical-frame ']
                capture.joinpath('timeline.log').write_text('\n'.join(
                    prefix + json.dumps(row) for prefix,row in records))
                output = capture / (case + '.json')
                result = subprocess.run([sys.executable,str(script),'--capture',str(capture),
                    '--out',str(output),'--settings-hover','bluetooth'], capture_output=True,text=True)
                with self.subTest(case=case):
                    self.assertNotEqual(result.returncode,0)
                    self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()

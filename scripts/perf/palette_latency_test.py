import copy
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from compositor_submission_test import gpu_record, record
from palette_latency import summarize_palette_latency, json_rows

SESSION = '2bd78136-4c17-4853-9d7c-7442274a7059'
QUERY = 'a' * 64
REVISION = 'b' * 64
PROVIDER = 'settings'
ACTION = 'open-bluetooth'


def state(**changes):
    result = dict(visible=True, mounted=True, query_hash=QUERY, query_bytes=9,
                  results_generation=7, query_current=True, published_revision_hash=REVISION,
                  selected_entry_id='bluetooth', hovered_entry_id=None,
                  rows=[dict(entry_id='bluetooth', provider_id=PROVIDER, action_id=ACTION,
                             bounds=[10, 20, 100, 60], selected=True, hovered=False)])
    result.update(changes)
    return result


def fixture(mode='hover'):
    before = state()
    after = state(hovered_entry_id='bluetooth')
    after['rows'][0]['hovered'] = True
    if mode == 'open':
        before = state(visible=False, mounted=False, rows=[], selected_entry_id=None)
        after = state()
    elif mode == 'query':
        before = state(query_hash='c' * 64, query_bytes=8, results_generation=6)
        after = state(query_current=False, results_generation=6, published_revision_hash='d' * 64)
    producer = dict(schema=1, clock_id=1, process_id=42, observer_session=SESSION)
    event = dict(producer, id=1, kind='physical_pointer_motion' if mode == 'hover' else 'physical_key',
                 input_time_ns=1000000, dispatch_start_ns=2000000, dispatch_end_ns=3000000,
                 before=before, after=after)
    drawn = copy.deepcopy(after) if mode != 'query' else state()
    frame = dict(producer, pipe_serial=41, scene_revision=9, input_ids=[1],
                 draw_time_ns=4000000, submit_time_ns=5000000, state=drawn)
    phases = dict(serial=41, revision=9, event='render-retired', phase_timing='true',
                  queue_wait_us=10, render_us=20, total_us=30, encode_submit_us=10,
                  gpu_completion_wait_us=3000, bump_settle_us=1, backdrop_us=1,
                  kms_retirement_us=1, cpu_copy_us=0, readback_bytes=0, output_bytes=12,
                  physical_presented_time_ns=6000000, physical_sequence=5)
    return [('tilo-palette-input ', event), ('tilo-palette-frame ', frame),
            ('FRAME-LIFECYCLE ', phases), ('raw', record()),
            ('raw', gpu_record(recording_ns=2000000, post_render_ns=1000000))]


def lines(records):
    return [row if prefix == 'raw' else prefix +
            (' '.join(f'{k}={v}' for k, v in row.items()) if prefix == 'FRAME-LIFECYCLE ' else json.dumps(row))
            for prefix, row in records]


def report(records, mode='hover'):
    return summarize_palette_latency(lines(records), mode=mode, process_id=42,
        observer_session=SESSION, expected_query_hash=QUERY, target_provider=PROVIDER, target_action=ACTION)


class PaletteLatency(unittest.TestCase):
    def test_real_entrypoint_separates_physical_cpu_and_gpu_costs(self):
        result = report(fixture())
        self.assertEqual(result['latency']['p50_ms'], 5)
        self.assertEqual(result['latency']['p95_ms'], 5)
        self.assertEqual(result['latency']['p99_ms'], 5)
        self.assertEqual(result['stages']['dispatch_cpu_ns']['p50_ms'], 1)
        self.assertEqual(result['compositor_cpu']['gpu_completion_wait_us']['p50_ms'], 3)
        self.assertEqual(result['compositor_gpu']['recording_ns']['p50_ms'], 2)
        self.assertEqual(result['compositor_gpu']['post_render_ns']['p50_ms'], 1)
        self.assertFalse(result['performance_certified'])
        self.assertTrue(result['frames'][0]['compositor']['submission_gpu']['excludes_pre_execution_queue'])

    def test_multiple_samples_keep_counts_percentiles_and_distinct_gpu_ownership(self):
        records = fixture()
        for index in (2, 3):
            added = fixture()
            event, frame, phase = [added[n][1] for n in (0, 1, 2)]
            event['id'] = index
            for key in ('input_time_ns', 'dispatch_start_ns', 'dispatch_end_ns'):
                event[key] += index * 10000000
            frame.update(pipe_serial=40+index, input_ids=[index])
            for key in ('draw_time_ns', 'submit_time_ns'):
                frame[key] += index * 10000000
            phase.update(serial=40+index, physical_presented_time_ns=6000000+index*10000000+index*1000000,
                         physical_sequence=4+index)
            added[3] = ('raw', record(40+index))
            added[4] = ('raw', gpu_record(40+index, first_recording=2+index, last_recording=2+index))
            records.extend(added)
        result = report(records)
        self.assertEqual(result['samples'], 3)
        self.assertEqual(result['latency']['n'], 3)
        self.assertEqual(result['latency']['p50_ms'], 7)
        self.assertEqual(result['latency']['p95_ms'], 8)
        self.assertEqual(result['latency']['p99_ms'], 8)
        self.assertTrue(all(stage['n'] == 3 for stage in result['stages'].values()))
        records[5][1]['id'] = 4
        records[6][1]['input_ids'] = [4]
        with self.assertRaisesRegex(ValueError, 'Non-monotonic'):
            report(records)

    def test_open_and_query_have_distinct_actual_draw_proofs(self):
        for mode in ('open', 'query'):
            with self.subTest(mode=mode):
                result = report(fixture(mode), mode)
                self.assertTrue(result['available'])
                self.assertEqual(result['frames'][0]['results_generation'], 7)

    def test_missing_inputs_frame_kms_cpu_or_gpu_refuses(self):
        original = fixture()
        for index in range(len(original)):
            with self.subTest(index=index), self.assertRaises(ValueError):
                report(original[:index] + original[index+1:])

    def test_duplicate_record_families_refuse(self):
        for row in fixture():
            with self.subTest(prefix=row[0]), self.assertRaises(ValueError):
                report(fixture() + [row])

    def test_clock_lifetime_schema_and_input_kind_refuse(self):
        for changes in (dict(clock_id=0), dict(clock_id=True), dict(process_id=43),
                        dict(observer_session='d18a8ac3-c453-437f-b6d6-3fef3f242154'),
                        dict(schema=2), dict(kind='controlled_input'), dict(id=True),
                        dict(input_time_ns=4000000), dict(dispatch_end_ns=5000000)):
            records = fixture()
            records[0][1].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                report(records)

    def test_stale_coalesced_wrong_revision_and_ordered_frame_refuse(self):
        for changes in (dict(input_ids=[1, 2]), dict(input_ids=[]), dict(scene_revision=10),
                        dict(draw_time_ns=2000000), dict(submit_time_ns=7000000),
                        dict(clock_id=0), dict(process_id=43), dict(pipe_serial=42)):
            records = fixture()
            records[1][1].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                report(records)
        records = fixture()
        extra = copy.deepcopy(records[1][1])
        extra.update(pipe_serial=42, input_ids=[8])
        with self.assertRaises(ValueError):
            report(records + [('tilo-palette-frame ', extra)])

    def test_no_positive_kms_and_incomplete_cpu_phase_refuse(self):
        for changes in (dict(physical_presented_time_ns='none', physical_sequence='none'),
                        dict(physical_presented_time_ns=0), dict(physical_sequence='none'),
                        dict(phase_timing='false'), dict(total_us=1), dict(revision=10)):
            records = fixture()
            records[2][1].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                report(records)

    def test_gpu_is_required_actual_owned_recording_not_host_wait(self):
        for changes in (dict(status='incomplete'), dict(process_id=43), dict(revision=10),
                        dict(status='missing', renderer_generation='none', first_recording='none',
                             last_recording='none', recording_count=0, measured_count=0,
                             recording_ns='none', post_render_ns='none')):
            records = fixture()
            records[-1] = ('raw', gpu_record(**changes))
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                report(records)

    def test_stale_unmounted_unpublished_wrong_target_or_unchanged_hover_refuse(self):
        for changes in (dict(mounted=False), dict(visible=False), dict(query_current=False),
                        dict(query_hash='c'*64), dict(published_revision_hash=None),
                        dict(results_generation=6), dict(published_revision_hash='d'*64)):
            records = fixture()
            records[1][1]['state'].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                report(records)
        for part in ('before', 'after'):
            records = fixture()
            records[0][1][part] = state(hovered_entry_id='bluetooth' if part == 'before' else None)
            records[0][1][part]['rows'][0]['hovered'] = part == 'before'
            with self.subTest(part=part), self.assertRaises(ValueError):
                report(records)
        records = fixture()
        records[1][1]['state']['rows'][0]['provider_id'] = 'foreign'
        with self.assertRaises(ValueError):
            report(records)

    def test_query_no_change_or_wrong_adoption_refuses(self):
        for part in ('before', 'after'):
            records = fixture('query')
            records[0][1][part]['query_hash'] = QUERY if part == 'before' else 'c'*64
            with self.subTest(part=part), self.assertRaises(ValueError):
                report(records, 'query')
        records = fixture('open')
        records[0][1]['before']['visible'] = True
        with self.assertRaises(ValueError):
            report(records, 'open')

    def test_duplicate_json_fields_and_unbounded_rows_fail(self):
        with self.assertRaises(ValueError):
            json_rows(['tilo-palette-input {"id":1,"id":2}'], 'tilo-palette-input ')
        with self.assertRaises(ValueError):
            json_rows(['tilo-palette-input ' + 'x'*65537], 'tilo-palette-input ')

    def test_physical_click_without_original_broker_link_refuses(self):
        records = fixture()
        records[0][1].update(kind='physical_pointer_button', before=state(hovered_entry_id='bluetooth'))
        records[0][1]['before']['rows'][0]['hovered'] = True
        with self.assertRaisesRegex(ValueError, 'original physical button'):
            report(records, 'click')
        submitted = dict(invocation='d18a8ac3-c453-437f-b6d6-3fef3f242154', process_id=42,
                         palette_input_id=1, palette_observer_session=SESSION, input_time_ns=1000000,
                         activation_dispatch_ns=2000000, source='semantic')
        with self.assertRaisesRegex(ValueError, 'original physical button'):
            report(records + [('tilo-action-submission ', submitted)], 'click')

    def test_cli_exclusive_private_output_and_failure_does_not_write(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            log, out = directory/'capture.log', directory/'report.json'
            log.write_text('\n'.join(lines(fixture())))
            command = [sys.executable, str(Path(__file__).with_name('palette_latency.py')),
                       '--log', str(log), '--out', str(out), '--mode', 'hover', '--process-id', '42',
                       '--observer-session', SESSION, '--expected-query-hash', QUERY,
                       '--target-provider', PROVIDER, '--target-action', ACTION]
            first = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(os.stat(out).st_mode & 0o777, 0o600)
            original = out.read_bytes()
            second = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(second.returncode, 0)
            self.assertEqual(out.read_bytes(), original)
            out.unlink()
            log.write_text('no evidence')
            third = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(third.returncode, 0)
            self.assertFalse(out.exists())


if __name__ == '__main__':
    unittest.main()

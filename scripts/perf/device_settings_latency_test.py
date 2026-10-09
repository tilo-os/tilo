"""Production Settings collector and public mounted-boundary guard tests."""
import copy
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import device_settings_latency as capture
from device_notes_trace import collect_settings_hover_trace
from device_notes_trace_test import LocalSlot
from physical_notes_stages import summarize_physical_settings_stages, require_gpu_stages
from physical_notes_stages_test import (PhysicalSettingsStages, NativeGpuJoin,
                                       CompositorPhaseJoin, ForeignCaptureJoin)
from compositor_submission_test import gpu_record
from fresh_notes_capture_test import state, cohort


def timeline():
    records = PhysicalSettingsStages().fixture(True)
    next(row for prefix, row in records if prefix == 'tilo-display-frame ')['sequence'] = 1
    records += [NativeGpuJoin().gpu(1, kind, event)
                for kind in ('passes', 'dispatches') for event in ('attempt', 'completed')]
    foreign = ForeignCaptureJoin().fixture()
    for prefix, row in foreign:
        for key in list(row):
            if key.endswith('_ns') and type(row[key]) is int:
                row[key] += 108190000
        if prefix == 'tilo-compositor-feedback ':
            row['physical_sequence'] = 1
            if row['event'] == 'completed':
                row.update(physical_presented_time_ns=110000000, event_time_ns=110000001)
    return ([prefix + json.dumps(row) for prefix, row in records + foreign]
            + [CompositorPhaseJoin().line(physical_presented_time_ns=110000000,
                                           physical_sequence=1, revision=9), gpu_record()])


class SettingsDeviceCapture(unittest.TestCase):
    def test_frontend_runs_public_hover_and_real_analyzer_with_restored_diagnostics(self):
        before = state([(1, 21, 31)])
        current = state([(1, 21, 31), (2, 99, 32)])
        current['workspace_tree']['local_app_records']['2']['app_id'] = capture.APP
        current['workspace_tree']['workspaces'][0]['strips'][0]['window_groups'][0]['tabs'][-1]['tile']['Foreign']['app_id'] = capture.APP
        worker = current['chrome']['native_window_provenance']['windows'][-1]
        worker['package_id'] = capture.APP
        worker['certified_checkout'] = dict(application=capture.APP, room='room', record='saved:settings:99')
        shown = dict(slot=dict(components={'apps+runtime': 'layer'}), files=[dict(
            component='apps+runtime', layer='layer', path='/usr/share/tilo-apps/sdk/apps/settings/bin/settings', sha256='b'*64)])
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            flight = output/'flight.log'
            class Vm:
                def __init__(self):
                    self.opened = False
                    self.moves = []
                    self.original = dict.fromkeys(capture.FLAGS, False)
                    self.configured = []
                def state(self): return current if self.opened else before
                def actions(self, query):
                    return [dict(provider='actions' if query == 'tab.open-beside' else 'desktop-windows', id=query)]
                def control_json(self, *args):
                    if args[3] == 'configure@session-diagnostics':
                        value = json.loads(args[-1]); self.configured.append(value)
                        return dict(diagnostics=value)
                    return dict(diagnostics=self.original)
                def root_exec(self, command):
                    return json.dumps(dict(pid=102, start_time_ticks='123', nspid=[102,123], flags=capture.FLAGS))
                def targets(self): return [dict(name='foreign-surface-32', rect=[500,40,800,600])]
                def input(self, *args):
                    self.moves.append(args)
                    lines = timeline()
                    if len(self.moves) == 1:
                        row = json.loads(next(line for line in lines if line.startswith('tilo-physical-input ')).split(' ',1)[1])
                        row['id'] = 0
                        lines = ['tilo-physical-input '+json.dumps(row)]
                    flight.write_text('\n'.join(lines))
                def run(self, *args):
                    if args[:3] == ('--json','slot','show'): return json.dumps(shown)
                    command = args[args.index('--')+1:]
                    if command[2] == capture.IDENTITY or command[2] == capture.collect_settings_hover_trace.__globals__['READ_TRACE_SCRIPT']:
                        return subprocess.run(command, check=True, text=True, capture_output=True).stdout
                    return str(flight)
            vm = Vm()
            class Cohort:
                mapped_record = staticmethod(cohort.mapped_record)
                def mapped(self, *args, **kwargs):
                    self.selected = kwargs['selected']
                def catalog(self, *args): return []
                def choose(self, *args, **kwargs): return dict(id='inspect', provider='app-actions')
            matched = Cohort()
            def invoke(vm, row, args, out, name, **kwargs):
                if name == 'fresh-open': vm.opened = True
                if not name.startswith('inspect-'): return None
                hover = dict(control='settings.nav', mounted=True, pane='mouse_keyboard')
                navigation = dict(selected_pane='network', requested_pane='network', path='settings://network',
                                  ready=True,pending=False,content_visible=True)
                return dict(app=capture.APP, **navigation, input=dict(
                    root_rect=[0,0,800,600], navigation=navigation, hover=hover, rendered_hover=hover,
                    rows=[dict(pane='mouse_keyboard',rect=[10,100,100,140]),dict(pane='bluetooth',rect=[10,150,100,190])]))
            options = argparse.Namespace(out=output, uid=1000, samples=1, pane='bluetooth', reset_pane='mouse_keyboard')
            with patch.object(capture, 'invoke', invoke):
                report = capture.capture(vm, matched, options, shown, {'/lib/libruntime_dylib.so':'a'*64})
            self.assertEqual(report['latency']['n'], 1)
            self.assertEqual(report['latency']['p95_ms'], 10)
            self.assertEqual(vm.moves, [('move','555','160'),('move','555','210')])
            self.assertEqual(vm.configured[-1], vm.original)
            self.assertEqual(matched.selected['peer_pid'], 102)
            self.assertTrue((output/'samples.json').is_file())

    def test_actual_reader_preserves_gpu_chain_and_exact_settings_report(self):
        lines = timeline()
        slot = LocalSlot()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'flight.log'
            path.write_text('\n'.join(lines + [gpu_record(900, first_recording=4, last_recording=4)
                                               + ' private=do-not-export']))
            captured = collect_settings_hover_trace(slot, str(path), {},
                         '2bd78136-4c17-4853-9d7c-7442274a7059', 0)
        gpu = [line for line in captured if line.startswith('FRAME-GPU-SUBMISSION ')]
        self.assertEqual(len(gpu), 1)
        self.assertNotIn('private', '\n'.join(captured))
        report = summarize_physical_settings_stages(captured, 'bluetooth', require_capture=True, hover=True)
        require_gpu_stages(report)
        self.assertTrue(report['main_stages_available'])
        self.assertEqual(report['latency']['n'], 1)
        self.assertEqual(report['compositor']['frames'][0]['submission_gpu']['recording_ns'], 30)

    def test_collected_missing_partial_duplicate_or_wrong_revision_gpu_cannot_qualify(self):
        for replacement in (None, gpu_record(status='incomplete', measured_count=0,
                            recording_ns='none', post_render_ns='none'),
                            gpu_record(revision=10), gpu_record() + '\n' + gpu_record()):
            with self.subTest(replacement=replacement), tempfile.TemporaryDirectory() as directory:
                lines = [line for line in timeline() if not line.startswith('FRAME-GPU-SUBMISSION ')]
                if replacement is not None:
                    lines.append(replacement)
                path = Path(directory) / 'flight.log'
                path.write_text('\n'.join(lines))
                with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                    captured = collect_settings_hover_trace(LocalSlot(), str(path), {},
                              '2bd78136-4c17-4853-9d7c-7442274a7059', 0)
                    require_gpu_stages(summarize_physical_settings_stages(
                        captured, 'bluetooth', require_capture=True, hover=True))

    def test_mounted_local_bounds_use_only_exact_surface_transform(self):
        inspection = dict(input=dict(root_rect=[0, 0, 800, 600], rows=[
            dict(pane='bluetooth', rect=[10, 100, 100, 140])]))
        targets = [dict(name='foreign-surface-42', rect=[500, 40, 800, 600])]
        self.assertEqual(capture.hover_point(inspection, targets, 42, 'bluetooth'), (555, 160))
        for value in ([], [dict(name='foreign-surface-43', rect=[500, 40, 800, 600])],
                      targets + targets, [dict(name='foreign-surface-42', rect=[500, 40, 400, 600])]):
            with self.subTest(targets=value), self.assertRaises(ValueError):
                capture.hover_point(inspection, value, 42, 'bluetooth')
        for bounds in ([10, 100, 10, 140], [0, float('nan'), 20, 40], [900, 100, 1000, 140]):
            changed = copy.deepcopy(inspection)
            changed['input']['rows'][0]['rect'] = bounds
            with self.subTest(bounds=bounds), self.assertRaises(ValueError):
                capture.hover_point(changed, targets, 42, 'bluetooth')

    def test_settings_binding_preserves_notes_and_refuses_borrowed_record(self):
        before = state([(1, 21, 31)])
        current = state([(1, 21, 31), (2, 99, 32)])
        tab = current['workspace_tree']['workspaces'][0]['strips'][0]['window_groups'][0]['tabs'][-1]
        tab['tile']['Foreign']['app_id'] = capture.APP
        current['workspace_tree']['local_app_records']['2']['app_id'] = capture.APP
        worker = current['chrome']['native_window_provenance']['windows'][-1]
        worker['package_id'] = capture.APP
        worker['certified_checkout'] = dict(application=capture.APP, room='room', record='saved:settings:99')
        binding = capture.fresh_binding(current, capture.tabs(before), cohort, capture.APP)
        self.assertEqual(binding['record'], 'saved:settings:99')
        changed = copy.deepcopy(current)
        changed['workspace_tree']['workspaces'][0]['strips'][0]['window_groups'][0]['tabs'][0]['tile']['Foreign']['surface_id'] = 333
        with self.assertRaisesRegex(RuntimeError, 'Pre-existing'):
            capture.fresh_binding(changed, capture.tabs(before), cohort, capture.APP)

    def test_observer_probe_matches_worker_without_exporting_user_fields(self):
        from physical_notes_latency_test import PhysicalSettingsHoverLatency
        lines = PhysicalSettingsHoverLatency().timeline()
        event = next(row for prefix, row in lines if prefix == 'tilo-physical-input ')
        event['private'] = 'never-export'
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'flight.log'
            path.write_text('\n'.join(prefix + json.dumps(row) for prefix, row in lines))
            result = subprocess.run(['python3', '-c', capture.IDENTITY, str(path), str(event['process_id'])],
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), dict(observer_session=event['observer_session'], after_input_id=1))
            self.assertNotIn('never-export', result.stdout)
            wrong = subprocess.run(['python3', '-c', capture.IDENTITY, str(path), '999'], capture_output=True)
            self.assertNotEqual(wrong.returncode, 0)


if __name__ == '__main__':
    unittest.main()

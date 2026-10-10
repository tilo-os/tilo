import json
import importlib.util
import tempfile
import signal
import subprocess
import unittest
from unittest import mock
from pathlib import Path
from input_latency import summarize_input_latency


class CompositorTraceCompletenessTests(unittest.TestCase):
    def runner(self):
        path = Path(__file__).with_name("frame-loop-bench.py")
        spec = importlib.util.spec_from_file_location("frame_loop_bench", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def parse(self, contents):
        module = self.runner()
        with tempfile.TemporaryDirectory() as directory:
            trace = Path(directory) / "frames.csv"
            trace.write_text(contents)
            return module.read_trace(trace)

    def test_valid_optional_latency_is_preserved(self):
        required = {
            "period_us": 16, "input_us": 0, "pump_us": 0, "layout_us": 0,
            "scene_us": 0, "submit_us": 0, "clients_us": 0, "sleep_us": 0,
            "painted": 1, "present_async": 0,
        }
        header = ["start_us", *required, "input_latency_us"]
        first = {"start_us": 1, **required, "input_latency_us": None}
        second = {"start_us": 2, **required, "input_latency_us": 14}
        contents = "\n".join([
            ",".join(header),
            ",".join("" if first[key] is None else str(first[key]) for key in header),
            ",".join(str(second[key]) for key in header),
            "",
        ])
        parsed_header, rows = self.parse(contents)
        self.assertEqual(parsed_header, header)
        self.assertEqual(rows, [first, second])

    def test_truncated_or_extra_rows_cannot_disappear_from_tail_statistics(self):
        for contents in [
            "start_us,period_us\n1,16\n2\n3,16\n",
            "start_us,period_us\n1,16\n2,80,1\n3,16\n",
            "start_us,period_us\n1,16\n2,not-a-time\n",
        ]:
            with self.subTest(contents=contents), self.assertRaises(ValueError):
                self.parse(contents)

    def test_missing_samples_or_ambiguous_headers_fail(self):
        for contents in ["", "start_us\n", "start_us,start_us\n1,2\n", ",period_us\n1,2\n"]:
            with self.subTest(contents=contents), self.assertRaises(ValueError):
                self.parse(contents)

    def test_requested_load_cannot_be_replaced_by_an_idle_run(self):
        runner = self.runner()
        with tempfile.TemporaryDirectory() as directory:
            shell = Path(directory) / "shell"
            self.assertIsNone(runner.required_load_client(shell, "none"))
            for load in ["fullscreen", "windowed"]:
                with self.subTest(load=load), self.assertRaises(FileNotFoundError):
                    runner.required_load_client(shell, load)
            client = shell.with_name("tilo-loadclient")
            client.mkdir()
            with self.assertRaises(FileNotFoundError):
                runner.required_load_client(shell, "fullscreen")
            client.rmdir()
            client.write_text("fixture")
            self.assertEqual(runner.required_load_client(shell, "fullscreen"), client)

    def test_early_failure_terminates_and_reaps_only_its_session_group(self):
        runner = self.runner()
        proc = mock.Mock(pid=42)
        proc.poll.return_value = None
        proc.wait.side_effect = [subprocess.TimeoutExpired("fixture", 5), 0]
        with mock.patch.object(runner.os, "killpg") as kill:
            runner.stop_process_group(proc)
        self.assertEqual(kill.call_args_list, [
            mock.call(42, signal.SIGTERM), mock.call(42, signal.SIGKILL),
        ])
        self.assertEqual(proc.wait.call_args_list, [mock.call(timeout=5), mock.call(timeout=5)])

    def test_completed_session_is_not_signalled(self):
        runner = self.runner()
        proc = mock.Mock()
        proc.poll.return_value = 0
        with mock.patch.object(runner.os, "killpg") as kill:
            runner.stop_process_group(proc)
        kill.assert_not_called()


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
            lines.append('tilo-native-input '+json.dumps(row))
        frames = [{'frame': 19, 'total_us': 100}, {'frame': 20, 'total_us': 100}]
        for cpu, ids, displayed in [(19, [0], 100.004), (20, [1, 2], 100.020)]:
            lines.append('tilo-native-frame '+json.dumps(dict(cpu_frame_id=cpu, input_ids=ids)))
            lines.append('tilo-render-frame '+json.dumps(dict(cpu_frame_id=cpu, outcome='Presented')))
            identity = dict(worker_id=1, worker_frame_ix=cpu, cpu_frame_id=cpu)
            lines.append('tilo-display-request '+json.dumps(identity))
            lines.append('tilo-display-frame '+json.dumps(dict(identity,
                presented_time_s=displayed, backend='metal', drawable_id=cpu)))
        return scenario, lines, frames

    def change(self, lines, prefix, index, **values):
        lines = list(lines)
        locations = [i for i,s in enumerate(lines) if s.startswith(prefix+' ')]
        at = locations[index]
        row = json.loads(lines[at].split(' ',1)[1]); row.update(values)
        lines[at] = prefix+' '+json.dumps(row)
        return lines

    def test_reports_measured_inputs_or_explicitly_unavailable_or_unresolved_latency(self):
        with self.subTest(case='keeps each coalesced input and final dispatch after sender finished'):
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
        with self.subTest(case='mixed warmup frame retains only measured inputs'):
            scenario, lines, frames = self.fixture()
            lines = self.change(lines, 'tilo-native-frame', 0, input_ids=[0,1])
            lines = self.change(lines, 'tilo-native-frame', 1, input_ids=[2])
            lines = self.change(lines, 'tilo-display-frame', 0, presented_time_s=100.010)
            report = summarize_input_latency(scenario, lines, frames)
            self.assertIsInstance(report, dict)
            self.assertEqual([r['input_id'] for r in report['inputs']], [1,2])
        with self.subTest(case='missing and partial clocks are distinguished'):
            scenario, lines, frames = self.fixture()
            old=[]
            for line in lines:
                if line.startswith('tilo-native-input '):
                    row=json.loads(line.split(' ',1)[1])
                    row={k:v for k,v in row.items() if 'host_' not in k}
                    line='tilo-native-input '+json.dumps(row)
                old.append(line)
            report=summarize_input_latency(scenario,old,frames)
            self.assertIsInstance(report, dict)
            self.assertFalse(report['available'])
            with self.assertRaises(ValueError):
                summarize_input_latency(scenario, [old[0],*lines[1:]], frames)
        with self.subTest(case='definite negative rejected and precision overlap unresolved'):
            scenario, lines, frames = self.fixture()
            with self.assertRaises(ValueError):
                summarize_input_latency(scenario,self.change(lines,'tilo-display-frame',1,presented_time_s=100.0045),frames)
            lines=self.change(lines,'tilo-display-frame',1,presented_time_s=100.005001)
            report=summarize_input_latency(scenario,lines,frames)
            self.assertIsInstance(report, dict)
            self.assertFalse(report['latency_complete'])
            self.assertEqual(report['unresolved_input_ids'],[2])
            self.assertIsNone(report['dispatch_to_present_upper'])
        with self.subTest(case='large host epoch keeps numeric uncertainty'):
            scenario,lines,frames=self.fixture()
            epoch=2**30
            for i in range(3):
                lines=self.change(lines,'tilo-native-input',i,
                    sent_host_lo_s=epoch+i*.001,sent_host_hi_s=epoch+i*.001,
                    dispatch_host_lo_s=epoch+.003+i*.001,dispatch_host_hi_s=epoch+.003+i*.001)
            for i,t in enumerate([epoch+.004,epoch+.020]):
                lines=self.change(lines,'tilo-display-frame',i,presented_time_s=t)
            report=summarize_input_latency(scenario,lines,frames)
            self.assertIsInstance(report,dict)
            bounds=report['inputs'][0]['sent_to_present_ms']
            self.assertLess(bounds[0],19)
            self.assertGreater(bounds[1],19)
        with self.subTest(case='unsupported platform is explicitly unavailable'):
            scenario,lines,frames=self.fixture()
            for i in range(3):
                lines=self.change(lines,'tilo-native-input',i,host_clock='unavailable',
                    sent_host_lo_s=None,sent_host_hi_s=None,
                    dispatch_host_lo_s=None,dispatch_host_hi_s=None)
            report=summarize_input_latency(scenario,lines,frames)
            self.assertFalse(report['available'])
            self.assertFalse(report['latency_complete'])

    def test_evidence_that_could_invent_latency_is_rejected(self):
        scenario, lines, frames = self.fixture()
        for changes in [dict(sent_host_lo_s=float('nan')),dict(sent_host_hi_s=float('inf')),
                        dict(sent_host_lo_s=True),dict(sent_host_lo_s=0),
                        dict(sent_host_hi_s=99),dict(dispatch_host_lo_s=99),
                        dict(host_clock='other'),dict(dispatch_host_hi_s=100.002)]:
            with self.subTest(case='invalid bracket or causal order', changes=changes), self.assertRaises(ValueError):
                summarize_input_latency(scenario,self.change(lines,'tilo-native-input',1,**changes),frames)
        noncontiguous=self.change(lines,'tilo-native-frame',0,input_ids=[0,2])
        noncontiguous=self.change(noncontiguous,'tilo-native-frame',1,input_ids=[1])
        noncontiguous=self.change(noncontiguous,'tilo-display-frame',0,presented_time_s=100.010)
        indices=[i for i,s in enumerate(lines) if s.startswith('tilo-native-frame ')]
        reordered=list(lines)
        reordered[indices[0]],reordered[indices[1]]=reordered[indices[1]],reordered[indices[0]]
        variants=[('missing feedback',lines[:-1]), ('duplicate feedback',lines+[lines[-1]]),
                  ('zero presentation',self.change(lines,'tilo-display-frame',1,presented_time_s=0)),
                  ('foreign feedback',self.change(lines,'tilo-display-frame',1,cpu_frame_id=999)),
                  ('duplicate input id',self.change(lines,'tilo-native-frame',1,input_ids=[1,1])),
                  ('foreign input id',self.change(lines,'tilo-native-frame',1,input_ids=[1,99])),
                  ('noncontiguous frame mapping cannot invent latency',noncontiguous),
                  ('numeric overflow cannot be complete latency',
                   self.change(lines,'tilo-display-frame',1,presented_time_s=1e308)),
                  ('clock lifecycle event',lines+['tilo-native-clock-event {"event":"suspend"}']),
                  ('frame reordering',reordered)]
        for label,variant in variants:
            with self.subTest(case=label), self.assertRaises(ValueError):
                summarize_input_latency(scenario,variant,frames)

if __name__=='__main__':unittest.main()

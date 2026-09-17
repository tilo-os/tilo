import argparse
import importlib.util
from pathlib import Path
import unittest
import tempfile
import signal
import subprocess
from unittest.mock import Mock, patch

SPEC = importlib.util.spec_from_file_location('producer',
    Path(__file__).resolve().parents[1] / 'dev/produce-wpe-stage.py')
producer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(producer)


class ProducerTests(unittest.TestCase):
    def setUp(self):
        self.args = argparse.Namespace(architecture='x86_64', revision='a' * 40,
            branch='refs/heads/codex/wpe', run_id='42', attempt='1',
            wpe='code.podesta.ai/qos/tilo-wpe-buildroot@sha256:' + '1' * 64,
            tools_builder='code.podesta.ai/qos/tilo-builder@sha256:' + '2' * 64)

    def test_supported_native_architectures(self):
        for architecture in ('x86_64', 'aarch64'):
            self.args.architecture = architecture
            producer.validate(self.args)

    def test_rejects_wrong_scope_or_mutable_inputs(self):
        for field, value in [('architecture', 'amd64'), ('revision', 'main'),
                             ('branch', 'refs/heads/main'), ('run_id', '0'), ('attempt', '-1'),
                             ('wpe', self.args.wpe.split('@')[0] + ':latest'),
                             ('tools_builder', 'foreign.example/builder@sha256:' + '2' * 64)]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                producer.validate(argparse.Namespace(**(vars(self.args) | {field: value})))

    def test_measured_capacity_boundaries(self):
        self.assertTrue(producer.capacity_ok(producer.START_FREE, starting=True))
        self.assertFalse(producer.capacity_ok(producer.START_FREE - 1, starting=True))
        self.assertTrue(producer.capacity_ok(producer.ABORT_FREE))
        self.assertFalse(producer.capacity_ok(producer.ABORT_FREE - 1))

    def test_capacity_abort_stops_child_and_records_measurement(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            child = Mock()
            child.poll.return_value = None
            measurements = []
            with patch.object(producer, 'EVIDENCE', root), \
                 patch.object(producer, 'capacity', return_value={'/work': 1}), \
                 patch.object(producer, 'memory', return_value={'memory.max': '23Gi'}), \
                 patch.object(producer.subprocess, 'Popen', return_value=child), \
                 patch.object(producer, 'stop') as stop:
                with self.assertRaises(ValueError):
                    producer.phase('fixture', ['never-executed'], {}, root, float('inf'), measurements)
                stop.assert_called_once_with(child)
            self.assertEqual(measurements[0]['status'], 'disk-reserve-abort')
            self.assertEqual(measurements[0]['minimum_free_bytes'], 1)

    def test_compiler_evidence_excludes_registry_errors_and_is_bounded(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'private.log'
            path.write_text('error: registry authentication PRIVATE_AUTH\n'
                + 'Source/WebCore/file.cpp:123:4: error: missing identifier\n' * 100
                + 'TILO_BUILD_TIMING phase=wpe elapsed_seconds=42 exit_code=1\n')
            evidence = producer.compiler_diagnostics(path)
            self.assertEqual(len(evidence['diagnostic_tail']), 80)
            self.assertNotIn('PRIVATE_AUTH', str(evidence))
            self.assertEqual(evidence['compiler_timings'][0]['elapsed_seconds'], 42)

    def test_deadline_abort_and_nonzero_exit_preserve_failure_without_stage_receipt(self):
        for running in (True, False):
            with self.subTest(running=running), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                child = Mock(returncode=9)
                child.poll.return_value = None if running else 9
                records = []
                with patch.object(producer, 'EVIDENCE', root), \
                     patch.object(producer, 'capacity', return_value={'/work': producer.START_FREE}), \
                     patch.object(producer, 'memory', return_value={}), \
                     patch.object(producer.subprocess, 'Popen', return_value=child), \
                     patch.object(producer, 'stop') as stop:
                    with self.assertRaises(ValueError):
                        producer.phase('fixture', ['not-executed'], {}, root, 0, records)
                    stop.assert_called_once_with(child)
                self.assertEqual(records[0]['status'], 'bounded-duration-abort' if running else 'command-failed')
                self.assertTrue((root / 'phases.json').is_file())
                self.assertFalse((root / 'wpe-stage.json').exists())

    def test_stop_terminates_entire_process_group_and_escalates(self):
        child = Mock(pid=123)
        child.poll.return_value = None
        child.wait.side_effect = [subprocess.TimeoutExpired('fixture', 10), 0]
        with patch.object(producer.os, 'killpg') as kill:
            producer.stop(child)
        self.assertEqual([call.args for call in kill.call_args_list],
                         [(123, signal.SIGTERM), (123, signal.SIGKILL)])


if __name__ == '__main__':
    unittest.main()

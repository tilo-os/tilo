"""Exercise the real feedback subprocess path without compiling on the test host."""
import argparse
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import rust_feedback as feedback


ARTIFACT = {"reason": "compiler-artifact", "package_id": "leaf", "fresh": False,
            "target": {"name": "leaf", "kind": ["lib"]}, "profile": {"test": True}}


class FeedbackTests(unittest.TestCase):
    def test_observations_distinguish_compilation_from_test_execution(self):
        observed = feedback.Observation()
        observed.observe(json.dumps(ARTIFACT), 2.0)
        observed.observe(json.dumps({**ARTIFACT, "fresh": True}), 2.1)
        observed.observe(json.dumps({"reason": "compiler-message", "message": {"level": "warning"}}), 3.0)
        observed.observe(json.dumps({"reason": "compiler-message", "message": {"level": "error"}}), 4.0)
        observed.observe('remote-driver: synced sources in 0.3s\n', 4.1)
        observed.observe('test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out\n', 5.0)
        self.assertIsNone(observed.first_test_result_seconds)
        observed.observe('test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 6 filtered out\n', 6.0)
        row = observed.receipt()
        self.assertEqual(row['first_diagnostic_seconds'], 3.0)
        self.assertEqual(row['first_error_seconds'], 4.0)
        self.assertEqual(row['first_test_result_seconds'], 6.0)
        self.assertEqual(row['fresh_units'], 1)
        self.assertEqual(row['rebuilt_units'], 0)
        self.assertEqual(row['tests'], {'passed': 2, 'failed': 0, 'ignored': 1, 'filtered_out': 15})
        self.assertEqual(row['driver_reported_seconds'], {'source_sync': 0.3})
        self.assertIn('link_seconds', row['unknown'])
        observed.observe('Summary [  0.03s] 4 tests run: 4 passed, 0 skipped', 7.0)
        self.assertEqual(observed.tests['passed'], 6)
        observed.observe('remote-driver: got a build slot in 1m10s', 72.0)
        self.assertEqual(observed.receipt()['driver_reported_seconds']['builder_admission_queue'], 70.0)
        self.assertNotIn('builder_admission_queue_seconds', observed.receipt()['unknown'])

    def test_real_child_failure_empty_filter_and_missing_protocol_fail_closed(self):
        for case, events, summary, code, expected in (
            ('pass', [ARTIFACT, {'reason': 'build-finished', 'success': True}],
             'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out', 0, 0),
            ('empty', [ARTIFACT, {'reason': 'build-finished', 'success': True}],
             'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out', 0, 1),
            ('missing', [], '', 0, 1),
            ('failed', [ARTIFACT, {'reason': 'build-finished', 'success': True}],
             'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out', 101, 101),
        ):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                out = root / 'receipt.json'
                args = argparse.Namespace(operation='test', package=['leaf'], lib=True, tests=False,
                                          test=None, filter=None, exact=False, out=out)
                output = '\n'.join([json.dumps(event) for event in events] + [summary])
                argv = [sys.executable, '-c', f'print({output!r}); raise SystemExit({code})']
                with patch.object(feedback, 'command', return_value=argv), \
                     patch.object(feedback, 'identity', return_value={'head': 'fixture'}), \
                     contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(feedback.run(args, root), expected)
                    receipt = json.loads(out.read_text())
                    self.assertEqual(receipt['status'], 'success' if expected == 0 else 'failure')
                    self.assertFalse(receipt['integration_qualification'])
                    self.assertEqual(receipt['exit_code'], code)
                    with self.assertRaises(FileExistsError):
                        feedback.run(args, root)
                    self.assertEqual(json.loads(out.read_text()), receipt)

    def test_spawn_failure_and_source_changes_keep_receipts_red(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            args = argparse.Namespace(operation='check', package=['leaf'], lib=True, tests=False,
                                      test=None, filter=None, exact=False, out=root / 'failed.json')
            with patch.object(feedback, 'command', return_value=[str(root / 'missing')]), \
                 patch.object(feedback, 'identity', return_value={'head': 'fixture'}), \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(feedback.run(args, root), 2)
            self.assertEqual(json.loads(args.out.read_text())['status'], 'failure')
            args.out = root / 'changed.json'
            output = '\n'.join(json.dumps(event) for event in
                               (ARTIFACT, {'reason': 'build-finished', 'success': True}))
            argv = [sys.executable, '-c', f'print({output!r})']
            with patch.object(feedback, 'command', return_value=argv), \
                 patch.object(feedback, 'identity', side_effect=[{'head': 'before'}, {'head': 'after'}]), \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(feedback.run(args, root), 1)
            self.assertTrue(json.loads(args.out.read_text())['source_changed_during_run'])

    def test_duration_and_requested_package_are_observed(self):
        self.assertEqual(feedback.duration_seconds('1m 22s'), 82)
        self.assertEqual(feedback.duration_seconds('1h2m3.5s'), 3723.5)
        self.assertIsNone(feedback.duration_seconds('unobserved'))
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / 'Cargo.toml'
            manifest.write_text('[package]\nname="leaf"\n')
            observed = feedback.Observation(['leaf'])
            observed.observe(json.dumps(ARTIFACT), 1.0)
            self.assertIsNone(observed.first_requested_artifact_seconds)
            observed.observe(json.dumps({**ARTIFACT, 'manifest_path': str(manifest)}), 2.0)
            self.assertEqual(observed.first_requested_artifact_seconds, 2.0)
            observed.observe('Finished `dev` profile target(s) in 1m 22s', 82.0)
            observed.observe('remote-driver: fetched 4 artifact(s) in 2.5s', 84.5)
            self.assertEqual(observed.reported_stages, {'cargo': 82.0, 'artifact_fetch': 2.5})

    def test_related_targets_share_one_invocation_without_losing_selection(self):
        args = argparse.Namespace(operation='test', package=['clock', 'lock'], lib=True, tests=False,
                                  test=['first', 'second'], filter=None, exact=False,
                                  cargo_command='remote-driver --no-fallback')
        self.assertEqual(feedback.command(args),
                         ['remote-driver', '--no-fallback', 'test', '--locked', '--color=never',
                          '-p', 'clock', '-p', 'lock', '--lib', '--test', 'first', '--test', 'second'])
        args.tests = True
        with self.assertRaises(ValueError):
            feedback.command(args)
        args.tests, args.lib, args.test = False, False, []
        with self.assertRaises(ValueError):
            feedback.command(args)

    def test_command_preserves_scope_and_disables_local_fallback(self):
        args = argparse.Namespace(operation='test', package=['shell'], lib=False, tests=False,
                                  test='it', filter='interaction_test::journey', exact=True, cargo_command='remote-driver --no-fallback')
        self.assertEqual(feedback.command(args),
                         ['remote-driver', '--no-fallback', 'test', '--locked', '--color=never', '-p', 'shell', '--test', 'it',
                          'interaction_test::journey', '--', '--exact'])
        args.operation = 'check'
        with self.assertRaises(ValueError):
            feedback.command(args)


if __name__ == '__main__':
    unittest.main()

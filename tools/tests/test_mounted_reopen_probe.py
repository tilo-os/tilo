import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('probe', ROOT / 'tools/ci/mounted_reopen_probe.py')
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
SHA = 'a' * 40


class MountedReopenProbeTests(unittest.TestCase):
    def test_exact_source_and_existing_evidence_fail_before_building(self):
        for source in ('main', 'a' * 39, 'A' * 40, SHA + '; command'):
            with self.assertRaises(ValueError):
                probe.request({'SOURCE_SHA': source})
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(probe.subprocess, 'check_output', return_value='b' * 40), \
                    patch.object(probe.subprocess, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'differs'):
                    probe.execute(root, SHA)
                run.assert_not_called()
            (root / 'target/mounted-resource-recovery').mkdir(parents=True)
            with patch.object(probe.subprocess, 'check_output', return_value=SHA), \
                    patch.object(probe.subprocess, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'preserve'):
                    probe.execute(root, SHA)
                run.assert_not_called()

    def test_real_failure_is_preserved_and_stops_before_another_attempt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = []
            def run(command, **kwargs):
                commands.append(command)
                self.assertEqual(kwargs['cwd'], root)
                if '--disconnect-retry' in command:
                    evidence = root / 'target/mounted-resource-recovery'
                    evidence.mkdir()
                    (evidence / 'failure.json').write_text('{"pageStatus":"refused"}')
                    return subprocess.CompletedProcess(command, 101)
                return subprocess.CompletedProcess(command, 0)
            with patch.object(probe.subprocess, 'check_output', return_value=SHA), \
                    patch.object(probe.subprocess, 'run', side_effect=run):
                self.assertEqual(probe.execute(root, SHA), 101)
            self.assertEqual(len(commands), 4)
            saved = root / 'target/mounted-reopen-probe/attempt-1'
            self.assertEqual(json.loads((saved / 'failure.json').read_text())['pageStatus'], 'refused')
            self.assertEqual(json.loads((saved / 'receipt.json').read_text())['exit_code'], 101)
            self.assertFalse((saved.parent / 'attempt-2').exists())

    def test_success_requires_observed_complete_journeys_and_preserves_all_three(self):
        for observed in (True, False):
            with self.subTest(observed=observed), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                attempts = []
                def run(command, **kwargs):
                    if '--disconnect-retry' in command:
                        attempts.append(command)
                        if observed:
                            evidence = root / 'target/mounted-resource-recovery'
                            evidence.mkdir()
                            (evidence / 'journey.json').write_text(json.dumps({
                                'mode': 'certified-browser-disconnect-retry',
                                'stages': ['governed-revocation-refuses-link-reopen']}))
                    return subprocess.CompletedProcess(command, 0)
                with patch.object(probe.subprocess, 'check_output', return_value=SHA), \
                        patch.object(probe.subprocess, 'run', side_effect=run):
                    if observed:
                        self.assertEqual(probe.execute(root, SHA), 0)
                    else:
                        with self.assertRaisesRegex(ValueError, 'evidence'):
                            probe.execute(root, SHA)
                self.assertEqual(len(attempts), 3 if observed else 1)
                for number in range(1, len(attempts) + 1):
                    self.assertTrue((root / f'target/mounted-reopen-probe/attempt-{number}/receipt.json').is_file())

    def test_launch_error_preserves_partial_fixture_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def run(command, **kwargs):
                if '--disconnect-retry' in command:
                    evidence = root / 'target/mounted-resource-recovery'
                    evidence.mkdir()
                    (evidence / 'failure.json').write_text('{"phase":"error"}')
                    raise OSError('owned fixture process failed')
                return subprocess.CompletedProcess(command, 0)
            with patch.object(probe.subprocess, 'check_output', return_value=SHA), \
                    patch.object(probe.subprocess, 'run', side_effect=run):
                with self.assertRaisesRegex(OSError, 'fixture process'):
                    probe.execute(root, SHA)
            saved = root / 'target/mounted-reopen-probe/attempt-1'
            self.assertTrue((saved / 'failure.json').is_file())
            self.assertEqual(json.loads((saved / 'receipt.json').read_text())['exit_code'], 1)

    def test_the_fixture_keeps_failure_evidence(self):
        fixture = (ROOT / 'crates/agent/agent-participant/tests/mounted_resource/driver.mjs').read_text()
        self.assertIn('130_000', fixture)
        self.assertIn("assert.equal(await tab.evaluate(entryOutcome), 'open'", fixture)
        self.assertIn('} catch (error) {', fixture)
        self.assertIn('throw error;', fixture)
        self.assertIn("phase:'error'", fixture)


if __name__ == '__main__':
    unittest.main()

"""Real WASM bytes and preserved production probes for host qualification."""
import copy
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('host_receipt', ROOT / 'tools/ci/sdk_web_host_receipt.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class HostReceipt(unittest.TestCase):
    def test_actual_modules_are_hashed_and_missing_or_mutated_input_is_observed(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory)
            for name in MODULE.NAMES:
                (bundle / name).write_bytes(b'\0asm\x01\0\0\0')
            initial = MODULE.wasm_inputs(bundle)
            self.assertEqual({row['sha256'] for row in initial.values()},
                             {hashlib.sha256(b'\0asm\x01\0\0\0').hexdigest()})
            changed = bundle / 'tilo_browser_runtime_bg.wasm'
            changed.write_bytes(b'\0asm\x01\0\0\0\0\0')
            self.assertNotEqual(MODULE.wasm_inputs(bundle), initial)
            changed.unlink()
            with self.assertRaises(RuntimeError):
                MODULE.wasm_inputs(bundle)

    def test_two_complete_receipts_with_identical_inputs_are_required(self):
        row = {'source': 'a' * 40, 'builder': 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'b' * 64,
               'wasm': {'runtime': {'bytes': 8, 'sha256': 'c' * 64}},
               'full_probes_passed': True, 'chrome_version': 'Google Chrome for Testing 155.0.8059.39'}
        rows = [dict(row, arch=arch) for arch in ('x86_64', 'aarch64')]
        self.assertTrue(MODULE.equivalent(rows)['full_probes_passed'])
        for field, value in (('source', 'different'), ('builder', 'different'),
                             ('wasm', {}), ('full_probes_passed', False),
                             ('chrome_version', 'Google Chrome 154.0.0.0')):
            changed = copy.deepcopy(rows)
            changed[1][field] = value
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                MODULE.equivalent(changed)
        with self.assertRaises(RuntimeError):
            MODULE.equivalent(rows[:1])
        with self.assertRaises(RuntimeError):
            MODULE.equivalent([rows[0], rows[0]])

    def test_every_existing_full_probe_command_is_preserved(self):
        production = yaml.safe_load((ROOT / '.forgejo/workflows/app-sdk.yml').read_text())['jobs']['web']
        trial = yaml.safe_load((ROOT / '.forgejo/workflows/sdk-web-host-qualification.yml').read_text())['jobs']['web']
        by_name = {step.get('name'): step for step in trial['steps'] if 'name' in step}
        for step in production['steps']:
            if 'run' not in step:
                continue
            candidate = by_name[step['name']]['run']
            candidate = candidate.replace('@puppeteer/browsers@3.2.4 install chrome@155.0.8059.39',
                                          '@puppeteer/browsers@2 install chrome@stable')
            candidate = ''.join(line for line in candidate.splitlines(keepends=True)
                                if "--config 'target.aarch64-unknown-linux-gnu.runner=" not in line)
            self.assertEqual(candidate, step['run'], step['name'])
        self.assertEqual(trial['env']['TILO_WEB_SMOKE_SHARDS'], '3')
        self.assertEqual([x['runner'] for x in trial['strategy']['matrix']['include']],
                         ['runner', 'runner-large-arm64'])
        self.assertFalse(trial['strategy']['fail-fast'])


if __name__ == '__main__':
    unittest.main()

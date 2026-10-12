"""Fail-closed compile-time capture admission and release packaging boundaries."""
import importlib.util
import json
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location('capture_admission', Path(__file__).with_name('admit-capture-sidecar.py'))
ADMISSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ADMISSION)


def pins():
    result = {name: 'a' * 64 for name in ADMISSION.DIGEST_FIELDS}
    result.update(schema=1, worker_protocol=6, arch='x86_64',
                  package_id='so.tilo.imager.home-export',
                  runtime_version='2026.07+native.' + 'b' * 32)
    return result


class CaptureAdmissionTests(unittest.TestCase):
    def test_closed_pin_contract_rejects_unsupported_or_ambiguous_inputs(self):
        valid = pins()
        self.assertEqual(ADMISSION.decode_pins(json.dumps(valid).encode(), 'x86_64'), valid)
        for key, value in (('schema', True), ('worker_protocol', 5), ('worker_protocol', True),
                           ('arch', 'aarch64'), ('package_id', 'so.tilo.other'),
                           ('runtime_version', '2026.07'), ('service_publisher', 'z' * 64)):
            with self.subTest(key=key, value=value):
                changed = dict(valid, **{key: value})
                with self.assertRaises(ValueError):
                    ADMISSION.decode_pins(json.dumps(changed).encode(), 'x86_64')
        with self.assertRaises(ValueError):
            ADMISSION.decode_pins(json.dumps(valid).encode()[:-1] + b',"schema":1}', 'x86_64')
        with self.assertRaises(ValueError):
            ADMISSION.decode_pins(json.dumps(dict(valid, ready=True)).encode(), 'x86_64')

    def test_redirected_pin_location_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'real').mkdir()
            (root / 'real/pins').write_text('{}')
            (root / 'alias').symlink_to(root / 'real', target_is_directory=True)
            with self.assertRaises(ValueError):
                ADMISSION.regular(root / 'alias/pins', 16384)

    def test_fresh_install_packaging_needs_no_capture_authority(self):
        with tempfile.TemporaryDirectory() as directory:
            env = dict(os.environ)
            env.pop('TILO_CAPTURE_PINS', None)
            env.pop('TILO_CAPTURE_SIDECAR', None)
            result = subprocess.run(['bash', str(ROOT / 'apps/imager/packaging/stage-capture-sidecar.sh'),
                                     '/missing/host', directory], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_no_ready_flag_can_replace_actual_release_qualification(self):
        # A package staging tree with no actual qualifier must refuse before
        # copying a sidecar, even when a caller supplies a ready flag.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            production = root / 'apps/imager/packaging/stage-capture-sidecar.sh'
            production.parent.mkdir(parents=True)
            shutil.copyfile(ROOT / 'apps/imager/packaging/stage-capture-sidecar.sh', production)
            prefix = root / 'package'
            prefix.mkdir()
            env = dict(os.environ, TILO_CAPTURE_PINS='/missing/pins',
                       TILO_CAPTURE_SIDECAR='/missing/sidecar', TILO_CAPTURE_READY='1')
            result = subprocess.run(['bash', str(production), '/missing/host', str(prefix)],
                                    env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('actual native release qualifier', result.stderr)
            self.assertEqual(list(prefix.iterdir()), [])


if __name__ == '__main__':
    unittest.main()

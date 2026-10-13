#!/usr/bin/env python3
"""Exercise frozen recipe surgery; no engine, compiler or downloads."""
import importlib.util
import hashlib
import json
from pathlib import Path
import subprocess
import unittest

from frozen_source_fixture import NATIVE_DRIVER, NATIVE_RECIPE

ROOT = Path(__file__).parents[2]
spec = importlib.util.spec_from_file_location('native_resume', ROOT / 'os/dev/native_resume.py')
resume = importlib.util.module_from_spec(spec)
spec.loader.exec_module(resume)
REQUIRED = ('shell fs tilo-model-manager tilo-speech-worker tilo-model-gateway tilo-relay '
 'tilo-relay-admin tilo-enroll tilo-home-peer-control tilo-greeter tilo-installer tilo-runtime-bundle '
 'tilo-launcher tilo-app-host tilo-unlock-helper tilo-rollback tilo-crypto tilo-image-trust '
 'tilo-device-key-init tilo tilo-app-package tilo-install tilo-cli-shim tilo-ush tilo-system-extension-manager '
 'tilo-projector tilo-windows-vm-control tilo-windows-rail tilo-windows-runtime').split()


class ResumeRecipe(unittest.TestCase):
    original = NATIVE_RECIPE

    def test_resume_phase_selection(self):
        with self.subTest(phase="only_missing_bins_selected_remaining_phases_unchanged"):
            retained = set(REQUIRED) | {'browser'}
            retained -= {'tilo-crypto', 'tilo-windows-rail'}
            derived, missing = resume.derive_recipe(self.original, retained, REQUIRED)
            self.assertEqual(missing, {'release': ['tilo-crypto'], 'windows': ['tilo-windows-rail']})
            self.assertIn('cargo build --release ${RELEASE_ARGS} -p tilo-relay --bin tilo-crypto ${BROWSER_PKG}', derived)
            self.assertIn('cargo build --release -p windows-vm --bin tilo-windows-rail', derived)
            self.assertNotIn(' --bins ', derived)
            self.assertLess(derived.index('cargo clean --profile thin-app'), derived.index('cp /tilo-resume/partial/*'))
            marker = '    && build_phase rust-output-validation'
            self.assertEqual(derived[derived.index(marker):], self.original[self.original.index(marker):])
            self.assertEqual(derived.count('from=tilo-resume'), 1)
            self.assertIn('CARGO_PROFILE_RELEASE_LTO=thin', derived)
            self.assertIn('CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8', derived)
            self.assertNotIn('\n+', derived)
        with self.subTest(phase="all_release_present_still_keeps_thin_runtime_and_packaging"):
            derived, missing = resume.derive_recipe(self.original, set(REQUIRED) | {'browser'}, REQUIRED)
            self.assertEqual(missing, {'release': [], 'windows': []})
            self.assertIn('/build/os/scripts/build-thin-apps.sh', derived)
            self.assertIn('/build/os/scripts/pack-apps.sh', derived)
            self.assertNotIn('cargo build --release', derived)
        with self.subTest(phase="native_driver_does_not_need_a_kernel_and_full_image_still_does"):
            original = NATIVE_DRIVER
            derived = resume.derive_driver(original, '/work/repo', '/work/Containerfile.resumed', 'sha256:' + 'a' * 64)
            start = derived.index('if [ "${TILO_NATIVE_BUILDER_ONLY:-0}" = 1 ]; then\n  KERNEL_RPMS_REF=scratch')
            block = derived[start:derived.index('# Pre-flight: every host-side COPY source exists.', start)]
            common = 'TILO_OS_DIR=/nonexistent-native-resume-fixture\nuname() { echo aarch64; }\ndie() { exit 1; }\n'
            for mode, kernel, expected in [('1', '', 'scratch'), ('0', 'pinned-kernel', 'pinned-kernel')]:
                result = subprocess.run(['bash', '-euc', common + 'TILO_NATIVE_BUILDER_ONLY=' + mode +
                    '\nKERNEL_RPMS_REF=' + kernel + '\n' + block + '\nprintf "%s" "$KERNEL_RPMS_REF"'],
                    capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, expected)
            missing = subprocess.run(['bash', '-euc', common + 'TILO_NATIVE_BUILDER_ONLY=0\nKERNEL_RPMS_REF=\n' + block],
                                     capture_output=True)
            self.assertNotEqual(missing.returncode, 0)

    def test_recipe_and_driver_binding(self):
        with self.subTest(phase="unknown_empty_or_changed_recipe_rejected"):
            for retained in (set(), {'shots'}):
                with self.assertRaises(ValueError):
                    resume.derive_recipe(self.original, retained, REQUIRED)
            with self.assertRaises(ValueError):
                resume.derive_recipe(self.original.replace('cargo clean --profile thin-app', 'true'), {'shell'}, REQUIRED)
        with self.subTest(phase="driver_hash_reproduced_from_exported_public_parameters"):
            original = NATIVE_DRIVER
            record = {'source_path': '/work/frozen source',
                      'derived_recipe_path': '/work/tilo-native-resume-random/Containerfile.resumed',
                      'seed_image_id': 'sha256:' + 'a' * 64}
            # JSON roundtrip models independent use of the exported receipt after
            # the temporary directory and seed image have been removed.
            derived = resume.derive_driver(original, **record)
            record['derived_driver_sha256'] = hashlib.sha256(derived.encode()).hexdigest()
            loaded = json.loads(json.dumps(record))
            expected = loaded.pop('derived_driver_sha256')
            self.assertEqual(hashlib.sha256(resume.derive_driver(original, **loaded).encode()).hexdigest(), expected)
            loaded['seed_image_id'] = 'sha256:' + 'b' * 64
            self.assertNotEqual(hashlib.sha256(resume.derive_driver(original, **loaded).encode()).hexdigest(), expected)


if __name__ == '__main__':
    unittest.main()

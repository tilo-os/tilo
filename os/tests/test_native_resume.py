#!/usr/bin/env python3
"""Exercise exact frozen recipe surgery; no engine, compiler or downloads."""
import importlib.util
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).parents[2]
spec = importlib.util.spec_from_file_location('native_resume', ROOT / 'os/dev/native_resume.py')
resume = importlib.util.module_from_spec(spec)
spec.loader.exec_module(resume)
REQUIRED = ('shell fs terminal file-explorer settings tilo-model-manager tilo-model-gateway tilo-relay '
 'tilo-relay-admin tilo-enroll tilo-home-peer-control tilo-greeter tilo-installer tilo-runtime-bundle '
 'tilo-launcher tilo-app-host tilo-unlock-helper tilo-rollback tilo-crypto tilo-image-trust '
 'tilo-device-key-init tilo tilo-pkg tilo-install tilo-cli-shim tilo-ush tilo-system-extension-manager '
 'tilo-projector tilo-windows-vm-control tilo-windows-rail tilo-windows-runtime').split()


class ResumeRecipe(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.original = subprocess.check_output(['git', '-C', str(ROOT), 'show', resume.SOURCE + ':os/Containerfile']).decode()

    def test_only_missing_bins_selected_remaining_phases_unchanged(self):
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

    def test_all_release_present_still_keeps_thin_runtime_and_packaging(self):
        derived, missing = resume.derive_recipe(self.original, set(REQUIRED) | {'browser'}, REQUIRED)
        self.assertEqual(missing, {'release': [], 'windows': []})
        self.assertIn('/build/os/scripts/build-thin-apps.sh', derived)
        self.assertIn('/build/os/scripts/pack-apps.sh', derived)
        self.assertNotIn('cargo build --release', derived)

    def test_unknown_empty_or_changed_recipe_rejected(self):
        for retained in (set(), {'shots'}):
            with self.assertRaises(ValueError):
                resume.derive_recipe(self.original, retained, REQUIRED)
        with self.assertRaises(ValueError):
            resume.derive_recipe(self.original.replace('cargo clean --profile thin-app', 'true'), {'shell'}, REQUIRED)


if __name__ == '__main__':
    unittest.main()

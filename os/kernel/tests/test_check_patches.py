#!/usr/bin/env python3
import importlib.util
import tomllib
import unittest
from pathlib import Path

KERNEL = Path(__file__).parents[1]
spec = importlib.util.spec_from_file_location('check_patches', KERNEL / 'check-patches.py')
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

MERGED = {'file': 'a.patch', 'upstream': 'series', 'merged': 'v7.3', 'commit': '0' * 40}
OWN = {'file': 'b.patch', 'upstream': "Tilo's own"}


class PatchRecordTests(unittest.TestCase):
    def test_the_record_covers_the_patches(self):
        record = tomllib.loads((KERNEL / 'patches.toml').read_text())
        patches = sorted(path.name for path in (KERNEL / 'patches').glob('*.patch'))
        self.assertEqual(check.problems(record, patches, None), [])

    def test_unrecorded_and_stale_entries(self):
        record = {'fedora_kernel': 'kernel-7.2.10-200.fc44', 'patch': [OWN, OWN]}
        found = check.problems(record, ['b.patch', 'c.patch'], None)
        self.assertTrue(any('c.patch has no entry' in p for p in found))
        self.assertTrue(any('b.patch has more than one entry' in p for p in found))
        found = check.problems({'fedora_kernel': 'x', 'patch': [OWN]}, [], None)
        self.assertTrue(any('not in the patch directory' in p for p in found))

    def test_a_merged_patch_goes_once_fedora_carries_its_release(self):
        record = {'fedora_kernel': 'kernel-7.2.10-200.fc44', 'patch': [MERGED, OWN]}
        files = ['a.patch', 'b.patch']
        for fedora in ['kernel-7.2.10-200.fc44', 'kernel-6.19.3-100.fc43']:
            self.assertEqual(check.problems(record, files, fedora), [])
        for fedora in ['kernel-7.3.0-200.fc44', 'kernel-7.3.0-0.rc2.20261001git1.fc44', 'kernel-8.0.1-200.fc45']:
            found = check.problems(record, files, fedora)
            self.assertEqual(len(found), 1)
            self.assertIn('a.patch is in mainline v7.3', found[0])

    def test_a_merged_patch_names_release_and_commit(self):
        partial = {k: v for k, v in MERGED.items() if k != 'commit'}
        found = check.problems({'fedora_kernel': 'x', 'patch': [partial]}, ['a.patch'], None)
        self.assertTrue(any('names both' in p for p in found))
        short = MERGED | {'commit': 'abc123'}
        found = check.problems({'fedora_kernel': 'x', 'patch': [short]}, ['a.patch'], None)
        self.assertTrue(any('full commit id' in p for p in found))


if __name__ == '__main__':
    unittest.main()

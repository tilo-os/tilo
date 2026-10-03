#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    'kernel_config_values', Path(__file__).parents[1] / 'kernel/check-config.py')
config = importlib.util.module_from_spec(spec)
spec.loader.exec_module(config)


class ResolvedConfigTests(unittest.TestCase):
    def test_disabled_and_required_forms(self):
        with self.subTest(phase="disabled_omitted_and_explicit_forms"):
            for actual in ('', 'CONFIG_CHILD=n', '# CONFIG_CHILD is not set'):
                for expected in ('CONFIG_CHILD=n', '# CONFIG_CHILD is not set'):
                    self.assertEqual(config.mismatches(config.settings(actual),
                                                      config.settings(expected)), [])
        with self.subTest(phase="disabled_still_rejects_reverse_selection"):
            for actual in ('y', 'm'):
                self.assertEqual(config.mismatches({'CONFIG_DEBUG_FS': actual},
                                                  {'CONFIG_DEBUG_FS': 'n'}),
                                 [('CONFIG_DEBUG_FS', 'n', actual)])
        with self.subTest(phase="required_enabled_never_accepts_absence"):
            for required in ('y', 'm', '1000', '"value"'):
                self.assertTrue(config.mismatches({}, {'CONFIG_REQUIRED': required}))
        with self.subTest(phase="transitional_required_option_is_not_silently_accepted"):
            resolved = config.settings('CONFIG_KMALLOC_PARTITION_CACHES=y\n'
                                       'CONFIG_KMALLOC_PARTITION_RANDOM=y')
            self.assertTrue(config.mismatches(resolved, {'CONFIG_RANDOM_KMALLOC_CACHES': 'y'}))
            self.assertFalse(config.mismatches(resolved, resolved))

    def test_exact_and_conflicting_values(self):
        with self.subTest(phase="exact_values_including_strings"):
            text = 'CONFIG_ARM64=y\nCONFIG_DRIVER=m\nCONFIG_HZ=1000\nCONFIG_TEXT="a b"'
            values = config.settings(text)
            self.assertFalse(config.mismatches(values, values))
            self.assertTrue(config.mismatches(values, {'CONFIG_DRIVER': 'y'}))
            self.assertTrue(config.mismatches(values, {'CONFIG_HZ': '250'}))
        with self.subTest(phase="conflicting_resolved_values_are_rejected"):
            with self.assertRaises(ValueError):
                config.settings('CONFIG_TEST=y\n# CONFIG_TEST is not set')


if __name__ == '__main__':
    unittest.main()

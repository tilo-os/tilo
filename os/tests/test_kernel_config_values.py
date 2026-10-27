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



class LenovoInputBusTests(unittest.TestCase):
    """The 83ED's built-in keyboard and touchpad are I2C HID, and the unlock
    prompt runs before a module could be loaded, so the whole path is built
    in. A modular driver there is reachable only after the root is open."""

    FRAGMENT = Path(__file__).parents[1] / 'kernel/config-fragments/aarch64.config'
    DRIVERS = ('CONFIG_I2C', 'CONFIG_I2C_QCOM_GENI', 'CONFIG_I2C_HID_OF',
               'CONFIG_INPUT_EVDEV', 'CONFIG_INPUT_KEYBOARD')

    def test_the_input_path_is_built_in(self):
        values = config.settings(self.FRAGMENT.read_text(encoding='utf-8'))
        for driver in self.DRIVERS:
            with self.subTest(driver=driver):
                self.assertEqual(config.mismatches(values, {driver: 'y'}), [])

    def test_the_drivers_ride_on_fedoras_hid_core(self):
        values = config.settings(self.FRAGMENT.read_text(encoding='utf-8'))
        # The fragment turns nothing off: HID core stays as Fedora builds it.
        self.assertEqual(config.mismatches(values, {'CONFIG_HID': 'y'}), [])


class SurfaceCameraSupportTests(unittest.TestCase):
    """Surface tablets and laptops get their embedded-controller stack (battery,
    thermal, lid, tablet-mode switch, integrated keyboard and touchpad) and their
    IPU6 webcam from mainline drivers. Fedora's stock config leaves the Surface
    menu unset, which cascades every driver under it to disabled, so a fragment
    that loses one line silently drops that hardware on a stock image."""

    FRAGMENT = Path(__file__).parents[1] / 'kernel/config-fragments/x86_64.config'
    DRIVERS = (
        'CONFIG_SURFACE_AGGREGATOR',
        'CONFIG_SURFACE_AGGREGATOR_HUB',
        'CONFIG_SURFACE_AGGREGATOR_REGISTRY', 'CONFIG_SURFACE_AGGREGATOR_CDEV',
        'CONFIG_SURFACE_ACPI_NOTIFY', 'CONFIG_SURFACE_AGGREGATOR_TABLET_SWITCH',
        'CONFIG_SURFACE_PLATFORM_PROFILE', 'CONFIG_SURFACE_DTX',
        'CONFIG_SURFACE_KBD', 'CONFIG_SURFACE_HID', 'CONFIG_SURFACE_GPE',
        'CONFIG_SURFACE_HOTPLUG', 'CONFIG_BATTERY_SURFACE',
        'CONFIG_CHARGER_SURFACE', 'CONFIG_SENSORS_SURFACE_FAN',
        'CONFIG_SENSORS_SURFACE_TEMP', 'CONFIG_HID_MICROSOFT',
        'CONFIG_IPU_BRIDGE', 'CONFIG_VIDEO_INTEL_IPU6', 'CONFIG_VIDEO_OV01A10',
        'CONFIG_VIDEO_OV02C10', 'CONFIG_VIDEO_OV08X40', 'CONFIG_VIDEO_HI556',
        'CONFIG_VIDEO_DW9714',
    )
    # `bool` symbols: the menu gate and the aggregator bus are part of the core
    # module, not modules of their own.
    GATES = ('CONFIG_SURFACE_PLATFORMS', 'CONFIG_SURFACE_AGGREGATOR_BUS')

    def test_the_surface_stack_is_requested_on_x86(self):
        values = config.settings(self.FRAGMENT.read_text(encoding='utf-8'))
        for driver in self.DRIVERS:
            with self.subTest(driver=driver):
                self.assertEqual(config.mismatches(values, {driver: 'm'}), [])
        for gate in self.GATES:
            with self.subTest(gate=gate):
                self.assertEqual(config.mismatches(values, {gate: 'y'}), [])

    def test_the_drivers_ride_on_the_shared_core(self):
        shared = config.settings((Path(__file__).parents[1] / 'kernel/config-fragments/tilo.config')
                                 .read_text(encoding='utf-8'))
        for core in ('CONFIG_MEDIA_SUPPORT', 'CONFIG_VIDEO_DEV'):
            with self.subTest(core=core):
                self.assertEqual(config.mismatches(shared, {core: 'm'}), [])


if __name__ == '__main__':
    unittest.main()

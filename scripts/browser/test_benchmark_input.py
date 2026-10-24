"""Offline checks for the native-input measurement's causal frame marker."""
import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location('benchmark_input', Path(__file__).with_name('benchmark-input.py'))
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class InputMeasurement(unittest.TestCase):
    def test_marker_requires_the_fixture_pixel_not_an_unrelated_frame(self):
        packet = b'\x02' + struct.pack('<II', 1, 1) + bytes([64, 255, 7, 255])
        self.assertEqual(benchmark.frame_marker(packet), {'event': 'frame', 'size': (1, 1), 'marker': 7})
        unrelated = packet[:-4] + bytes([255, 255, 255, 255])
        self.assertIsNone(benchmark.frame_marker(unrelated)['marker'])
        transparent = packet[:-1] + b'\x00'
        self.assertIsNone(benchmark.frame_marker(transparent)['marker'])

    def test_malformed_frames_cannot_be_valid_measurements(self):
        packet = b'\x02' + struct.pack('<II', 1, 1) + bytes([64, 255, 7, 255])
        for invalid in [b'', packet[:-1], packet + b'\x00', b'\x03' + packet[1:],
                        b'\x02' + struct.pack('<II', 0, 1) + packet[-4:]]:
            with self.subTest(packet=invalid), self.assertRaises(ValueError):
                benchmark.frame_marker(invalid)

    def test_memory_pressure_must_be_observable_and_finite(self):
        self.assertEqual(benchmark.parse_memory_pressure('some avg10=7.2\nfull avg10=1.5 avg60=2 total=123\n'), 1.5)
        for text in ['', 'some avg10=1', 'full avg10=nan', 'full avg10=-1', 'full avg10=101']:
            with self.subTest(text=text), self.assertRaises(ValueError):
                benchmark.parse_memory_pressure(text)

    def test_nearest_rank_percentiles_and_invalid_samples(self):
        self.assertEqual(benchmark.percentile([4, 1, 3, 2], 50), 2)
        self.assertEqual(benchmark.percentile(list(range(1, 101)), 95), 95)
        for samples in [[], [float('nan')], [float('inf')], [-1]]:
            with self.subTest(samples=samples), self.assertRaises(ValueError):
                benchmark.percentile(samples, 95)


if __name__ == '__main__':
    unittest.main()

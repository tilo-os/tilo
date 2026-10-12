"""Closed executable qualification contract; no capture or native tool invocation."""
import unittest
import capture_protocol


class CaptureProtocol(unittest.TestCase):
    def test_exact_advertisement_stamps_actual_worker_version(self):
        self.assertEqual(capture_protocol.advertisement(b'{"schema":1,"worker_protocol":5}\n'), 5)
        self.assertEqual(capture_protocol.advertisement(b'{"schema":1,"worker_protocol":6}\n'), 6)

    def test_actual_advertisement_cannot_be_relabelled_as_another_cohort(self):
        for actual in (5, 6):
            output = ('{"schema":1,"worker_protocol":%d}' % actual).encode()
            self.assertEqual(capture_protocol.for_cohort(output, actual), actual)
            with self.assertRaises(ValueError):
                capture_protocol.for_cohort(output, 11 - actual)
        for expected in (True, '6', 7):
            with self.assertRaises(ValueError):
                capture_protocol.for_cohort(b'{"schema":1,"worker_protocol":6}', expected)

    def test_missing_old_unknown_boolean_and_duplicate_versions_refuse(self):
        for output in [b'{"schema":1}', b'{"schema":1,"worker_protocol":1}',
                       b'{"schema":1,"worker_protocol":2}',
                       b'{"schema":1,"worker_protocol":3}',
                       b'{"schema":1,"worker_protocol":4}',
                       b'{"schema":1,"worker_protocol":true}',
                       b'{"schema":1,"worker_protocol":5,"worker_protocol":5}',
                       b'{"schema":1,"worker_protocol":5,"path":"/private"}', b'x' * 4097]:
            with self.subTest(output=output[:100]):
                with self.assertRaises(ValueError):
                    capture_protocol.advertisement(output)


if __name__ == '__main__':
    unittest.main()

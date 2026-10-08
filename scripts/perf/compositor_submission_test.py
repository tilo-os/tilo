import unittest
from compositor_phases import submission_records


def record(serial=41, status='complete', **changes):
    row = dict(serial=serial, status=status, scene_count=1, command_count=1,
               finish_count=1, submit_count=1, scene_record_us=3,
               command_encode_us=4, encoder_finish_us=0, queue_submit_us=0,
               sparse_marker_submit_us='none')
    row.update(changes)
    return 'FRAME-SUBMISSION ' + ' '.join(f'{key}={value}' for key, value in row.items())


class SubmissionBoundaries(unittest.TestCase):
    def test_exact_serial_preserves_real_zero_and_separate_sparse_marker(self):
        result = submission_records([record(), record(42, sparse_marker_submit_us=8)])
        self.assertTrue(result[41]['available'])
        self.assertEqual(result[41]['costs_us']['encoder_finish_us'], 0)
        self.assertEqual(result[42]['costs_us']['sparse_marker_submit_us'], 8)
        self.assertNotIn(43, result)

    def test_old_capture_missing_and_incomplete_are_unavailable(self):
        self.assertEqual(submission_records(['FRAME-LIFECYCLE serial=41']), {})
        result = submission_records([record(status='incomplete', submit_count=0, queue_submit_us='none')])
        self.assertFalse(result[41]['available'])
        self.assertIsNone(result[41]['costs_us']['queue_submit_us'])

    def test_missing_not_zero_and_duplicate_or_partial_completion_refused(self):
        for lines in ([record(), record()], [record(submit_count=0)],
                      [record(submit_count=0, queue_submit_us='none')],
                      [record(status='missing')], [record(serial=0)]):
            with self.assertRaises(ValueError):
                submission_records(lines)

    def test_invalid_bounded_scope_is_explicit_not_complete(self):
        result = submission_records([record(status='invalid', scene_count=256)])
        self.assertFalse(result[41]['available'])
        with self.assertRaises(ValueError):
            submission_records([record(scene_count=257, command_count=257,
                                       finish_count=257, submit_count=257)])


if __name__ == '__main__':
    unittest.main()

import unittest
from compositor_gpu import gpu_submission_records, summarize_compositor_gpu


def gpu_record(serial=41, **changes):
    row = dict(serial=serial, revision=9, status='complete', process_id=42,
               encoder_supported='true', encoder_enabled='true', external_queue_work='false', renderer_generation=7,
               first_recording=3, last_recording=3, recording_count=1, measured_count=1,
               stale_count=0, recording_ns=30, post_render_ns=15,
               scope='encoder_before_query_resolve')
    row.update(changes)
    return 'FRAME-GPU-SUBMISSION ' + ' '.join(f'{key}={value}' for key, value in row.items())


class GpuSubmissionBoundaries(unittest.TestCase):
    def test_real_zero_is_distinct_from_missing_and_nested_duration_is_not_summed(self):
        result = gpu_submission_records([gpu_record(recording_ns=0, post_render_ns=0)])
        self.assertTrue(result[41]['available'])
        self.assertEqual(result[41]['recording_ns'], 0)
        self.assertTrue(result[41]['excludes_pre_execution_queue'])
        missing = gpu_submission_records([gpu_record(status='missing', renderer_generation='none',
            first_recording='none', last_recording='none', recording_count=0, measured_count=0,
            recording_ns='none', post_render_ns='none')])
        self.assertIsNone(missing[41]['recording_ns'])
        self.assertFalse(missing[41]['available'])

    def test_partial_range_and_old_capture_cannot_claim_complete(self):
        self.assertEqual(gpu_submission_records(['FRAME-LIFECYCLE serial=41']), {})
        result = gpu_submission_records([gpu_record(status='incomplete', last_recording=4, recording_count=2)])
        self.assertFalse(result[41]['available'])
        with self.assertRaises(ValueError):
            gpu_submission_records([gpu_record(last_recording=4, recording_count=2)])

    def test_reused_recordings_duplicate_serial_and_wrong_ranges_refuse(self):
        for rows in ([gpu_record(), gpu_record()], [gpu_record(), gpu_record(42)],
                     [gpu_record(last_recording=4)], [gpu_record(first_recording=0)],
                     [gpu_record(), gpu_record(42, process_id=43, first_recording=4, last_recording=4)],
                     [gpu_record(recording_count=257)], [gpu_record(renderer_generation='none')]):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                gpu_submission_records(rows)
        result = gpu_submission_records([gpu_record(), gpu_record(42, renderer_generation=8)])
        self.assertEqual(len(result), 2)

    def test_wrong_scope_unavailable_observation_and_overlap_sum_refuse(self):
        for changes in (dict(scope='estimated'), dict(status='unsupported'), dict(stale_count=1),
                        dict(post_render_ns=31), dict(recording_ns='none'), dict(measured_count=2),
                        dict(encoder_enabled='false'), dict(external_queue_work='true'),
                        dict(recording_ns=1 << 64)):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                gpu_submission_records([gpu_record(**changes)])


class RetiredFrameOwnership(unittest.TestCase):
    def test_joins_exact_serial_revision_and_preserves_missing(self):
        result = summarize_compositor_gpu([gpu_record(),
            'FRAME-LIFECYCLE serial=41 revision=9 event=render-retired phase_timing=true',
            'FRAME-LIFECYCLE serial=42 revision=10 event=render-retired phase_timing=false'])
        self.assertTrue(result['frames'][41]['submission_gpu']['available'])
        self.assertEqual(result['frames'][42]['submission_gpu']['status'], 'missing')
        self.assertFalse(result['performance_certified'])

    def test_missing_mismatched_and_duplicate_retirement_refuse(self):
        for rows in ([gpu_record()],
                     [gpu_record(), 'FRAME-LIFECYCLE serial=41 revision=10 event=render-retired phase_timing=true'],
                     [gpu_record(), 'FRAME-LIFECYCLE serial=41 revision=9 event=render-retired phase_timing=false'],
                     ['FRAME-LIFECYCLE serial=41 revision=9 event=render-retired phase_timing=true'] * 2):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                summarize_compositor_gpu(rows)


if __name__ == '__main__':
    unittest.main()

import unittest
from frame_presentation import analyze


def receipt(frame=1, lo=10.0, hi=10.001, pid=42):
    return ('qos-webview-received', dict(process_id=pid, frame_id=frame, tab_id=1,
        host_clock='mach_absolute_seconds', received_host_lo_s=lo, received_host_hi_s=hi))

def scene(cpu=5, ids=None, **kwargs):
    return ('qos-webview-scene', dict(process_id=42, cpu_frame_id=cpu,
        webview_frame_ids=[1] if ids is None else ids, overflow=False) | kwargs)

def display(cpu=5, time=10.017):
    return ('qos-display-frame', dict(cpu_frame_id=cpu, worker_id=1, worker_frame_ix=cpu,
        drawable_id=cpu, presented_time_s=time, backend='metal'))

class FramePresentationTests(unittest.TestCase):
    def test_exact_identity_join_and_first_cached_presentation(self):
        result = analyze([display(6, 10.030), scene(6), receipt(), scene(), display()])
        self.assertEqual(len(result['samples']), 1)
        sample = result['samples'][0]
        self.assertEqual(sample['cpu_frame_id'], 5)
        self.assertAlmostEqual(sample['latency_low_ms'], 16)
        self.assertAlmostEqual(sample['latency_high_ms'], 17)
        self.assertEqual(result['unpresented_frame_ids'], [])

    def test_zero_based_cpu_and_worker_frame_indices_are_valid(self):
        result = analyze([receipt(), scene(cpu=0), display(cpu=0)])
        self.assertEqual(len(result["samples"]), 1)
        self.assertFalse(result["issues"])

    def test_overwritten_receipts_are_not_zero_latency(self):
        result = analyze([receipt(), receipt(2), scene(ids=[2]), display()])
        self.assertEqual(result['unpresented_frame_ids'], [1])
        self.assertEqual(len(result['samples']), 1)

    def test_dropped_scene_has_no_sample(self):
        result = analyze([receipt(), scene()])
        self.assertEqual(result['samples'], [])
        self.assertEqual(result['unpresented_frame_ids'], [1])

    def test_duplicate_presentation_is_ambiguous(self):
        result = analyze([receipt(), scene(), display(), display(time=10.020)])
        self.assertEqual(result['samples'], [])
        self.assertIn('ambiguous_presentation', result['issues'])

    def test_overflow_and_missing_receipt_are_explicit(self):
        for bad in [scene(overflow=True), scene(ids=[99])]:
            result = analyze([receipt(), bad, display()])
            self.assertEqual(result['samples'], [])
            self.assertTrue(result['issues'])

    def test_invalid_clocks_and_timestamps_are_rejected(self):
        for bad in [receipt(hi=9), receipt(lo=float('nan')), receipt(hi=None)]:
            self.assertEqual(analyze([bad, scene(), display()])['samples'], [])
        for value in [0, float('nan'), float('inf'), 9]:
            self.assertEqual(analyze([receipt(), scene(), display(time=value)])['samples'], [])

    def test_mixed_process_log_cannot_join_pidless_display_records(self):
        result = analyze([receipt(), receipt(2, pid=43), scene(), display()])
        self.assertEqual(result['samples'], [])
        self.assertIn('mixed_processes', result['issues'])

    def test_duplicate_receipt_and_null_cpu_do_not_guess(self):
        for rows in [[receipt(), receipt(), scene(), display()], [receipt(), scene(cpu=None), display()]]:
            result = analyze(rows)
            self.assertEqual(result['samples'], [])
            self.assertTrue(result['issues'])

if __name__ == '__main__':
    unittest.main()

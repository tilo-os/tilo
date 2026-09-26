import unittest
from frame_presentation import analyze


def receipt(frame=1, lo=10.0, hi=10.001, pid=42):
    return ('tilo-webview-received', dict(process_id=pid, frame_id=frame, tab_id=1,
        host_clock='mach_absolute_seconds', received_host_lo_s=lo, received_host_hi_s=hi))

def scene(cpu=5, ids=None, **kwargs):
    return ('tilo-webview-scene', dict(process_id=42, cpu_frame_id=cpu,
        webview_frame_ids=[1] if ids is None else ids, overflow=False) | kwargs)

def display(cpu=5, time=10.017):
    return ('tilo-display-frame', dict(cpu_frame_id=cpu, worker_id=1, worker_frame_ix=cpu,
        drawable_id=cpu, presented_time_s=time, backend='metal'))

class FramePresentationTests(unittest.TestCase):
    def test_receipts_join_to_their_first_exact_presentation(self):
        with self.subTest(case='exact identity join and first cached presentation'):
            result = analyze([display(6, 10.030), scene(6), receipt(), scene(), display()])
            self.assertEqual(len(result['samples']), 1)
            sample = result['samples'][0]
            self.assertEqual(sample['cpu_frame_id'], 5)
            self.assertAlmostEqual(sample['latency_low_ms'], 16)
            self.assertAlmostEqual(sample['latency_high_ms'], 17)
            self.assertEqual(result['unpresented_frame_ids'], [])
        with self.subTest(case='zero-based cpu and worker frame indices are valid'):
            result = analyze([receipt(), scene(cpu=0), display(cpu=0)])
            self.assertEqual(len(result["samples"]), 1)
            self.assertFalse(result["issues"])
        with self.subTest(case='overwritten receipts are not zero latency'):
            result = analyze([receipt(), receipt(2), scene(ids=[2]), display()])
            self.assertEqual(result['unpresented_frame_ids'], [1])
            self.assertEqual(len(result['samples']), 1)

    def test_unjoinable_or_ambiguous_evidence_yields_no_sample(self):
        with self.subTest(case='dropped scene'):
            result = analyze([receipt(), scene()])
            self.assertEqual(result['samples'], [])
            self.assertEqual(result['unpresented_frame_ids'], [1])
        with self.subTest(case='duplicate presentation is ambiguous'):
            result = analyze([receipt(), scene(), display(), display(time=10.020)])
            self.assertEqual(result['samples'], [])
            self.assertIn('ambiguous_presentation', result['issues'])
        with self.subTest(case='mixed process log cannot join pidless display records'):
            result = analyze([receipt(), receipt(2, pid=43), scene(), display()])
            self.assertEqual(result['samples'], [])
            self.assertIn('mixed_processes', result['issues'])
        # Explicit issues: overflow, missing receipt, duplicate receipt, null cpu.
        for label, rows in (
            ('overflow', [receipt(), scene(overflow=True), display()]),
            ('missing receipt', [receipt(), scene(ids=[99]), display()]),
            ('duplicate receipt', [receipt(), receipt(), scene(), display()]),
            ('null cpu', [receipt(), scene(cpu=None), display()]),
        ):
            with self.subTest(case=label):
                result = analyze(rows)
                self.assertEqual(result['samples'], [])
                self.assertTrue(result['issues'])
        for label, bad in (('receipt hi before lo', receipt(hi=9)),
                           ('receipt nan lo', receipt(lo=float('nan'))),
                           ('receipt missing hi', receipt(hi=None))):
            with self.subTest(case=label):
                self.assertEqual(analyze([bad, scene(), display()])['samples'], [])
        for value in [0, float('nan'), float('inf'), 9]:
            with self.subTest(case=f'display time {value}'):
                self.assertEqual(analyze([receipt(), scene(), display(time=value)])['samples'], [])

if __name__ == '__main__':
    unittest.main()

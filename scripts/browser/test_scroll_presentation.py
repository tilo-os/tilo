import unittest
from scroll_presentation import analyze
from test_frame_presentation import receipt, scene, display


def input_row(identifier=1, dy=-1.625, lo=9.990, hi=9.991):
    return ('tilo-native-input', dict(id=identifier, input=dict(kind='wheel', dx=0, dy=dy),
        host_clock='mach_absolute_seconds', dispatch_host_lo_s=lo, dispatch_host_hi_s=hi,
        warmup=False))

def evidence(marker=65):
    received = receipt()
    received[1]['scroll_marker_delta'] = marker
    return [('tilo-native-window', dict(scale_factor=2)), input_row(), received, scene(), display()]

class ScrollPresentationTests(unittest.TestCase):
    def test_wheel_delta_matches_first_visible_scroll_acknowledgement(self):
        with self.subTest(case='native line wheel delta'):
            result = analyze(evidence())
            self.assertEqual(len(result['samples']), 1)
            self.assertEqual(result['samples'][0]['input_id'], 1)
            self.assertAlmostEqual(result['samples'][0]['latency_low_ms'], 26)
            self.assertAlmostEqual(result['samples'][0]['latency_high_ms'], 27)
        with self.subTest(case='first valid presentation across repeated marker frames'):
            rows = evidence()
            second = receipt(frame=2, lo=10.02, hi=10.021)
            second[1]['scroll_marker_delta'] = 65
            rows += [second, scene(cpu=6, ids=[2]), display(cpu=6, time=10.04)]
            self.assertEqual(analyze(rows)['samples'][0]['frame_id'], 1)

    def test_unrelated_ambiguous_or_invalid_evidence_is_not_a_scroll_response(self):
        with self.subTest(case='unrelated animation'):
            result = analyze(evidence(None))
            self.assertEqual(result['samples'], [])
            self.assertEqual(result['unanswered_input_ids'], [1])
        with self.subTest(case='unpresented pre-input marker invalidates later repaint'):
            stale = receipt(frame=2, lo=9.980, hi=9.981)
            stale[1]['scroll_marker_delta'] = 65
            result = analyze(evidence() + [stale])
            self.assertEqual(result['samples'], [])
            self.assertEqual(result['unanswered_input_ids'], [1])
            self.assertIn('marker_before_input', result['issues'])
        bad = evidence(); bad[1] = input_row(lo=11, hi=11.1)
        backwards = evidence(); backwards[1] = input_row(lo=10, hi=9)
        invalid_scale = evidence(); invalid_scale[0][1]['scale_factor'] = 0
        for label, rows in (
            ('duplicate delta', evidence() + [input_row(2)]),
            ('duplicate input id', evidence() + [input_row(1, dy=-1.65)]),
            ('input after marker', bad),
            ('backwards input clock', backwards),
            ('unsupported scale', invalid_scale),
        ):
            with self.subTest(case=label):
                self.assertEqual(analyze(rows)['samples'], [])
                self.assertTrue(analyze(rows)['issues'])

if __name__ == '__main__': unittest.main()

import unittest
from scroll_presentation import analyze
from test_frame_presentation import receipt, scene, display


def input_row(identifier=1, dy=-1.625, lo=9.990, hi=9.991):
    return ('qos-native-input', dict(id=identifier, input=dict(kind='wheel', dx=0, dy=dy),
        host_clock='mach_absolute_seconds', dispatch_host_lo_s=lo, dispatch_host_hi_s=hi,
        warmup=False))

def evidence(marker=65):
    received = receipt()
    received[1]['scroll_marker_delta'] = marker
    return [('qos-native-window', dict(scale_factor=2)), input_row(), received, scene(), display()]

class ScrollPresentationTests(unittest.TestCase):
    def test_matches_native_line_wheel_delta_to_visible_scroll_acknowledgement(self):
        result = analyze(evidence())
        self.assertEqual(len(result['samples']), 1)
        self.assertEqual(result['samples'][0]['input_id'], 1)
        self.assertAlmostEqual(result['samples'][0]['latency_low_ms'], 26)
        self.assertAlmostEqual(result['samples'][0]['latency_high_ms'], 27)

    def test_unrelated_animation_is_not_a_scroll_response(self):
        result = analyze(evidence(None))
        self.assertEqual(result['samples'], [])
        self.assertEqual(result['unanswered_input_ids'], [1])

    def test_duplicate_deltas_and_input_ids_are_ambiguous(self):
        for extra in [input_row(2), input_row(1, dy=-1.65)]:
            result = analyze(evidence() + [extra])
            self.assertEqual(result['samples'], [])
            self.assertTrue(result['issues'])

    def test_bad_input_clocks_unsupported_scale_and_preinput_markers_are_rejected(self):
        bad = evidence(); bad[1] = input_row(lo=11, hi=11.1)
        backwards = evidence(); backwards[1] = input_row(lo=10, hi=9)
        invalid_scale = evidence(); invalid_scale[0][1]['scale_factor'] = 0
        for rows in [bad, backwards, invalid_scale]:
            self.assertEqual(analyze(rows)['samples'], [])
            self.assertTrue(analyze(rows)['issues'])

    def test_unpresented_preinput_marker_invalidates_later_repaint(self):
        stale = receipt(frame=2, lo=9.980, hi=9.981)
        stale[1]['scroll_marker_delta'] = 65
        result = analyze(evidence() + [stale])
        self.assertEqual(result['samples'], [])
        self.assertEqual(result['unanswered_input_ids'], [1])
        self.assertIn('marker_before_input', result['issues'])

    def test_first_valid_presentation_across_repeated_marker_frames(self):
        rows = evidence()
        second = receipt(frame=2, lo=10.02, hi=10.021)
        second[1]['scroll_marker_delta'] = 65
        rows += [second, scene(cpu=6, ids=[2]), display(cpu=6, time=10.04)]
        self.assertEqual(analyze(rows)['samples'][0]['frame_id'], 1)

if __name__ == '__main__': unittest.main()

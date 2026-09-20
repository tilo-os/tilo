import json
import unittest
import render_boundaries_test
from main_stages import summarize_main_stages, POINTS

class MainStagesTests(unittest.TestCase):
    def fixture(self):
        lines,ids=render_boundaries_test.RenderBoundaryTests().fixture()
        row=dict(cpu_frame_id=7,host_clock='mach_absolute_seconds')
        for i,p in enumerate(POINTS):
            row[p+'_host_lo_s']=99.98+i*.001
            row[p+'_host_hi_s']=99.98+i*.001
        return lines+['qos-native-main-stages '+json.dumps(row)],ids

    def mutate(self,lines,**changes):
        row=json.loads(lines[-1].split(' ',1)[1]);row.update(changes)
        return lines[:-1]+['qos-native-main-stages '+json.dumps(row)]

    def test_reports_each_stage_and_total_without_certifying(self):
        r=summarize_main_stages(*self.fixture())
        self.assertTrue(r['available']);self.assertFalse(r['performance_certified'])
        self.assertAlmostEqual(r['stages']['profiler_commit']['upper']['p50_ms'],1)
        self.assertAlmostEqual(r['stages']['post_draw_to_send']['upper']['p50_ms'],20)
        self.assertEqual([x['cpu_frame_id'] for x in r['frames']],[7])

    def test_old_unavailable_partial_and_duplicate_rejected(self):
        lines,ids=self.fixture()
        self.assertFalse(summarize_main_stages(lines[:-1],ids)['available'])
        for bad in [lines+[lines[-1]],self.mutate(lines,cpu_frame_id=8)]:
            with self.assertRaises(ValueError):summarize_main_stages(bad,ids)

    def test_bad_clock_order_and_after_send_rejected(self):
        lines,ids=self.fixture()
        for changes in [dict(host_clock='other'),dict(before_profiler_commit_host_lo_s=True),dict(after_accessibility_host_hi_s=float('nan')),dict(after_profiler_commit_host_lo_s=99.),dict(after_native_redraw_log_host_lo_s=101.,after_native_redraw_log_host_hi_s=101.)]:
            with self.subTest(changes=changes),self.assertRaises(ValueError):
                summarize_main_stages(self.mutate(lines,**changes),ids)

    def test_missing_positive_feedback_and_send_rejected(self):
        lines,ids=self.fixture()
        for prefix in ('qos-display-frame ','qos-native-send '):
            with self.assertRaises(ValueError):summarize_main_stages([l for l in lines if not l.startswith(prefix)],ids)

    def test_null_auxiliary_validated_without_aliasing(self):
        lines,ids=self.fixture();extra=self.mutate(lines,cpu_frame_id=None)[-1]
        self.assertEqual(len(summarize_main_stages(lines+[extra],ids)['frames']),1)
        bad=self.mutate(lines,cpu_frame_id=None,after_hud_request_host_hi_s=None)[-1]
        with self.assertRaises(ValueError):summarize_main_stages(lines+[bad],ids)

if __name__=='__main__':unittest.main()

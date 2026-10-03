import json
import unittest
from render_boundaries import summarize_render_boundaries

class RenderBoundaryTests(unittest.TestCase):
    def fixture(self):
        identity=dict(cpu_frame_id=7,worker_id=1,worker_frame_ix=9)
        clock=dict(host_clock='mach_absolute_seconds')
        values=[('tilo-native-send',dict(clock,cpu_frame_id=7,send_ok=True,before_host_lo_s=100.,before_host_hi_s=100.,after_host_lo_s=100.003,after_host_hi_s=100.003)),
                ('tilo-native-worker',dict(clock,**identity,receive_host_lo_s=100.001,receive_host_hi_s=100.001,render_start_host_lo_s=100.002,render_start_host_hi_s=100.002,render_end_host_lo_s=100.005,render_end_host_hi_s=100.005,outcome='Presented')),
                ('tilo-native-present-call',dict(clock,**identity,before_host_lo_s=100.004,before_host_hi_s=100.004,after_host_lo_s=100.0041,after_host_hi_s=100.0041)),
                ('tilo-display-request',identity),
                ('tilo-display-frame',dict(identity,presented_time_s=100.024,backend='metal',drawable_id=42))]
        return [prefix+' '+json.dumps(value) for prefix,value in values], [7]

    def change(self,lines,index,**values):
        lines=list(lines);prefix,raw=lines[index].split(' ',1);row=json.loads(raw);row.update(values);lines[index]=prefix+' '+json.dumps(row);return lines

    def test_valid_boundaries_are_reported_and_auxiliary_rows_never_alias_presentation(self):
        with self.subTest(phase='worker receive during blocking send is valid'):
            report=summarize_render_boundaries(*self.fixture())
            self.assertIsInstance(report,dict)
            self.assertTrue(report['available'])
            self.assertFalse(report['performance_certified'])
            stages=report['stages']
            for stage,want in [('send_call',3),('send_start_to_receive',1),('worker_render_cpu',3),('present_call_cpu',.1),('present_return_to_display',19.9)]:
                self.assertAlmostEqual(stages[stage]['upper']['p50_ms'],want,places=7)
        with self.subTest(phase='display may precede cpu present return'):
            lines,ids=self.fixture()
            lines=self.change(lines,2,after_host_lo_s=100.028,after_host_hi_s=100.028)
            lines=self.change(lines,1,render_end_host_lo_s=100.029,render_end_host_hi_s=100.029)
            report=summarize_render_boundaries(lines,ids)
            self.assertIsInstance(report,dict)
            self.assertAlmostEqual(report['stages']['present_return_to_display']['upper']['p50_ms'],-4,places=7)
        with self.subTest(phase='auxiliary occlusion diagnostic does not count as presentation'):
            lines,ids=self.fixture()
            row=json.loads(lines[1].split(' ',1)[1])
            row.update(cpu_frame_id=None,worker_frame_ix=10,outcome='Occluded')
            report=summarize_render_boundaries(lines+['tilo-native-worker '+json.dumps(row)],ids)
            self.assertEqual(len(report['frames']),1)
            with self.assertRaises(ValueError):
                summarize_render_boundaries(self.change(lines,1,outcome='Occluded'),ids)
        with self.subTest(phase='unprofiled worker rows validated but not matched to cpu zero'):
            lines,ids=self.fixture()
            extra=[]
            for index in [1,2]:
                row=json.loads(lines[index].split(' ',1)[1]);row.update(cpu_frame_id=None,worker_frame_ix=10)
                extra.append(lines[index].split(' ',1)[0]+' '+json.dumps(row))
            report=summarize_render_boundaries(lines+extra,ids)
            self.assertEqual([r['cpu_frame_id'] for r in report['frames']],[7])
            with self.assertRaises(ValueError):summarize_render_boundaries(lines+extra+[extra[0]],ids)
            bad=list(extra);row=json.loads(bad[0].split(' ',1)[1]);row['receive_host_lo_s']=float('nan');bad[0]='tilo-native-worker '+json.dumps(row)
            with self.assertRaises(ValueError):summarize_render_boundaries(lines+bad,ids)
        with self.subTest(phase='unprofiled surface gone before render has no render brackets'):
            lines,ids=self.fixture()
            row=json.loads(lines[1].split(' ',1)[1]);row.update(cpu_frame_id=None,worker_frame_ix=10,outcome='SurfaceGone')
            for field in ['render_start_host_lo_s','render_start_host_hi_s','render_end_host_lo_s','render_end_host_hi_s']:row[field]=None
            extra='tilo-native-worker '+json.dumps(row)
            report=summarize_render_boundaries(lines+[extra],ids)
            self.assertEqual(len(report['frames']),1)
            row['outcome']='Presented'
            with self.assertRaises(ValueError):summarize_render_boundaries(lines+['tilo-native-worker '+json.dumps(row)],ids)

    def test_partial_duplicate_or_invalid_boundaries_are_rejected(self):
        with self.subTest(phase='old evidence unavailable but partial boundaries rejected'):
            lines,ids=self.fixture()
            report=summarize_render_boundaries(lines[3:],ids)
            self.assertIsInstance(report,dict)
            self.assertFalse(report['available'])
            for index in [0,1,2]:
                with self.assertRaises(ValueError):summarize_render_boundaries(lines[:index]+lines[index+1:],ids)
        with self.subTest(phase='duplicate identity, wrong worker or incomplete feedback'):
            lines,ids=self.fixture()
            variants=[lines+[lines[0]],lines+[lines[1]],lines+[lines[2]],lines[:-1],
                      self.change(lines,1,worker_id=2),self.change(lines,2,worker_frame_ix=10),
                      self.change(lines,4,presented_time_s=0),self.change(lines,0,send_ok=False)]
            for variant in variants:
                with self.assertRaises(ValueError):summarize_render_boundaries(variant,ids)
        with self.subTest(phase='invalid or reversed clock samples'):
            lines,ids=self.fixture()
            for index,changes in [(0,dict(before_host_lo_s=True)),(0,dict(before_host_lo_s=float('nan'))),
                                  (0,dict(before_host_hi_s=99)),(1,dict(receive_host_lo_s=99)),
                                  (1,dict(render_start_host_hi_s=100.006)),
                                  (2,dict(before_host_hi_s=float('inf'))),(1,dict(host_clock='other'))]:
                with self.subTest(changes=changes),self.assertRaises(ValueError):
                    summarize_render_boundaries(self.change(lines,index,**changes),ids)


if __name__=='__main__':unittest.main()

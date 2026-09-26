import copy
import json
import unittest
from worker_handoff import summarize_worker_handoffs

PREVIOUS = ['render_end', 'after_native_worker_log', 'after_outcome_log',
            'after_scene_recycle', 'after_pool_send', 'after_outcome_send',
            'after_completion_notify', 'handoff_log_before', 'handoff_log_after']

def point(row, name, value):
    for end in ('lo', 'hi'): row[name + '_host_' + end + '_s'] = value

def fixture():
    result=[]
    for ix in (0,1):
        base=100. + ix
        ident=dict(worker_id=1,worker_frame_ix=ix,cpu_frame_id=7+ix,host_clock='mach_absolute_seconds')
        worker=dict(ident,outcome='Presented')
        for name,t in [('receive',base+.1),('render_start',base+.2),('render_end',base+.3)]: point(worker,name,t)
        row=dict(ident,previous_worker_frame_ix=ix-1 if ix else None,previous_cpu_frame_id=7 if ix else None)
        point(row,'before_receive',base)
        point(row,'after_receive',base+.1)
        for offset,name in enumerate(PREVIOUS):
            t=99.3+offset*.01 if ix else None
            if ix and name=='handoff_log_before':t=100.11
            if ix and name=='handoff_log_after':t=100.12
            if ix and name not in ('handoff_log_before','handoff_log_after'):t=100.3+offset*.01
            point(row,'previous_'+name,t)
        result.extend([('tilo-native-worker',worker),('tilo-native-worker-handoff',row)])
    return result

def encode(rows):return [p+' '+json.dumps(r) for p,r in rows]

class WorkerHandoffTests(unittest.TestCase):
    def test_handoffs_are_summarized_per_frame_without_aliasing_auxiliary_workers(self):
        with self.subTest(phase='first and last frame need no future frame'):
            r=summarize_worker_handoffs(encode(fixture()),[7,8])
            self.assertTrue(r['available'])
            self.assertFalse(r['performance_certified'])
            self.assertEqual(len(r['frames']),2)
            self.assertNotIn('previous_scene_recycle',r['frames'][0]['intervals_ms'])
            self.assertAlmostEqual(r['frames'][1]['intervals_ms']['previous_scene_recycle'][1],10,places=6)
            self.assertAlmostEqual(r['frames'][1]['intervals_ms']['receive_call'][1],100,places=6)
        with self.subTest(phase='explicit null auxiliary identity not cpu zero'):
            rows=fixture();aux=copy.deepcopy(rows[:2])
            for _,r in aux:r.update(worker_id=2,cpu_frame_id=None)
            r=summarize_worker_handoffs(encode(rows+aux),[7,8])
            self.assertEqual([f['cpu_frame_id'] for f in r['frames']],[7,8])
            del aux[1][1]['cpu_frame_id']
            with self.assertRaises(ValueError):summarize_worker_handoffs(encode(rows+aux),[7,8])

    def test_partial_duplicate_or_invalid_handoffs_are_rejected(self):
        with self.subTest(phase='missing legacy optional, partial and duplicate rejected'):
            self.assertFalse(summarize_worker_handoffs([], [7])['available'])
            rows=fixture()
            for bad in [rows[:2],rows+[rows[3]],rows[:3]]:
                with self.assertRaises(ValueError):summarize_worker_handoffs(encode(bad),[7,8])
        with self.subTest(phase='invalid clock, identity and predecessor'):
            changes=[{'previous_worker_frame_ix':4},{'previous_cpu_frame_id':8},
                     {'worker_id':True},{'cpu_frame_id':None},
                     {'before_receive_host_lo_s':float('nan')},
                     {'before_receive_host_hi_s':102},
                     {'previous_after_pool_send_host_lo_s':99},
                     {'previous_render_end_host_hi_s':100.31},
                     {'previous_handoff_log_after_host_hi_s':100.25}]
            for change in changes:
                rows=copy.deepcopy(fixture());rows[3][1].update(change)
                with self.subTest(change=change),self.assertRaises(ValueError):
                    summarize_worker_handoffs(encode(rows),[7,8])


if __name__=='__main__':unittest.main()

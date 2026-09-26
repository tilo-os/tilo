import json
import unittest
from presentation_feedback import summarize_presentation

class PresentationTests(unittest.TestCase):
    def fixture(self):
        lines=[]
        for i in range(3):
            identity={'worker_id':1,'worker_frame_ix':i,'cpu_frame_id':i}
            lines.append('tilo-display-request '+json.dumps(identity))
            lines.append('tilo-display-frame '+json.dumps(dict(identity,presented_time_s=100+i/60,backend="metal",drawable_id=i)))
        return lines,[0,1,2]

    def test_display_timestamps_drive_completeness_and_intervals(self):
        with self.subTest(phase='display timestamps not callback arrival order'):
            lines,ids=self.fixture()
            report=summarize_presentation(list(reversed(lines)),ids)
            self.assertEqual(report['positive_presentations'],3)
            self.assertAlmostEqual(report['presentation_intervals']['p99_ms'],1000/60)
            self.assertTrue(report['feedback_complete'])
            self.assertFalse(report['performance_certified'])
        with self.subTest(phase='zero and missing callbacks remain distinct'):
            lines,ids=self.fixture()
            lines[1]=lines[1].replace('100.0','0.0')
            del lines[-1]
            report=summarize_presentation(lines,ids)
            self.assertEqual(report['zero_timestamp_cpu_ids'],[0])
            self.assertEqual(report['missing_callback_cpu_ids'],[2])
            self.assertFalse(report['feedback_complete'])
            self.assertIsNone(report['presentation_intervals'])

    def test_missing_duplicate_or_invalid_feedback_and_identity_is_rejected(self):
        with self.subTest(phase='missing requests, duplicate or invalid feedback'):
            lines,ids=self.fixture()
            for changed in [lines[1:], lines+[lines[1]], lines+[lines[0]],
                            [s.replace('100.0','NaN') for s in lines],
                            [s.replace('100.0','-1.0') for s in lines]]:
                with self.assertRaises(ValueError):
                    summarize_presentation(changed,ids)
        with self.subTest(phase='provenance and drawable identity'):
            lines,ids=self.fixture()
            for key,value in [('backend','other'),('backend',None),('drawable_id',True),('drawable_id',-1)]:
                changed=list(lines)
                row=json.loads(changed[1].split(' ',1)[1]);row[key]=value
                changed[1]='tilo-display-frame '+json.dumps(row)
                with self.assertRaises(ValueError):summarize_presentation(changed,ids)
            changed=list(lines)
            row=json.loads(changed[3].split(' ',1)[1]);row['drawable_id']=0
            changed[3]='tilo-display-frame '+json.dumps(row)
            with self.assertRaises(ValueError):summarize_presentation(changed,ids)


if __name__=='__main__':unittest.main()

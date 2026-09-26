import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

from fixture_host_tools import fixture_path

spec=importlib.util.spec_from_file_location('scratchpad_collector',Path(__file__).with_name('collect-scratchpad-native.py'))
collector=importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)

class CollectorFailureTests(unittest.TestCase):
    def run_failure(self, app_source):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            binary=root/'fake-app'
            binary.write_text('#!/usr/bin/env python3\n'+app_source)
            binary.chmod(0o755)
            tool=root/'fake-fixture'
            tool.write_text('#!/usr/bin/env python3\nimport json\nprint(json.dumps(dict(lines=1000,body_bytes=66000)))\n')
            tool.chmod(0o755)
            scenario=root/'scenario.json'
            scenario.write_text(json.dumps(dict(name='early exit test',interval_us=16667,
                                               warmup_steps=0,steps=[dict(kind='key',key='home')])))
            output=root/'runs'
            argv=['collector','--binary',str(binary),'--fixture-tool',str(tool),
                  '--scenario',str(scenario),'--mode','typing','--output-root',str(output)]
            with patch.object(sys,'argv',argv),patch.dict(os.environ,PATH=fixture_path(root)),\
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(collector.main(),1)
            run=next(output.iterdir())
            failure=json.loads((run/'failed-report.json').read_text())
            self.assertFalse(failure['performance_certified'])
            metadata=json.loads((run/'metadata.json').read_text())
            self.assertTrue((run/'app.log').is_file())
            return failure,metadata

    def test_failed_capture_retains_failure_report_and_executable_identity(self):
        with self.subTest(case='early process exit'):
            failure,metadata=self.run_failure('raise SystemExit(7)\n')
            self.assertIn('exited early: 7',failure['capture_error'])
            self.assertEqual(metadata['exit_code'],7)
            self.assertEqual(metadata['hashes_before'],metadata['hashes_after'])
        with self.subTest(case='removed executable preserves initial hash'):
            failure,metadata=self.run_failure('from pathlib import Path\nPath(__file__).unlink()\nraise SystemExit(7)\n')
            self.assertTrue(metadata['hashes_before'])
            self.assertTrue(metadata['final_hash_errors'])
            self.assertEqual(metadata['exit_code'],7)
            self.assertIn('exited early',failure['capture_error'])
        with self.subTest(case='malformed evidence keeps failure and stops child'):
            source='''import time
print('tilo-native-effect {}',flush=True)
print('tilo-native-frame {"cpu_frame_id": 1}',flush=True)
time.sleep(30)
'''
            failure,metadata=self.run_failure(source)
            self.assertIn('input_ids',failure['capture_error'])
            self.assertLess(metadata['exit_code'],0)

    def test_native_window_metadata_requires_real_scale_and_size(self):
        valid=dict(scale_factor=2.0,physical_width=1600,physical_height=1200)
        lines=lambda row:['tilo-native-window '+json.dumps(row)]
        self.assertEqual(collector.window_metadata(lines(valid)),valid)
        for key,value in [('scale_factor',0),('scale_factor',float('nan')),
                          ('physical_width',True),('physical_height',0)]:
            with self.subTest(key=key),self.assertRaises(ValueError):
                collector.window_metadata(lines(dict(valid,**{key:value})))
        with self.subTest(case='no window record'),self.assertRaises(ValueError):
            collector.window_metadata([])


if __name__ == '__main__':
    unittest.main()

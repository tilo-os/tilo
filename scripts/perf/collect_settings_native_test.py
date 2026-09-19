"""Exercise the Settings production launch route without opening a native app."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import native_app_capture

class SettingsCollectorTests(unittest.TestCase):
    def test_normal_launch_isolated_roots_no_fixture_or_screenshot_flags(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);app=root/'app'
            app.write_text('''#!/usr/bin/env python3
import os,sys,json
from pathlib import Path
assert len(sys.argv)==1
assert os.environ['QUICK_APP_ID']=='dev.qos.settings'
run=Path(os.environ['QOS_NATIVE_SCENARIO']).parent
for key,name in [('XDG_DATA_HOME','xdg'),('XDG_CONFIG_HOME','config'),('XDG_CACHE_HOME','cache')]:
    assert Path(os.environ[key])==run/name
assert not Path(os.environ['XDG_CONFIG_HOME']).exists()
scenario=json.loads(Path(os.environ['QOS_NATIVE_SCENARIO']).read_text())
assert len(scenario['steps'])-scenario['warmup_steps']>=1000
assert all(s in [{'kind':'character','value':'z'},{'kind':'key','key':'backspace'}] for s in scenario['steps'][scenario['warmup_steps']:])
print('normal Settings route',flush=True)
raise SystemExit(7)
''');app.chmod(0o755)
            result=subprocess.run([sys.executable,str(Path(__file__).with_name('collect-settings-native.py')),
                '--binary',str(app),'--output-root',str(root/'runs')],capture_output=True,text=True)
            self.assertEqual(result.returncode,1,result.stderr)
            run=next((root/'runs').iterdir())
            self.assertIn('normal Settings route',(run/'app.log').read_text())
            failed=json.loads((run/'failed-report.json').read_text())
            self.assertIn('exited early: 7',failed['capture_error'])
            self.assertFalse(failed['performance_certified'])
            meta=json.loads((run/'metadata.json').read_text())
            self.assertEqual(meta['hashes_before'],meta['hashes_after'])
            self.assertEqual(list(meta['hashes_before']),[str(app.resolve())])

    def test_unsupported_scroll_mode_does_not_launch_settings(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);app=root/'app';marker=root/'launched'
            app.write_text('#!/usr/bin/env python3\nfrom pathlib import Path\nPath('+repr(str(marker))+').touch()\n')
            app.chmod(0o755)
            result=subprocess.run([sys.executable,str(Path(__file__).with_name('collect-settings-native.py')),
                '--binary',str(app),'--mode','scroll','--output-root',str(root/'runs')],capture_output=True,text=True)
            self.assertEqual(result.returncode,1,result.stderr)
            self.assertFalse(marker.exists())
            run=next((root/'runs').iterdir())
            self.assertIn('requires typing mode',json.loads((run/'failed-report.json').read_text())['capture_error'])

    def test_default_scenario_is_alternating_and_large_enough(self):
        scenario=native_app_capture.expand_scenario(
            json.loads((Path(__file__).parent/'scenarios/settings-search.json').read_text())
        )
        self.assertEqual(scenario['interval_us'],16667)
        self.assertEqual(scenario['warmup_steps'],61)
        self.assertEqual(len(scenario['steps']),1261)
        self.assertEqual(scenario['steps'][0],dict(kind='click',x=80.0,y=48.0))
        for i,step in enumerate(scenario['steps'][1:]):
            self.assertEqual(step,dict(kind='character',value='z') if i%2==0 else dict(kind='key',key='backspace'))

if __name__=='__main__':unittest.main()

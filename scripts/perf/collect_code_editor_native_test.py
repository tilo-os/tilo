"""Exercise real collector subprocess setup without opening a native window."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from fixture_host_tools import PROCESS_LISTING, fixture_path


class CodeEditorCollectorTests(unittest.TestCase):
    def test_production_path_and_isolated_fixture_are_used(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / 'seed'
            fixture.write_text('#!/usr/bin/env python3\nimport sys,json\nfrom pathlib import Path\np=Path(sys.argv[1]);p.write_text("fixture source")\nprint(json.dumps(dict(lines=2000,body_bytes=66000)))\n')
            fixture.chmod(0o755)
            app = root / 'app'
            app.write_text('#!/usr/bin/env python3\nimport sys,os,json\nfrom pathlib import Path\nassert Path(sys.argv[1]).read_text()=="fixture source"\nassert os.environ["QUICK_APP_ID"]=="dev.tilo.code-editor"\nassert Path(os.environ["XDG_CONFIG_HOME"])==Path(sys.argv[1]).parent/"config"\nassert Path(os.environ["XDG_CACHE_HOME"])==Path(sys.argv[1]).parent/"cache"\nprint("production fixture opened",flush=True)\nraise SystemExit(7)\n')
            app.chmod(0o755)
            scenario = root / 'scenario.json'
            scenario.write_text(json.dumps(dict(name='fixture route',interval_us=16667,
                warmup_steps=0,steps=[dict(kind='key',key='home')])))
            output = root / 'runs'
            result = subprocess.run([sys.executable,str(Path(__file__).with_name('collect-code-editor-native.py')),
                '--binary',str(app),'--fixture-tool',str(fixture),'--scenario',str(scenario),
                '--mode','typing','--output-root',str(output)],capture_output=True,text=True,
                env=dict(os.environ,PATH=fixture_path(root)))
            self.assertEqual(result.returncode,1,result.stderr)
            run = next(output.iterdir())
            self.assertIn('production fixture opened',(run/'app.log').read_text())
            self.assertEqual((run/'host-processes.txt').read_text(),PROCESS_LISTING)
            report = json.loads((run/'failed-report.json').read_text())
            self.assertIn('exited early: 7',report['capture_error'])
            self.assertFalse(report['performance_certified'])
            metadata = json.loads((run/'metadata.json').read_text())
            self.assertEqual(metadata['hashes_before'],metadata['hashes_after'])


if __name__ == '__main__':
    unittest.main()

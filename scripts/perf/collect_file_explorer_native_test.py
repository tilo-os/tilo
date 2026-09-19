"""The live Files route takes a directory positional, not screenshot --path."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class FileExplorerCollectorTests(unittest.TestCase):
    def test_real_directory_positional_and_isolated_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seed = root / 'seed'
            seed.write_text('#!/usr/bin/env python3\nimport sys,json\nfrom pathlib import Path\np=Path(sys.argv[1]);p.mkdir();(p/"fixture.txt").write_text("seed")\nprint(json.dumps(dict(schema=1,entry_count=1200,image_count=299,hash_algorithm="sha256",order_hash="a"*64)))\n')
            seed.chmod(0o755)
            app = root / 'app'
            app.write_text('#!/usr/bin/env python3\nimport sys,os\nfrom pathlib import Path\nassert len(sys.argv)==2\np=Path(sys.argv[1]);assert p.is_absolute() and p.is_dir()\nassert (p/"fixture.txt").read_text()=="seed"\nassert os.environ["QUICK_APP_ID"]=="dev.qos.file-explorer"\nassert Path(os.environ["XDG_CONFIG_HOME"])==p.parent/"config"\nassert Path(os.environ["XDG_CACHE_HOME"])==p.parent/"cache"\nprint("production directory opened",flush=True)\nraise SystemExit(7)\n')
            app.chmod(0o755)
            scenario = root / 'scenario.json'
            scenario.write_text(json.dumps(dict(name='Files route',interval_us=16667,warmup_steps=0,steps=[dict(kind='wheel',dx=0,dy=-3)])))
            out = root / 'runs'
            result = subprocess.run([sys.executable,str(Path(__file__).with_name('collect-file-explorer-native.py')),'--binary',str(app),'--fixture-tool',str(seed),'--scenario',str(scenario),'--mode','scroll','--output-root',str(out)],capture_output=True,text=True)
            self.assertEqual(result.returncode,1,result.stderr)
            run = next(out.iterdir())
            self.assertIn('production directory opened',(run/'app.log').read_text())
            report=json.loads((run/'failed-report.json').read_text())
            self.assertIn('exited early: 7',report['capture_error'])
            self.assertFalse(report['performance_certified'])
            metadata=json.loads((run/'metadata.json').read_text())
            self.assertEqual(metadata['hashes_before'],metadata['hashes_after'])

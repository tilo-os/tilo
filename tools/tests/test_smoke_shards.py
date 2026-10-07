"""Exercise browser/shard orchestration with process fixtures, without a browser."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "web" / "run_smoke_shards.sh"


class SmokeShards(unittest.TestCase):
    def run_driver(self, apps, shards="2", failing_app=""):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            browser = root / "chrome"
            browser.write_text(
                "#!/usr/bin/env python3\n"
                "import json,os,pathlib,signal,sys,time\n"
                "pathlib.Path(os.environ['FIXTURE_ROOT'],f'chrome-{os.getpid()}.json')"
                ".write_text(json.dumps(sys.argv[1:]))\n"
                "profile=pathlib.Path(next(a.split('=',1)[1] for a in sys.argv[1:] if a.startswith('--user-data-dir=')))\n"
                "profile.mkdir(parents=True)\n"
                "def stop(*args):\n"
                " time.sleep(.05)\n"
                " if not profile.exists():pathlib.Path(os.environ['FIXTURE_ROOT'],'cleanup-race').touch()\n"
                " else:(profile/'shutdown').write_text('flushed')\n"
                " sys.exit(0)\n"
                "signal.signal(signal.SIGTERM,stop)\n"
                "while True:time.sleep(.01)\n"
            )
            curl = root / "curl"
            curl.write_text(
                "#!/usr/bin/env python3\n"
                "import json,os,pathlib,re,sys,time\n"
                "port=re.search(r':(\\d+)/',sys.argv[-1]).group(1)\n"
                "for _ in range(100):\n"
                " for p in pathlib.Path(os.environ['FIXTURE_ROOT']).glob('chrome-*.json'):\n"
                "  if f'--remote-debugging-port={port}' in json.loads(p.read_text()):sys.exit(0)\n"
                " time.sleep(.005)\n"
                "sys.exit(1)\n"
            )
            node = root / "node"
            node.write_text(
                "#!/usr/bin/env python3\n"
                "import json,os,pathlib,sys,time\n"
                "time.sleep(.05)\n"
                "pathlib.Path(os.environ['FIXTURE_ROOT'],"
                "f'worker-{os.environ[\"CDP_PORT\"]}.json').write_text("
                "json.dumps({'args':sys.argv[1:],'port':os.environ['CDP_PORT']}))\n"
                "sys.exit(7 if os.environ['FAILING_APP'] in sys.argv[3:] else 0)\n"
            )
            for executable in (browser, curl, node):
                executable.chmod(0o755)
            environment = dict(
                os.environ,
                CHROME=str(browser),
                RUNNER_TEMP=directory,
                FIXTURE_ROOT=directory,
                FAILING_APP=failing_app,
                TILO_WEB_SMOKE_SHARDS=shards,
                CDP_PORT="9333",
                PATH=f"{directory}:{os.environ['PATH']}",
            )
            result = subprocess.run(
                ["bash", str(SCRIPT), "http://fixture", *apps],
                env=environment,
                capture_output=True,
                text=True,
                timeout=5,
            )
            workers = [json.loads(p.read_text()) for p in root.glob("worker-*.json")]
            browsers = [json.loads(p.read_text()) for p in root.glob("chrome-*.json")]
            self.assertFalse((root / "cleanup-race").exists(), "profile removed before browser exit")
            self.assertEqual(list(root.glob("smoke-browsers.*")), [])
            return result, workers, browsers

    def test_every_probe_runs_once_in_separate_browser_profiles(self):
        apps = ["calculator", "calendar", "file-explorer", "desktop", "terminal"]
        result, workers, browsers = self.run_driver(apps)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(workers), 2)
        self.assertCountEqual([app for w in workers for app in w["args"][2:]], apps)
        self.assertEqual({w["port"] for w in workers}, {"9333", "9334"})
        self.assertEqual(len(browsers), 2)
        profiles = [next(a for a in args if a.startswith("--user-data-dir=")) for args in browsers]
        self.assertEqual(len(set(profiles)), 2)
        for worker in workers:
            self.assertEqual(worker["args"][0], "crates/browser/tilo-web-desktop/scripts/smoke.mjs")
            self.assertEqual(worker["args"][1], "http://fixture")

    def test_failure_is_propagated_and_other_shard_finishes(self):
        result, workers, _ = self.run_driver(["calculator", "calendar", "desktop"], failing_app="calculator")
        self.assertEqual(result.returncode, 1)
        self.assertCountEqual([app for w in workers for app in w["args"][2:]], ["calculator", "calendar", "desktop"])

    def test_single_app_starts_only_one_browser(self):
        result, workers, browsers = self.run_driver(["calculator"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(workers), 1)
        self.assertEqual(len(browsers), 1)

    def test_single_shard_retains_sequential_driver_option(self):
        result, workers, browsers = self.run_driver(["calculator", "desktop"], shards="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(browsers), 1)
        self.assertEqual(workers[0]["args"][2:], ["calculator", "desktop"])

    def test_extra_parallelism_is_rejected(self):
        result, workers, browsers = self.run_driver(["calculator", "desktop"], shards="3")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(workers, [])
        self.assertEqual(browsers, [])


if __name__ == "__main__":
    unittest.main()

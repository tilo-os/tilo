"""Exercise browser/shard orchestration with process fixtures, without a browser."""
import json
import os
from pathlib import Path
import subprocess
import shutil
import signal
import time
import tempfile
import unittest

from hang import HANG_BOUND


SCRIPT = Path(__file__).resolve().parents[1] / "web" / "run_smoke_shards.sh"
ENTRY = Path(__file__).resolve().parents[2] / "crates/browser/tilo-web-desktop/scripts/smoke.mjs"


class SmokeShards(unittest.TestCase):
    def run_driver(self, apps, shards="2", failing_app="", orphan=False, cleanup_failures=0):
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
                "if os.environ['ORPHAN_WRITER']=='1':\n"
                " child=os.fork()\n"
                " if child:signal.signal(signal.SIGTERM,lambda *args:sys.exit(0))\n"
                # Runs until the driver stops it; the bound only ends a forgotten fixture.
                "deadline=time.monotonic()+float(os.environ['FIXTURE_HANG_BOUND'])\n"
                "while time.monotonic()<deadline:time.sleep(.01)\n"
                "stop()\n"
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
                "json.dumps({'args':sys.argv[1:],'port':os.environ['CDP_PORT'],'worker':os.environ.get('TILO_WEB_SMOKE_WORKER')}))\n"
                "sys.exit(7 if os.environ['FAILING_APP'] in sys.argv[3:] else 0)\n"
            )
            remove = root / "rm"
            remove.write_text(
                "#!/usr/bin/env python3\n"
                "import os,pathlib,sys\n"
                "attempts=pathlib.Path(os.environ['FIXTURE_ROOT'],'removal-attempts')\n"
                "count=int(attempts.read_text())+1 if attempts.exists() else 1\n"
                "attempts.write_text(str(count))\n"
                "if count<=int(os.environ['CLEANUP_FAILURES']):\n"
                " print('rm: Directory not empty',file=sys.stderr); sys.exit(1)\n"
                f"os.execv({shutil.which('rm')!r},['rm',*sys.argv[1:]])\n"
            )
            for executable in (browser, curl, node, remove):
                executable.chmod(0o755)
            environment = dict(
                os.environ,
                CHROME=str(browser),
                RUNNER_TEMP=directory,
                FIXTURE_ROOT=directory,
                FAILING_APP=failing_app,
                ORPHAN_WRITER="1" if orphan else "0",
                CLEANUP_FAILURES=str(cleanup_failures),
                TILO_WEB_SMOKE_SHARDS=shards,
                CDP_PORT="9333",
                FIXTURE_HANG_BOUND=str(HANG_BOUND),
                PATH=f"{directory}:{os.environ['PATH']}",
            )
            # Its own session, so a hung driver is stopped with every browser,
            # supervisor and worker it started before the directory goes.
            process = subprocess.Popen(
                ["bash", str(SCRIPT), "http://fixture", *apps],
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            try:
                stdout, stderr = process.communicate(timeout=HANG_BOUND)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.communicate()
            result = subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)
            workers = [json.loads(p.read_text()) for p in root.glob("worker-*.json")]
            browsers = [json.loads(p.read_text()) for p in root.glob("chrome-*.json")]
            self.assertFalse((root / "cleanup-race").exists(), "profile removed before browser exit")
            if cleanup_failures < 100:
                self.assertEqual(list(root.glob("smoke-browsers.*")), [], result.stderr)
            else:
                self.assertTrue(list(root.glob("smoke-browsers.*")), "permanent removal failure must remain observable")
            return result, workers, browsers

    def run_entrypoint(self, *, ci="true", worker="", code=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            driver = root / "tools/web/run_smoke_shards.sh"
            driver.parent.mkdir(parents=True)
            driver.write_text(
                'printf "%s\\n" "$CDP_PORT" "$@" >"$RUNNER_TEMP/entry"\n'
                f"exit {code}\n"
            )
            node = shutil.which("node")
            self.assertIsNotNone(node, "the CI policy image provides Node")
            environment = dict(os.environ, CI=ci, CHROME="fixture", RUNNER_TEMP=directory,
                               TILO_WEB_SMOKE_WORKER=worker, CDP_PORT="0")
            result = subprocess.run([node, str(ENTRY), "http://fixture", "calculator", "desktop"],
                                    cwd=root, env=environment, capture_output=True, text=True,
                                    timeout=HANG_BOUND)
            entry = (root / "entry").read_text().splitlines() if (root / "entry").exists() else None
            return result, entry

    def test_legacy_ci_entrypoint_starts_driver_outside_existing_browser_port(self):
        result, entry = self.run_entrypoint()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(entry, ["1", "http://fixture", "calculator", "desktop"])

    def test_legacy_ci_entrypoint_propagates_driver_failure(self):
        result, _ = self.run_entrypoint(code=7)
        self.assertEqual(result.returncode, 7)

    def test_manual_and_worker_entrypoints_do_not_start_another_driver(self):
        for options in ({"ci": "false"}, {"worker": "1"}):
            with self.subTest(options=options):
                result, entry = self.run_entrypoint(**options)
                self.assertEqual(result.returncode, 1)
                self.assertIsNone(entry)

    def test_entrypoint_forwards_termination_to_its_driver(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            driver = root / "tools/web/run_smoke_shards.sh"
            driver.parent.mkdir(parents=True)
            driver.write_text(
                "trap 'echo terminated >\"$RUNNER_TEMP/stopped\"; exit 143' TERM\n"
                'echo $$ >"$RUNNER_TEMP/ready"\n'
                'while :; do sleep .02; done\n'
            )
            # The child can become ready before spawn returns to the parent.
            # Deliver the real signal in that window, rather than depending
            # on scheduler load to expose missing signal watchers.
            preload = root / "signal-during-spawn.mjs"
            preload.write_text(
                "import childProcess from 'node:child_process';\n"
                "import {syncBuiltinESMExports} from 'node:module';\n"
                "import {existsSync} from 'node:fs';\n"
                "const spawn=childProcess.spawn;\n"
                "childProcess.spawn=(...args)=>{\n"
                " const child=spawn(...args);\n"
                " const deadline=performance.now()+Number(process.env.FIXTURE_HANG_BOUND)*1000;\n"
                " while(!existsSync(`${process.env.RUNNER_TEMP}/ready`)){\n"
                "  if(performance.now()>=deadline)throw Error('driver never started');\n"
                " }\n"
                " process.kill(process.pid,'SIGTERM');\n"
                " return child;\n"
                "};\n"
                "syncBuiltinESMExports();\n"
            )
            environment = dict(os.environ, CI="true", CHROME="fixture", RUNNER_TEMP=directory,
                               TILO_WEB_SMOKE_WORKER="", CDP_PORT="9333",
                               FIXTURE_HANG_BOUND=str(HANG_BOUND))
            environment["NODE_OPTIONS"] = (
                f"{environment.get('NODE_OPTIONS', '')} --import {preload.as_uri()}"
            ).strip()
            process = subprocess.Popen([shutil.which("node"), str(ENTRY), "http://fixture", "desktop"],
                                       cwd=root, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                deadline = time.monotonic() + HANG_BOUND
                while not (root / "ready").exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                self.assertTrue((root / "ready").exists(), "driver never started")
                process.terminate()
                process.communicate(timeout=HANG_BOUND)
                self.assertEqual(process.returncode, 143)
                self.assertTrue((root / "stopped").exists())
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                if (root / "ready").exists():
                    try:
                        os.kill(int((root / "ready").read_text()), signal.SIGTERM)
                    except ProcessLookupError:
                        pass

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
            self.assertEqual(worker["worker"], "1")

    def test_three_shards_preserve_round_robin_coverage_and_profile_isolation(self):
        apps = ["calculator", "calendar", "file-explorer", "desktop", "terminal", "settings", "chat"]
        result, workers, browsers = self.run_driver(apps, shards="3")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(workers), 3)
        self.assertEqual(len(browsers), 3)
        assignments = {worker["port"]: worker["args"][2:] for worker in workers}
        self.assertEqual(assignments, {str(9333 + shard): apps[shard::3] for shard in range(3)})
        profiles = [next(arg for arg in args if arg.startswith("--user-data-dir=")) for args in browsers]
        self.assertEqual(len(set(profiles)), 3)

    def test_three_shards_finish_all_probes_and_preserve_failure_through_cleanup(self):
        apps = ["calculator", "calendar", "desktop", "terminal"]
        result, workers, _ = self.run_driver(apps, shards="3", failing_app="calendar", cleanup_failures=1)
        self.assertEqual(result.returncode, 1)
        self.assertCountEqual([app for worker in workers for app in worker["args"][2:]], apps)

    def test_three_requested_shards_do_not_start_empty_browsers(self):
        result, workers, browsers = self.run_driver(["calculator", "desktop"], shards="3")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(workers), 2)
        self.assertEqual(len(browsers), 2)

    def test_failure_is_propagated_and_other_shard_finishes(self):
        result, workers, _ = self.run_driver(["calculator", "calendar", "desktop"], failing_app="calculator")
        self.assertEqual(result.returncode, 1)
        self.assertCountEqual([app for w in workers for app in w["args"][2:]], ["calculator", "calendar", "desktop"])

    def test_delayed_profile_removal_does_not_fail_completed_probes(self):
        result, workers, _ = self.run_driver(["calculator", "desktop"], cleanup_failures=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(workers), 2)

    def test_permanent_profile_removal_failure_fails_the_job(self):
        result, workers, _ = self.run_driver(["calculator", "desktop"], cleanup_failures=100)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(workers), 2)
        self.assertIn("could not remove browser profiles", result.stderr)

    def test_cleanup_retry_preserves_a_failed_probe(self):
        result, _, _ = self.run_driver(["calculator", "desktop"], failing_app="calculator", cleanup_failures=1)
        self.assertEqual(result.returncode, 1)

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

    def test_browser_descendant_is_reaped_before_its_profile_is_removed(self):
        result, workers, browsers = self.run_driver(["calculator"], orphan=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(workers), 1)
        self.assertEqual(len(browsers), 1)

    def test_extra_parallelism_is_rejected(self):
        result, workers, browsers = self.run_driver(["calculator", "desktop"], shards="4")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(workers, [])
        self.assertEqual(browsers, [])


if __name__ == "__main__":
    unittest.main()

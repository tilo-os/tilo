"""Real process ownership and close ordering for the smoke driver's cleanup."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch


REPO = Path(__file__).resolve().parents[2]
DRIVER = REPO / 'tools/web/run_smoke_shards.sh'


class BrowserFixture:
    def __init__(self, *, detached=False, failure=False):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.process = None
        self.detached = detached
        self.failure = failure
        browser = self.root / 'chrome'
        browser.write_text('''#!/usr/bin/env python3
import json,os,pathlib,signal,sys,time
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
profile=pathlib.Path(next(a.split('=',1)[1] for a in sys.argv if a.startswith('--user-data-dir=')))
profile.mkdir(parents=True)
with (root/'launches').open('a') as stream: stream.write(json.dumps(str(profile))+'\\n')
def stop(*args):
 (root/'closing').touch()
 while not (root/'release-browser').exists(): time.sleep(.005)
 if not profile.exists(): (root/'early-removal').touch()
 else: (profile/'shutdown').write_text('flushed')
 (root/'closed').touch()
 sys.exit(0)
signal.signal(signal.SIGTERM,stop)
if os.environ['DETACHED']=='1':
 child=os.fork()
 if child: signal.signal(signal.SIGTERM,lambda *args:sys.exit(0))
 else: os.setsid()
(root/'browser-ready').touch()
while True: time.sleep(.01)
''')
        curl = self.root / 'curl'
        curl.write_text('''#!/usr/bin/env python3
import os,pathlib,sys,time
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
for _ in range(100):
 if (root/'browser-ready').exists(): sys.exit(0)
 time.sleep(.005)
sys.exit(1)
''')
        node = self.root / 'node'
        node.write_text('''#!/usr/bin/env python3
import os,pathlib,sys,time
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
(root/'worker-ready').touch()
while not (root/'release-worker').exists(): time.sleep(.005)
sys.exit(int(os.environ['PROBE_STATUS']))
''')
        for executable in (browser, curl, node):
            executable.chmod(0o755)

    def start(self):
        for name in ('browser-ready', 'worker-ready', 'closing', 'closed', 'release-browser', 'release-worker'):
            (self.root / name).unlink(missing_ok=True)
        environment = dict(os.environ, CHROME=str(self.root / 'chrome'), RUNNER_TEMP=str(self.root),
                           FIXTURE_ROOT=str(self.root), DETACHED='1' if self.detached else '0',
                           PROBE_STATUS='7' if self.failure else '0', TILO_WEB_SMOKE_SHARDS='1',
                           PATH=f'{self.root}:{os.environ["PATH"]}')
        self.process = subprocess.Popen(['bash', str(DRIVER), 'http://fixture', 'calculator'],
                                        cwd=REPO, env=environment, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True)
        self.wait('worker-ready')
        return self.process

    def wait(self, name):
        deadline = time.monotonic() + 5
        while not (self.root / name).exists():
            if self.process.poll() is not None or time.monotonic() >= deadline:
                raise AssertionError(f'driver did not reach {name}')
            time.sleep(.005)

    def finish(self, *, terminate=False, duplicate=False):
        if terminate:
            self.process.terminate()
        else:
            (self.root / 'release-worker').touch()
        self.wait('closing')
        if duplicate:
            self.process.terminate()
        (self.root / 'release-browser').touch()
        stdout, stderr = self.process.communicate(timeout=5)
        return self.process.returncode, stdout + stderr

    def __enter__(self):
        return self

    def __exit__(self, *_exception):
        (self.root / 'release-browser').touch()
        (self.root / 'release-worker').touch()
        if self.process is not None:
            if self.process.poll() is None:
                self.process.terminate()
            self.process.communicate(timeout=5)
        self.directory.cleanup()


class BrowserCleanupLifecycle(unittest.TestCase):
    def assert_closed(self, fixture):
        self.assertTrue((fixture.root / 'closed').exists(), 'writer did not acknowledge its close')
        self.assertFalse((fixture.root / 'early-removal').exists(), 'profile removed while its writer lived')
        self.assertEqual(list(fixture.root.glob('smoke-browsers.*')), [], 'cleanup left a profile behind')

    def test_completed_probes_survive_a_close_request_during_cleanup(self):
        with BrowserFixture() as fixture:
            fixture.start()
            status, output = fixture.finish(duplicate=True)
            self.assertEqual(status, 0, output)
            self.assert_closed(fixture)

    def test_failed_probe_status_survives_a_close_request_during_cleanup(self):
        with BrowserFixture(failure=True) as fixture:
            fixture.start()
            status, output = fixture.finish(duplicate=True)
            self.assertEqual(status, 1, output)
            self.assert_closed(fixture)

    def test_repeated_termination_waits_for_owned_browser_exit(self):
        with BrowserFixture() as fixture:
            fixture.start()
            status, output = fixture.finish(terminate=True, duplicate=True)
            self.assertEqual(status, 143, output)
            self.assert_closed(fixture)

    def test_separate_session_writer_is_reaped_before_profile_removal(self):
        with BrowserFixture(detached=True) as fixture:
            fixture.start()
            status, output = fixture.finish()
            self.assertEqual(status, 0, output)
            self.assert_closed(fixture)

    def test_restart_allocates_a_fresh_profile_after_completed_cleanup(self):
        with BrowserFixture() as fixture:
            for _ in range(2):
                fixture.start()
                status, output = fixture.finish()
                self.assertEqual(status, 0, output)
                self.assert_closed(fixture)
            profiles = [json.loads(line) for line in (fixture.root / 'launches').read_text().splitlines()]
            self.assertEqual(len(profiles), 2)
            self.assertNotEqual(profiles[0], profiles[1])

    def test_closing_one_driver_keeps_another_driver_and_profile_live(self):
        with BrowserFixture() as first, BrowserFixture(detached=True) as second:
            first.start()
            second.start()
            status, output = first.finish()
            self.assertEqual(status, 0, output)
            self.assert_closed(first)
            self.assertIsNone(second.process.poll())
            self.assertFalse((second.root / 'closing').exists(), 'another invocation was signalled')
            self.assertTrue(list(second.root.glob('smoke-browsers.*/0')))
            status, output = second.finish()
            self.assertEqual(status, 0, output)
            self.assert_closed(second)


class BrowserProcessDiscovery(unittest.TestCase):
    @staticmethod
    def supervisor():
        spec = importlib.util.spec_from_file_location(
            'owned_browser', REPO / 'tools/web/owned_browser.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_exit_during_proc_stat_read_is_left_to_waitpid(self):
        supervisor = self.supervisor()
        for error in (FileNotFoundError, ProcessLookupError):
            with self.subTest(error=error), \
                    patch.object(Path, 'iterdir', return_value=[Path('/proc/123')]), \
                    patch.object(Path, 'read_text', side_effect=error):
                self.assertEqual(supervisor.children(), [])

    def test_unreadable_proc_stat_fails_discovery(self):
        supervisor = self.supervisor()
        with patch.object(Path, 'iterdir', return_value=[Path('/proc/123')]), \
                patch.object(Path, 'read_text', side_effect=PermissionError):
            with self.assertRaises(PermissionError):
                supervisor.children()


if __name__ == '__main__':
    unittest.main()

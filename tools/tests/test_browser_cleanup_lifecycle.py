"""Real process ownership and close ordering for the smoke driver's cleanup."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import signal
import sys
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

    def test_starved_term_phase_still_kills_and_reaps(self):
        supervisor = self.supervisor()
        with patch.object(supervisor, 'reap', side_effect=[False, False, True]), \
                patch.object(supervisor, 'children', return_value=[(123, '456')]), \
                patch.object(supervisor, 'owned_fields', return_value=['S']), \
                patch.object(supervisor.os, 'pidfd_open', return_value=42), \
                patch.object(supervisor.os, 'close'), \
                patch.object(supervisor.signal, 'pidfd_send_signal') as send, \
                patch.object(supervisor.time, 'monotonic', side_effect=[0, .1, 5.5, 5.5]), \
                patch.object(supervisor.time, 'sleep') as pause:
            supervisor.quiesce()
        self.assertEqual([call.args for call in send.call_args_list],
                         [(42, signal.SIGTERM), (42, signal.SIGKILL)])
        self.assertEqual(pause.call_count, 2, 'KILL must get a post-signal reaping turn')

    def test_unreapable_child_fails_after_bounded_kill_phase(self):
        supervisor = self.supervisor()
        with patch.object(supervisor, 'reap', return_value=False), \
                patch.object(supervisor, 'children', return_value=[(123, '456')]), \
                patch.object(supervisor, 'owned_fields', return_value=['D']), \
                patch.object(supervisor.os, 'pidfd_open', return_value=42), \
                patch.object(supervisor.os, 'close'), \
                patch.object(supervisor.signal, 'pidfd_send_signal') as send, \
                patch.object(supervisor.time, 'monotonic', side_effect=[0, .1, 5.5, 5.5, 8.6, 8.6]), \
                patch.object(supervisor.time, 'sleep'):
            with self.assertRaisesRegex(RuntimeError, 'profile must remain; pid=123 birth=456 state=D sent=SIGTERM,SIGKILL'):
                supervisor.quiesce()
        self.assertEqual([call.args for call in send.call_args_list],
                         [(42, signal.SIGTERM), (42, signal.SIGKILL)])

    def test_killed_zombie_gets_time_to_finish_exiting(self):
        supervisor = self.supervisor()
        with patch.object(supervisor, 'reap', side_effect=[False, False, False, False, True]), \
                patch.object(supervisor, 'children', return_value=[(123, '456')]), \
                patch.object(supervisor, 'owned_fields', return_value=['Z']), \
                patch.object(supervisor.os, 'pidfd_open', return_value=42), \
                patch.object(supervisor.os, 'close'), \
                patch.object(supervisor.signal, 'pidfd_send_signal'), \
                patch.object(supervisor.time, 'monotonic', side_effect=[0, .1, 5.5, 5.5, 8.6, 8.6]), \
                patch.object(supervisor.time, 'sleep'):
            supervisor.quiesce()

    def test_zombie_that_never_becomes_reapable_still_fails(self):
        supervisor = self.supervisor()
        with patch.object(supervisor, 'reap', return_value=False), \
                patch.object(supervisor, 'children', return_value=[(123, '456')]), \
                patch.object(supervisor, 'owned_fields', return_value=['Z']), \
                patch.object(supervisor.os, 'pidfd_open', return_value=42), \
                patch.object(supervisor.os, 'close'), \
                patch.object(supervisor.signal, 'pidfd_send_signal'), \
                patch.object(supervisor.time, 'monotonic', side_effect=[0, .1, 5.5, 5.5, 40, 40]), \
                patch.object(supervisor.time, 'sleep'):
            with self.assertRaisesRegex(RuntimeError, 'pid=123 birth=456 state=Z sent=SIGTERM,SIGKILL'):
                supervisor.quiesce()

    def test_adopted_children_get_kill_without_renewing_deadline(self):
        supervisor = self.supervisor()
        with patch.object(supervisor, 'reap', return_value=False), \
                patch.object(supervisor, 'children', side_effect=[
                    [(123, '456')], [(123, '456')],  # First KILL and diagnostic.
                    [(124, '789')], [(124, '789')], [(124, '789')]]), \
                patch.object(supervisor, 'owned_fields', return_value=['D']), \
                patch.object(supervisor.os, 'pidfd_open', side_effect=[42, 43]), \
                patch.object(supervisor.os, 'close'), \
                patch.object(supervisor.signal, 'pidfd_send_signal') as send, \
                patch.object(supervisor.time, 'monotonic', side_effect=[0, 2.1, 2.1, 4, 4, 5.2, 5.2]), \
                patch.object(supervisor.time, 'sleep'):
            with self.assertRaisesRegex(RuntimeError, 'pid=124 birth=789 state=D sent=SIGKILL'):
                supervisor.quiesce()
        self.assertEqual([call.args for call in send.call_args_list],
                         [(42, signal.SIGKILL), (43, signal.SIGKILL)])

    def test_reused_pid_or_changed_parent_is_never_signalled(self):
        supervisor = self.supervisor()
        for parent, birth in ((os.getpid(), '789'), (os.getpid() + 1, '456')):
            fields = ['S', str(parent)] + ['0'] * 17 + [birth]
            with self.subTest(parent=parent, birth=birth), \
                    patch.object(supervisor, 'reap', side_effect=[False, True]), \
                    patch.object(supervisor, 'children', return_value=[(123, '456')]), \
                    patch.object(Path, 'read_text', return_value='123 (fixture) ' + ' '.join(fields)), \
                    patch.object(supervisor.os, 'pidfd_open', return_value=42), \
                    patch.object(supervisor.os, 'close') as close, \
                    patch.object(supervisor.signal, 'pidfd_send_signal') as send, \
                    patch.object(supervisor.time, 'sleep'):
                supervisor.quiesce()
            send.assert_not_called()
            close.assert_called_once_with(42)

    def test_unreadable_proc_stat_fails_discovery(self):
        supervisor = self.supervisor()
        with patch.object(Path, 'iterdir', return_value=[Path('/proc/123')]), \
                patch.object(Path, 'read_text', side_effect=PermissionError):
            with self.assertRaises(PermissionError):
                supervisor.children()


class BrowserKillLifecycle(unittest.TestCase):
    def test_term_resistant_detached_descendants_are_killed_and_reaped(self):
        with tempfile.TemporaryDirectory() as directory:
            ready = Path(directory) / 'ready'
            script = '''
import os, pathlib, signal, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = os.fork()
if child == 0:
    os.setsid()
    grandchild = os.fork()
    if grandchild != 0:
        pathlib.Path(sys.argv[1]).write_text(f'{os.getppid()} {os.getpid()} {grandchild}')
# A failsafe bounds the fixture's life even if supervision is broken.
time.sleep(15)
'''
            process = subprocess.Popen(
                [sys.executable, str(REPO / 'tools/web/owned_browser.py'),
                 sys.executable, '-c', script, str(ready)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                deadline = time.monotonic() + 5
                while not ready.exists():
                    self.assertIsNone(process.poll(), 'supervisor exited before fixture startup')
                    self.assertLess(time.monotonic(), deadline, 'owned tree did not start')
                    time.sleep(.005)
                pids = [int(pid) for pid in ready.read_text().split()]
                process.terminate()
                stdout, stderr = process.communicate(timeout=8)
                self.assertEqual(process.returncode, 0, stdout + stderr)
                self.assertIn('owned browser KILL phase:', stderr)
                for pid in pids:
                    self.assertFalse(Path(f'/proc/{pid}').exists(), f'owned child {pid} not reaped')
            finally:
                if process.poll() is None:
                    process.terminate()
                process.communicate(timeout=20)


if __name__ == '__main__':
    unittest.main()

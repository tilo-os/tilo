"""Chromium fixture cleanup must own live writers, not only its root process."""
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import tempfile
import time
import unittest
from hang import HANG_BOUND


REPO = Path(__file__).resolve().parents[2]

BROWSER = r'''#!/usr/bin/env python3
import json, os, pathlib, signal, sys, time
root = pathlib.Path(__file__).parent
profile = pathlib.Path(next(a.split('=', 1)[1] for a in sys.argv if a.startswith('--user-data-dir=')))
(root / 'profile').write_text(str(profile))
(profile / 'Default').mkdir(exist_ok=True)
def identity():
    fields = pathlib.Path('/proc/self/stat').read_text().rsplit(')', 1)[1].split()
    return {'pid': os.getpid(), 'birth': fields[19]}
(root / 'root-process.json').write_text(json.dumps(identity()))
writer = os.fork()
if writer == 0:
    os.setsid()
    (root / 'writer-process.json').write_text(json.dumps(identity()))
    def stop(*_args):
        (root / 'writer-closed').touch()
        sys.exit(0)
    signal.signal(signal.SIGTERM, stop)
    (root / 'writer-ready').touch()
    while True:
        if not profile.exists():
            (root / 'early-removal').touch()
        else:
            directory = profile / 'Default'
            directory.mkdir(exist_ok=True)
            (directory / 'heartbeat').write_text('owned writer')
        time.sleep(.005)
while not (root / 'writer-ready').exists():
    time.sleep(.005)
print('DevTools listening on ws://fixture' + str(root), file=sys.stderr, flush=True)
def stop_root(*_args):
    (root / 'root-closed').touch()
    sys.exit(0)
if (root / 'stall-close').exists():
    signal.signal(signal.SIGTERM, stop_root)
while (root / 'stall-close').exists() or not (root / 'close-request').exists():
    time.sleep(.005)
(root / 'root-closed').touch()
sys.exit(7 if (root / 'fail-root').exists() else 0)
'''

CLIENT = r'''
import {existsSync, readFileSync, writeFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
class FixtureSocket extends EventTarget {
  constructor(endpoint) {
    super();
    this.root = new URL(endpoint).pathname;
    queueMicrotask(() => this.dispatchEvent(new Event('open')));
  }
  send(encoded) {
    const message = JSON.parse(encoded);
    if (message.method !== 'Browser.close') throw new Error('unexpected lifecycle request');
    writeFileSync(this.root + '/close-request', 'requested');
    if (existsSync(this.root + '/no-close-reply')) return;
    queueMicrotask(() => this.dispatchEvent(new MessageEvent('message', {
      data: JSON.stringify({id: message.id, result: {}}),
    })));
  }
  close() {}
}
globalThis.WebSocket = FixtureSocket;
const {launch} = await import(pathToFileURL(process.env.FIXTURE_HELPER));
const roots = JSON.parse(process.env.FIXTURE_ROOTS);
const browsers = [];
for (const root of roots) {
  process.env.CHROME = root + '/fake-chrome';
  browsers.push(await launch());
}
const facts = root => ({
  writerClosed: existsSync(root + '/writer-closed'),
  rootClosed: existsSync(root + '/root-closed'),
  earlyRemoval: existsSync(root + '/early-removal'),
  profileExists: existsSync(readFileSync(root + '/profile', 'utf8')),
});
let closeError = null;
try { await browsers[0].close(); } catch (error) { closeError = String(error); }
const first = facts(roots[0]);
let independent = null;
if (browsers.length > 1) {
  independent = facts(roots[1]);
  await browsers[1].close();
}
console.log(JSON.stringify({first, independent, closeError}));
process.exit(0);
'''


class ChromeProcessCleanup(unittest.TestCase):
    def fixture(self, root):
        root.mkdir()
        executable = root / 'fake-chrome'
        executable.write_text(BROWSER)
        executable.chmod(0o755)

    @staticmethod
    def stop_owned_fixture(root):
        # Failed baseline cleanup can orphan this exact synthetic writer.
        # Bind its recorded start time and private executable before signalling.
        for name in ('writer-process.json', 'root-process.json'):
            record = root / name
            if not record.exists():
                continue
            identity = json.loads(record.read_text())
            pid = identity['pid']
            try:
                descriptor = os.pidfd_open(pid)
            except ProcessLookupError:
                continue
            try:
                fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
                arguments = Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0')
                if fields[19] != identity['birth'] or os.fsencode(root / 'fake-chrome') not in arguments:
                    raise AssertionError('fixture process identity changed; refuse signalling')
                signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                if not select.select([descriptor], [], [], 5)[0]:
                    raise AssertionError('the exact fixture writer did not stop')
            except (FileNotFoundError, ProcessLookupError):
                pass
            finally:
                os.close(descriptor)
        record = root / 'profile'
        if record.exists():
            profile = Path(record.read_text())
            if profile.parent != Path(tempfile.gettempdir()) or not profile.name.startswith('tilo-chrome-'):
                raise AssertionError('refuse deleting an unidentified profile')
            if profile.exists():
                shutil.rmtree(profile)

    def run_fixture(self, *, independent=False, root_failure=False, stalled_close=False, close_reply=False):
        with tempfile.TemporaryDirectory(prefix='tilo-chrome-lifecycle-') as temporary:
            root = Path(temporary)
            roots = [root / 'first']
            if independent:
                roots.append(root / 'second')
            for directory in roots:
                self.fixture(directory)
            if stalled_close:
                (roots[0] / 'stall-close').touch()
                if not close_reply:
                    (roots[0] / 'no-close-reply').touch()
            if root_failure:
                (roots[0] / 'fail-root').touch()
            script = root / 'client.mjs'
            script.write_text(CLIENT)
            environment = dict(os.environ, FIXTURE_ROOTS=json.dumps([str(p) for p in roots]),
                               FIXTURE_HELPER=str(REPO / 'tools/web/chrome.mjs'))
            try:
                process = subprocess.run(['node', str(script)], cwd=REPO, env=environment,
                                         capture_output=True, text=True, timeout=HANG_BOUND)
                self.assertEqual(process.returncode, 0, process.stdout + process.stderr)
                return json.loads(process.stdout)
            finally:
                for directory in roots:
                    self.stop_owned_fixture(directory)

    def test_detached_writer_exits_before_profile_removal(self):
        result = self.run_fixture()
        self.assertIsNone(result['closeError'], result)
        self.assertTrue(result['first']['rootClosed'], result)
        self.assertTrue(result['first']['writerClosed'], result)
        self.assertFalse(result['first']['earlyRemoval'], result)
        self.assertFalse(result['first']['profileExists'], result)

    def test_closing_one_browser_keeps_an_independent_browser_live(self):
        result = self.run_fixture(independent=True)
        self.assertIsNone(result['closeError'], result)
        self.assertTrue(result['first']['writerClosed'], result)
        self.assertFalse(result['independent']['rootClosed'], result)
        self.assertFalse(result['independent']['writerClosed'], result)
        self.assertTrue(result['independent']['profileExists'], result)

    def test_stalled_cdp_close_stops_owned_writers_within_original_deadline(self):
        started = time.monotonic()
        result = self.run_fixture(stalled_close=True, independent=True)
        self.assertIsNone(result['closeError'], result)
        self.assertLess(time.monotonic() - started, 10, 'original shutdown budget exceeded')
        self.assertTrue(result['first']['rootClosed'], result)
        self.assertTrue(result['first']['writerClosed'], result)
        self.assertFalse(result['first']['earlyRemoval'], result)
        self.assertFalse(result['first']['profileExists'], result)
        self.assertFalse(result['independent']['rootClosed'], result)
        self.assertFalse(result['independent']['writerClosed'], result)
        self.assertTrue(result['independent']['profileExists'], result)

    def test_acknowledged_close_still_waits_for_owned_processes(self):
        started = time.monotonic()
        result = self.run_fixture(stalled_close=True, close_reply=True)
        self.assertIsNone(result['closeError'], result)
        self.assertLess(time.monotonic() - started, 10)
        self.assertTrue(result['first']['rootClosed'], result)
        self.assertTrue(result['first']['writerClosed'], result)
        self.assertFalse(result['first']['earlyRemoval'], result)
        self.assertFalse(result['first']['profileExists'], result)

    def test_abnormal_owned_exit_preserves_the_profile_and_fails(self):
        result = self.run_fixture(root_failure=True)
        self.assertIsNotNone(result['closeError'], result)
        self.assertTrue(result['first']['writerClosed'], result)
        self.assertFalse(result['first']['earlyRemoval'], result)
        self.assertTrue(result['first']['profileExists'], result)


if __name__ == '__main__':
    unittest.main()

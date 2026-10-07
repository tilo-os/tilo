"""Owned real-FUSE host/recipient; never reads recipient content to warm it."""
import os
import json
from pathlib import Path
import signal
import subprocess
import uuid

from qualify_apk import digest
from run_arity_journey import REPO, wait_for


class ColdDocumentMount:
    def __init__(self, root, expected, output):
        self.root, self.expected, self.output = root, expected, output
        self.binary = REPO / 'target/debug/fs'
        self.helper = REPO / 'target/debug/examples/android_hydration_fixture'
        self.processes = {}
        self.logs = []
        self.steps = []
        self.manifest = None
        self.document = None
        self.recipient_name = None
        self.record = None

    def checked(self, *arguments):
        return subprocess.run([str(self.helper), *map(str, arguments)], check=True,
                              capture_output=True, text=True, timeout=30).stdout.strip()

    def start(self, name, command):
        mount = self.root / ('h' if name == 'host' else 'r')
        log = (self.output / ('fs-' + name + '.log')).open('a')
        self.logs.append(log)
        env = {**os.environ, 'TILO_IDENTITY_ROOT': str(self.root / ('oi' if name == 'host' else 'ri')),
               'TILO_AUTHORITY_SOCKET_DIR': str(self.root / 'a'), 'TILO_FS_FUSE_OWNER_ONLY': '1',
               'TOKIO_WORKER_THREADS': '2'}
        process = subprocess.Popen([str(self.binary), *command], env=env, stdout=log, stderr=log,
            stdin=subprocess.PIPE if name == 'viewer' else subprocess.DEVNULL, start_new_session=True)
        self.processes[name] = (process, mount)
        if name == 'viewer':
            process.stdin.write((self.root / 'link').read_bytes())
            process.stdin.close()
        def ready():
            if process.poll() is not None:
                raise RuntimeError('owned ' + name + ' filesystem exited; inspect its private log')
            entries = list(mount.iterdir())
            if len(entries) != 1 or not os.path.ismount(mount): return None
            file = entries[0]
            expected_name = self.expected.name if name == 'host' else self.recipient_name
            if file.name != expected_name:
                raise RuntimeError('authenticated filename differs: ' + repr(file.name))
            return file if file.stat().st_size == self.expected.stat().st_size else None
        file = wait_for(ready, name + ' real FUSE mount')
        return file

    def start_viewer(self):
        return self.start('viewer', ['mount-share', '--mount-point', str(self.root / 'r'),
            '--cache-dir', str(self.root / 'rc'), '--link-stdin'])

    def stop(self, name):
        entry = self.processes.get(name)
        if entry is None: return
        process, mount = entry
        if os.path.ismount(mount):
            subprocess.run(['fusermount3', '-u', str(mount)], check=True,
                           capture_output=True, timeout=10)
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try: process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=10)
        if os.path.ismount(mount): raise RuntimeError('owned FUSE mount survived cleanup')
        del self.processes[name]

    def state(self, expected):
        def observed():
            process = self.processes['viewer'][0]
            if process.poll() is not None: raise RuntimeError('recipient exited before observation')
            return os.getxattr(self.document, 'user.tilo.sync') == expected
        wait_for(observed, 'actual recipient state ' + expected.decode())

    def require_projected(self):
        if os.getxattr(self.document, 'user.tilo.sync') != b'projected':
            raise RuntimeError('fixture hydrated before the actual Android picker selection')
        self.steps.append({'phase': 'before-android-selection', 'projected': True})

    def inspect(self, resident):
        if 'viewer' in self.processes: raise RuntimeError('backend inspection requires stopped recipient')
        args = ['inspect', self.root / 'rc/blobs', self.expected, self.manifest]
        if resident: args.append('--resident')
        return self.checked(*args)

    def __enter__(self):
        try:
            self.root.mkdir(mode=0o700)
            for name in ('seed', 'h', 'r', 'a'):
                (self.root / name).mkdir(mode=0o700)
            (self.root / 'seed' / self.expected.name).write_bytes(self.expected.read_bytes())
            self.checked('identity', self.root / 'oi', '--owner')
            self.checked('identity', self.root / 'ri')
            file = self.start('host', ['serve', '--data-plane=fuse', '--mount-point', str(self.root / 'h'),
                '--cache-dir', str(self.root / 'hc'), '--seed', str(self.root / 'seed')])
            self.manifest = os.getxattr(file, 'user.tilo.content-hash').hex()
            record = uuid.UUID(bytes=os.getxattr(file, 'user.tilo.record-id'))
            self.record = str(record)
            self.recipient_name = 'Shared-' + record.hex[:12]
            self.document = self.root / 'r' / self.recipient_name
            wait_for(lambda: (self.root / 'hc/share-control.sock').is_socket(), 'owned share control')
            self.checked('share', self.root / 'hc/share-control.sock', record, self.root / 'link')
            self.start_viewer()
            self.state(b'projected')
            self.stop('viewer')
            observation = self.inspect(False)
            self.start_viewer()
            self.state(b'projected')
            self.steps.append({'phase': 'initial-cold-backend', 'observed': observation})
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def evict_and_restart(self):
        self.state(b'local')
        self.stop('viewer')
        self.steps.append({'phase': 'actual-apk-read-restored', 'observed': self.inspect(True)})
        self.start_viewer()
        self.state(b'local')
        # Existing safe device control verifies signed exact-version replica evidence.
        os.setxattr(self.document, 'user.tilo.availability-control', b'evict')
        self.state(b'projected')
        def completed():
            if self.processes['viewer'][0].poll() is not None:
                raise RuntimeError('recipient exited before durable eviction completion')
            queue = json.loads((self.root / 'rc/evictions.json').read_text())
            requests = queue['requests']
            if len(requests) != 1 or requests[0]['record_id'] != self.record:
                raise RuntimeError('eviction queue does not name the owned exact record')
            return requests[0]['state']['state'] == 'completed'
        # Projected is not a completion barrier. The live observer must persist
        # backend absence before shutdown, or restart resumes the old intent.
        wait_for(completed, 'durable exact-record eviction completion')
        self.stop('viewer')
        self.steps.append({'phase': 'safe-eviction-backend', 'observed': self.inspect(False)})
        self.start_viewer()
        self.state(b'projected')
        self.steps.append({'phase': 'recipient-restarted', 'projected': True})

    def verify_restored(self, phase='actual-apk-relaunch-restored'):
        self.state(b'local')
        self.stop('viewer')
        self.steps.append({'phase': phase, 'observed': self.inspect(True)})
        self.start_viewer()
        self.state(b'local')

    def report(self):
        self.state(b'local')
        return {'steps': self.steps, 'content_manifest': self.manifest,
                'source_sha256': digest(self.expected), 'fs_sha256': digest(self.binary),
                'fixture_helper_sha256': digest(self.helper), 'real_fuse': True,
                'signed_os_image_qualified': False,
                'revocation_checked_with_resident_source': True}

    def __exit__(self, kind, value, traceback):
        try:
            self.stop('viewer')
        finally:
            try: self.stop('host')
            finally:
                for log in self.logs: log.close()

#!/usr/bin/python3
"""Closed installed data consumer contracts. Digests bind artifacts, not execution.

Applications qualification is exclusively the Rust owned-disposable factory.
Read-only checks revalidate its protected native receipt and signed source cohort;
they never rerun capture or manufacture a successful qualification.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import tempfile
import time

ENV = {'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C'}
HELPER = '/usr/libexec/tilo-prepared-setup'
HOME = 'home-consumer-capabilities.json'
APPS = 'applications-consumer-capabilities.json'
NATIVE = 'applications-consumer-native-qualification.json'
ORIGINAL = 'original-source-consumer-capabilities.json'
TOOLS = ('/usr/bin/flatpak', '/usr/bin/ostree', '/usr/bin/unshare', '/usr/bin/setpriv')
HOME_KEYS = {'schema', 'archive_protocol', 'seed_protocol', 'firstboot_user_sha256',
             'seed_helper_sha256', 'prepared_helper_sha256', 'tar_sha256'}
APPS_KEYS = {'schema', 'subset_protocol', 'prepared_helper_sha256',
             'qualification_script_sha256', 'native_qualification_sha256'}
NATIVE_KEYS = {'schema', 'subset_protocol', 'source_protocol', 'prepared_helper_sha256',
               'tools', 'source_read_claim_sha256', 'source_completion_receipt_sha256',
               'setup_ciphertext_sha256', 'applications_ciphertext_sha256', 'receiving'}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate consumer proof field')
        result[key] = value
    return result


def decode(data, limit):
    require(0 < len(data) <= limit, 'consumer response exceeds bound')
    return json.loads(data, object_pairs_hook=unique)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def shape(value, keys, protocols):
    require(type(value) is dict and set(value) == keys, 'unknown or missing consumer fields')
    for key, expected in protocols.items():
        require(type(value[key]) is int and value[key] == expected, 'unsupported consumer protocol')
    for key in keys:
        if key.endswith('_sha256'):
            require(type(value[key]) is str and re.fullmatch('[0-9a-f]{64}', value[key]),
                    'invalid consumer digest')
    return value


class Store:
    """The root/owner seams are Python test seams, never installed CLI arguments."""
    def __init__(self, root=Path('/'), owner=0):
        self.root = Path(root)
        self.owner = owner

    def path(self, name):
        require(name.startswith('/') and all(p not in ('', '.', '..') for p in name[1:].split('/')),
                'invalid fixed artifact path')
        return self.root / name[1:]

    def trusted(self, path):
        # Check all namespace edges, including the root. No redirected files or directories.
        require(path.is_relative_to(self.root), 'artifact outside installed root')
        edges = [self.root]
        edge = self.root
        for part in path.relative_to(self.root).parts:
            edge = edge / part
            edges.append(edge)
        for edge in edges:
            info = edge.lstat()
            require(info.st_uid == self.owner and not info.st_mode & 0o022
                    and not stat.S_ISLNK(info.st_mode), 'consumer artifact is not image-owned')
        return info

    @staticmethod
    def stamp(info):
        return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_gid,
                info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns)

    def read(self, name, limit, executable=False):
        path = self.path(name)
        before = self.trusted(path)
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1
                and 0 < before.st_size <= limit and (not executable or before.st_mode & 0o111),
                'consumer artifact is not bounded regular executable/data')
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
        with os.fdopen(fd, 'rb') as source:
            require(self.stamp(before) == self.stamp(os.fstat(source.fileno())), 'consumer replaced')
            data = source.read(limit + 1)
            require(len(data) == before.st_size and self.stamp(before) == self.stamp(os.fstat(source.fileno()))
                    and self.stamp(before) == self.stamp(self.trusted(path)), 'consumer changed')
        return data

    def digest(self, name, limit=1 << 30, executable=True):
        # Stream larger executables rather than placing an unbounded binary in memory.
        path = self.path(name)
        before = self.trusted(path)
        require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= limit
                and (not executable or before.st_mode & 0o111), 'invalid installed executable')
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
        with os.fdopen(fd, 'rb') as source:
            require(self.stamp(before) == self.stamp(os.fstat(source.fileno())), 'consumer replaced')
            value = hashlib.file_digest(source, 'sha256').hexdigest()
            require(self.stamp(before) == self.stamp(os.fstat(source.fileno()))
                    and self.stamp(before) == self.stamp(self.trusted(path)), 'consumer changed')
        return value

    def receipt(self, name, limit):
        return decode(self.read('/usr/share/tilo/' + name, limit), limit)

    def publish(self, name, data, limit):
        require(os.geteuid() == 0, 'consumer publication requires root')
        require(0 < len(data) <= limit, 'consumer proof exceeds public bound')
        parent = self.path('/usr/share/tilo')
        self.trusted(parent)
        target = parent / name
        require(not os.path.lexists(target), 'consumer receipt already exists; refusing replacement')
        fd, temporary = tempfile.mkstemp(prefix='.data-consumer-', dir=parent)
        try:
            with os.fdopen(fd, 'wb') as output:
                os.fchmod(output.fileno(), 0o644)
                output.write(data)
                output.flush()
                os.fsync(output.fileno())
            os.link(temporary, target, follow_symlinks=False)  # atomic, no replacement
            directory = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            os.unlink(temporary)


def invoke(command, limit=16384, timeout=120):
    """Own and retire the exact process group on every exit, with bounded pipes."""
    child = subprocess.Popen(command, env=ENV, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    output = bytearray()
    counts = {child.stdout: 0, child.stderr: 0}
    deadline = time.monotonic() + timeout
    selector = selectors.DefaultSelector()
    try:
        for pipe in counts:
            os.set_blocking(pipe.fileno(), False)
            selector.register(pipe, selectors.EVENT_READ)
        while selector.get_map():
            require(time.monotonic() < deadline, 'consumer command deadline exceeded')
            for key, _ in selector.select(min(0.1, max(0, deadline - time.monotonic()))):
                data = os.read(key.fileobj.fileno(), 4096)
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                counts[key.fileobj] += len(data)
                require(counts[key.fileobj] <= limit, 'consumer command output exceeds bound')
                if key.fileobj is child.stdout:
                    output.extend(data)
        # waitid observes without reaping: the leader PID stays reserved until group cleanup.
        while not os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT):
            require(time.monotonic() < deadline, 'consumer command deadline exceeded')
            time.sleep(0.005)
        os.killpg(child.pid, signal.SIGKILL)
        require(child.wait() == 0, 'installed consumer command failed')
        return bytes(output)
    finally:
        # Never signal an already reaped group (its PID may have been recycled).
        if child.returncode is None:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait()
        selector.close()
        child.stdout.close()
        child.stderr.close()


def home_facts(store):
    return {'schema': 1, 'archive_protocol': 2, 'seed_protocol': 1,
            'prepared_helper_sha256': store.digest(HELPER),
            'firstboot_user_sha256': store.digest('/usr/libexec/tilo-firstboot-user', 1 << 20),
            'seed_helper_sha256': store.digest('/usr/libexec/tilo-firstboot-home-seeds', 64 << 20),
            'tar_sha256': store.digest('/usr/bin/tar', 64 << 20)}


def home(store, check=False, runner=invoke):
    dependencies = ('/usr/libexec/tilo-verify-home-consumer-capability',
                    '/usr/libexec/tilo-installed-data-consumer.py',
                    '/usr/libexec/tilo-firstboot-files')
    # Bind the probe to the installed compiled publication helper, never its test oracle.
    infrastructure = {name: store.digest(name, 64 << 20) for name in dependencies}
    before = home_facts(store)
    actual = shape(decode(runner([str(store.path(HELPER)), '--home-consumer-capability']), 4096),
                   HOME_KEYS, {'schema': 1, 'archive_protocol': 2, 'seed_protocol': 1})
    require(actual == before, 'installed Home probe disagrees with exact artifacts')
    seed = decode(runner([str(store.path('/usr/libexec/tilo-firstboot-home-seeds')), '--capabilities']), 4096)
    shape(seed, {'schema', 'seed_protocol'}, {'schema': 1, 'seed_protocol': 1})
    require(home_facts(store) == before
            and {name: store.digest(name, 64 << 20) for name in dependencies} == infrastructure,
            'Home artifacts changed during probe')
    if check:
        require(shape(store.receipt(HOME, 4096), HOME_KEYS,
                      {'schema': 1, 'archive_protocol': 2, 'seed_protocol': 1}) == actual,
                'installed Home receipt differs from actual consumers')
    else:
        store.publish(HOME, canonical(actual) + b'\n', 4096)
    return actual


def apps_artifacts(store):
    return {'prepared_helper_sha256': store.digest(HELPER),
            'qualification_script_sha256': store.digest('/usr/libexec/tilo-verify-applications-consumer-capability', 1 << 20),
            'module_sha256': store.digest('/usr/libexec/tilo-installed-data-consumer.py', 1 << 20),
            'tools': {name: store.digest(name) for name in TOOLS}}


def native_proof(native, capability, facts):
    shape(native, NATIVE_KEYS, {'schema': 1, 'subset_protocol': 2, 'source_protocol': 6})
    require(native['prepared_helper_sha256'] == facts['prepared_helper_sha256']
            and native['tools'] == facts['tools'], 'native qualification artifact mismatch')
    report = native['receiving']
    shape(report, {'schema', 'subset_protocol', 'deployments', 'install_commands', 'commits',
                   'selected_data_sha256'}, {'schema': 1, 'subset_protocol': 2, 'deployments': 2, 'install_commands': 2})
    require(type(report['commits']) is dict
            and set(report['commits']) == {'app/org.tilo.Fixture/x86_64/stable',
                                          'runtime/org.tilo.FixtureRuntime/x86_64/stable'}
            and all(type(v) is str and re.fullmatch('[0-9a-f]{64}', v) for v in report['commits'].values()),
            'receiving qualification did not restore and retry')
    require(hashlib.sha256(canonical(native)).hexdigest() == capability['native_qualification_sha256'],
            'native qualification receipt digest mismatch')


def applications(store, check=False, runner=invoke):
    before = apps_artifacts(store)
    if check:
        capability = shape(store.receipt(APPS, 4096), APPS_KEYS, {'schema': 1, 'subset_protocol': 2})
        native_bytes = store.read('/usr/share/tilo/' + NATIVE, 8192)
        native = decode(native_bytes, 8192)
        require(native_bytes == canonical(native), 'native qualification bytes are not canonical')
    else:
        require(os.geteuid() == 0, 'native application qualification requires root')
        for name in (APPS, NATIVE, ORIGINAL):
            require(not os.path.lexists(store.path('/usr/share/tilo/' + name)),
                    'native qualification must begin without previous receipts')
        # No path/receipt/boolean input: the Rust factory checks disposable ownership,
        # real original-user grants, signed source6 cohort, target restore/retry and retirement.
        command = [str(store.path(HELPER)), '--applications-consumer-capability']
        # Actual capture/receiving qualification can be long; only the producer
        # gets this budget. Rootless admission remains a finite 120-second check.
        output = invoke(command, limit=8192, timeout=1800) if runner is invoke else runner(command)
        envelope = decode(output, 8192)
        require(type(envelope) is dict and set(envelope) == {'capability', 'native_qualification', 'original_source'},
                'invalid actual native qualification envelope')
        capability = shape(envelope['capability'], APPS_KEYS, {'schema': 1, 'subset_protocol': 2})
        native = envelope['native_qualification']
    require(all(capability[key] == before[key] for key in ('prepared_helper_sha256', 'qualification_script_sha256')),
            'application capability artifacts differ')
    native_proof(native, capability, before)
    require(apps_artifacts(store) == before, 'applications artifacts changed during qualification')
    if not check:
        # The app capability is the completion marker, published LAST. A crash before
        # it leaves admission unavailable; no read-only check ever repairs publication.
        store.publish(ORIGINAL, canonical(envelope['original_source']) + b'\n', 8192)
    original = decode(runner([str(store.path(HELPER)), '--reattest-original-source-consumer-capability']), 16384)
    require(original == store.receipt(ORIGINAL, 8192), 'signed original consumer reattestation differs')
    require(type(original.get('original_source_protocol')) is int
            and original['original_source_protocol'] == 6
            and original.get('prepared_helper_sha256') == before['prepared_helper_sha256']
            and type(original.get('capture_pins')) is dict
            and type(original['capture_pins'].get('worker_protocol')) is int
            and original['capture_pins']['worker_protocol'] == 6,
            'applications require the exact reattested source6 cohort')
    if check:
        require(store.receipt(APPS, 4096) == capability
                and store.read('/usr/share/tilo/' + NATIVE, 8192) == native_bytes,
                'application receipts changed during reattestation')
    require(apps_artifacts(store) == before, 'applications artifacts changed during reattestation')
    if not check:
        store.publish(NATIVE, canonical(native), 8192)
        store.publish(APPS, canonical(capability) + b'\n', 4096)
    return capability


def main(kind, argv):
    require(argv in ([], ['--check']), 'consumer verifier accepts only --check')
    if not argv:
        require(os.geteuid() == 0, 'consumer producer requires root')
    proof = (home if kind == 'home' else applications)(Store(), check=bool(argv))
    print(canonical(proof).decode())

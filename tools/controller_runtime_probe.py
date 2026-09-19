"""Two-Pod storage/lock probe with simulated dispatch crash; never packaging.

Invoke via python3 -I tools/ci/python.py tools/controller_runtime_probe.py.
The CLI has no configurable state path, transport, token or policy input.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import sys
import time

import app_packaging_coordinator as core
import app_packaging_service as service

STATE = Path('/state/runtime-qualification.sqlite')
DESIRED = {'source_sha': 'a' * 40, 'qualification': 'simulated-dispatch-crash-only'}
REVIEWED_BASE = 'd4e96bedb0aba46801a2bb86c5533182b9301a2e'
SOURCE_HASHES = {
    'tools/app_packaging_coordinator.py': '87ed9f1d592f80edb0107e30ebd45b93988196097a3b629019e49ead676e0bbc',
    'tools/app_packaging_service.py': '35017c49b27ef7a0c7702e13c015aaf857766b4b3281682867d34d365d17b675',
    'tools/ci/python.py': '90e2cec5d2f48817ac0d8ce2276b235d3e1321ecdcd1f678230cc436cfe8d25f',
}


def require(value, message):
    if not value:
        raise ValueError(message)


def checked_source():
    root = Path(__file__).resolve().parents[1]
    actual = {path: hashlib.sha256((root / path).read_bytes()).hexdigest() for path in SOURCE_HASHES}
    require(actual == SOURCE_HASHES, 'controller/service/launcher differ from reviewed source')
    return actual


def guard(state):
    require(state.name == 'runtime-qualification.sqlite', 'only fixture database permitted')
    require(state.parent.is_dir() and not state.parent.is_symlink(), 'dedicated state mount required')
    require(not (state.parent / 'coordinator.sqlite').exists()
            and not (state.parent / 'coordinator.sqlite').is_symlink(), 'real coordinator database must be absent')
    for path in (state, Path(str(state) + '.lock')):
        require(not path.is_symlink(), 'fixture links refused')
        if path.exists():
            require(path.is_file() and path.stat().st_nlink == 1 and path.stat().st_uid == os.geteuid(),
                    'fixture must be a singly linked file owned by this UID')


def read(state):
    store = core.Store(state)
    try:
        return store.read()
    finally:
        if store.db:
            store.db.close()


def child(callback, expected):
    pid = os.fork()
    if pid == 0:
        try:
            callback()
        except BaseException:
            os._exit(90)
        os._exit(91)
    deadline = time.monotonic() + 10
    while True:
        completed, status = os.waitpid(pid, os.WNOHANG)
        if completed:
            require(os.waitstatus_to_exitcode(status) == expected, 'unexpected probe child result')
            return
        if time.monotonic() >= deadline:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            raise ValueError('nonblocking probe child timed out')
        time.sleep(0.01)


def assert_intent(state, expected_id=None):
    snapshot = read(state)
    active = snapshot['active']
    require(snapshot['sequence'] == 1 and snapshot['desired'] == DESIRED
            and snapshot['baseline'] is None and snapshot['last_attempt'] is None, 'unexpected fixture state')
    require(active and active['desired'] == DESIRED and active['status'] == 'dispatch-intent'
            and 'run_id' not in active and active['receipt_sha256'] == core.receipt_key(DESIRED),
            'committed pre-dispatch intent missing')
    require(expected_id is None or active['id'] == expected_id, 'persisted intent differs from write Pod')
    require(core.next_action(snapshot) == 'reconcile-required', 'uncertain dispatch must require reconciliation')
    return snapshot


def write_phase(state):
    guard(state)
    require(not state.exists() and not Path(str(state) + '.lock').exists(), 'write phase requires fresh fixture files')
    with service.invocation_lock(state):
        def rejected_contender():
            try:
                with service.invocation_lock(state):
                    os._exit(92)
            except ValueError as error:
                require(str(error) == 'packaging invocation already active', 'unexpected lock failure')
                os._exit(0)
        child(rejected_contender, 0)
        store = core.Store(state, writable=True)
        try:
            store.observe(1, DESIRED)
        finally:
            store.db.close()
        def simulated_dispatch_crash():
            class FakeNoNetworkDispatcher:
                def qualify_receipt(self, desired):
                    require(desired == DESIRED, 'unexpected qualification input')
                def dispatch(self, intent):
                    # A separate read-only connection must see the committed
                    # intent before the simulated process loss, not just memory.
                    assert_intent(state, intent['id'])
                    os._exit(73)
            crashed = core.Store(state, writable=True)
            try:
                core.tick(crashed, FakeNoNetworkDispatcher())
            finally:
                crashed.db.close()
        child(simulated_dispatch_crash, 73)
        snapshot = assert_intent(state)
    guard(state)
    return {'phase': 'write', 'intent_id': snapshot['active']['id'],
            'cross_process_nonblocking_lock_rejection': True, 'simulated_dispatch_exit': 73,
            'committed_intent_survived': True}


def restart_phase(state, intent_id):
    guard(state)
    require(state.is_file() and Path(str(state) + '.lock').is_file(), 'persisted fixture files required')
    with service.invocation_lock(state):
        before = hashlib.sha256(state.read_bytes()).hexdigest()
        snapshot = assert_intent(state, intent_id)
        class NeverAPI:
            def __getattribute__(self, name):
                raise AssertionError('API access forbidden in reconcile-required state: ' + name)
        store = core.Store(state)
        try:
            core.tick(store, NeverAPI())
            require(store.read() == snapshot, 'read-only reconciliation changed state')
        finally:
            store.db.close()
        # None would fail immediately if the dry-run path started reading policy
        # or credentials. There are no such files mounted in this qualification.
        observation = service.invoke(state, None, None, None, apply=False)
        require(observation == {'dry_run': True, 'next_action': 'reconcile-required', 'state': snapshot},
                'service dry-run did not observe persisted state')
        require(hashlib.sha256(state.read_bytes()).hexdigest() == before, 'restart observation wrote the database')
    guard(state)
    # SQLite may create its normal transient rollback journal while committing.
    # A crash after the completed intent transaction must leave none to clean up.
    for suffix in ('-journal', '-wal', '-shm'):
        require(not Path(str(state) + suffix).exists(), 'unexpected SQLite sidecar; retain evidence for investigation')
    state.unlink()
    Path(str(state) + '.lock').unlink()
    guard(state)
    return {'phase': 'restart', 'intent_id': intent_id, 'next_action': 'reconcile-required',
            'no_api_access': True, 'service_dry_run_same_state': True, 'fixture_files_removed': True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=('write', 'restart'))
    parser.add_argument('--expected-intent')
    args = parser.parse_args()
    require(sys.flags.isolated and sys.platform == 'linux' and os.geteuid() == 1000,
            'isolated Linux process with UID 1000 required')
    require((args.phase == 'restart') == bool(args.expected_intent), 'restart requires write-Pod intent ID only')
    def no_network(event, values):
        if event.startswith('socket.'):
            raise RuntimeError('network forbidden for storage qualification')
    sys.addaudithook(no_network)
    source_hashes = checked_source()
    result = write_phase(STATE) if args.phase == 'write' else restart_phase(STATE, args.expected_intent)
    result.update(scope='simulated-dispatch-crash/storage/lock qualification only', reviewed_base=REVIEWED_BASE,
                  source_sha256=source_hashes, uid=os.geteuid(), state_path=str(STATE), live_packaging_acceptance=False)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()

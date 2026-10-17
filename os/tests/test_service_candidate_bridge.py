"""Owned bridge boundaries; mocks do not certify native route qualification."""
import json
import re
import subprocess
from pathlib import Path
import stat
import sys
import types
import unittest
from unittest.mock import patch, Mock

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / 'os/scripts/installer-receive-fixture'
saved_path = list(sys.path)
try:
    sys.path.insert(0, str(FIXTURE))
    import service_contract as contract
    import delivery
    import service_sessions as sessions
    import service_processes
finally:
    sys.path[:] = saved_path


def request():
    value = {key: 'ab' * 32 for key in contract.SHA_FIELDS}
    value.update(schema=2, root='/', shell_path='/usr/bin/shell', required_protocol=2,
                 sdk_runtime_version='1+fixture', cef_runtime_version='1+fixture-chromium',
                 receiving_dependencies={'/usr/libexec/fixture.py': 'ab' * 32},
                 receiving_python_path='/usr/bin/python3')
    return value


class ServiceCandidateBridge(unittest.TestCase):
    def test_request_is_closed_and_bound_to_actual_root_protocol(self):
        value = request()
        self.assertEqual(contract.decode(json.dumps(value).encode()), value)
        for altered in (dict(value, required_protocol=True), dict(value, root='/tmp'),
                        dict(value, required_protocol=1), dict(value, ready=True)):
            with self.assertRaises(RuntimeError):
                contract.decode(json.dumps(altered).encode())
        with self.assertRaisesRegex(RuntimeError, 'duplicate'):
            contract.decode(b'{"schema":2,"schema":2}')
        with self.assertRaisesRegex(RuntimeError, 'bound'):
            contract.decode(b' ' * (16 * 1024 + 1))

    def test_measured_receiving_closure_uses_the_producer_request_bound(self):
        value = request()
        value['receiving_dependencies'] = {
            path: 'ab' * 32 for path in contract.RECEIVING_DEPENDENCIES
        }
        value['sdk_runtime_version'] = 'v' * 256
        value['cef_runtime_version'] = 'v' * 247 + '-chromium'
        encoded = json.dumps(value).encode()
        self.assertLessEqual(len(encoded), 16 * 1024)
        self.assertEqual(contract.decode(encoded), value)

    def test_measured_closure_matches_the_installed_producer(self):
        source = (ROOT / 'crates/platform/imager-backend/src/account_admission.rs').read_text()
        block = source.split('pub const RECEIVING_DEPENDENCIES: &[&str] = &[', 1)[1].split('];', 1)[0]
        self.assertEqual(tuple(re.findall(r'"([^"]+)"', block)), contract.RECEIVING_DEPENDENCIES)

    def test_isolated_bridge_entry_imports_only_its_fixture_directory(self):
        completed = subprocess.run([sys.executable, '-I', str(FIXTURE / 'service_contract.py')],
                                   capture_output=True, timeout=30, env={'PATH': '/usr/bin:/bin'})
        self.assertNotEqual(completed.returncode, 0)
        self.assertEqual(completed.stdout, b'')
        self.assertIn(b'accepts only --owned-bridge', completed.stderr)

    def test_native_scope_refuses_ordinary_root_namespace_before_artifact_work(self):
        import os
        original_open = os.open
        def namespace_only(path, flags, *args):
            self.assertEqual(path, '/owned/initial-pidns')
            return original_open('/proc/self/ns/pid', flags & ~os.O_NOFOLLOW)
        with patch.object(contract.os, 'getuid', return_value=0), \
             patch.object(contract.os, 'geteuid', return_value=0), \
             patch.object(contract.os, 'open', side_effect=namespace_only), \
             patch.object(contract, 'admit_request') as inspect, \
             patch.object(contract, 'bounded_command') as execute:
            with self.assertRaisesRegex(RuntimeError, 'disposable owned PID namespace'):
                contract.install_native_scope(request(), 'receiver', Path(delivery.PREPARED), Mock(),
                    authenticated={'identity_public_key': [1] * 32, 'setup_binding': 'native-person'})
            inspect.assert_not_called()
            execute.assert_not_called()

    def test_native_scope_refuses_unprivileged_actor_before_namespace_work(self):
        with patch.object(contract.os, 'getuid', return_value=1000), \
             patch.object(contract.os, 'geteuid', return_value=1000), \
             patch.object(contract.os, 'open') as opened:
            with self.assertRaisesRegex(RuntimeError, 'owned root'):
                contract.install_native_scope(request(), 'source', Path('/tmp/imager'), Mock())
            opened.assert_not_called()

    def test_revocation_rejects_local_cleanup_and_unobserved_backend(self):
        selected = {'id': 'selected-slack', 'service': 'slack', 'setup_binding': 'native-person'}
        report = dict(session_id=selected['id'], status='forgotten', custody_revoked=True,
                      custody_backend_needed=False, local_only=False, engine_shutdown_confirmed=True)
        self.assertTrue(sessions.revoked(report, selected))
        for changed in (dict(report, custody_revoked=False), dict(report, local_only=True),
                        dict(report, engine_shutdown_confirmed=False),
                        dict(report, custody_backend_needed=True), dict(report, status='local-forgotten'),
                        dict(report, session_id='different-slack')):
            with self.assertRaises(RuntimeError):
                sessions.revoked(changed, selected)
        self.assertFalse(sessions.revoked(dict(report, status='forgetting', custody_revoked=False), selected))

    def test_profile_removal_is_observed_on_held_directory_not_report_flags(self):
        import os
        import tempfile
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'selected-slack'
            path.mkdir()
            descriptor = os.open(root, os.O_PATH | os.O_DIRECTORY)
            try:
                self.assertFalse(service_processes.profile_removed(descriptor, {'id': path.name}))
                path.rmdir()
                self.assertTrue(service_processes.profile_removed(descriptor, {'id': path.name}))
            finally:
                os.close(descriptor)

    def test_candidate_scope_never_contains_consumer_completion(self):
        scope = contract.candidate_scope({'request': request(), 'subjects': {}})
        self.assertEqual(scope['required_protocol'], 2)
        self.assertEqual(set(scope), {'required_protocol', 'browser_package_hash',
                                     'cef_runtime_root_hash', 'sdk_runtime_root_hash',
                                     'receiving_route_qualification_sha256'})
        self.assertTrue(set(scope).isdisjoint(contract.CONDITIONS))

    def test_bridge_calls_only_fixed_existing_coordinator_with_private_input(self):
        raw = json.dumps(request()).encode()
        command = ['/usr/bin/python3', '-I', str(Path('/usr/libexec') /
                   'tilo-installer-receive-fixture/receive.py'), '--service-session-qualification']
        env = {'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C'}
        def path(name):
            item = Mock()
            item.resolve.return_value = item
            item.lstat.return_value = types.SimpleNamespace(st_mode=stat.S_IFDIR | 0o700, st_uid=0)
            item.stat.return_value = types.SimpleNamespace(st_ino=2)
            item.open.return_value.__enter__ = Mock(return_value=Mock(fileno=lambda: 42))
            item.open.return_value.__exit__ = Mock(return_value=False)
            return item
        with patch.object(contract, 'Path', side_effect=path), \
             patch.object(contract.os, 'getuid', return_value=0), \
             patch.object(contract.os, 'geteuid', return_value=0), \
             patch.object(contract.os, 'open', return_value=43), \
             patch.object(contract.os, 'fstat', return_value=types.SimpleNamespace(st_uid=0, st_ino=1)), \
             patch.object(contract.fcntl, 'ioctl', return_value=0x20000000), \
             patch.object(contract.os, 'fchmod'), patch.object(contract.os, 'close') as close, \
             patch.object(contract, 'bounded_command', return_value=b'observed') as execute:
            self.assertEqual(contract.owned_bridge(command, env, raw), b'observed')
            argv, deadline, log, value, maximum = execute.call_args.args
            self.assertEqual(argv, command + ['--candidate', '/owned/candidate', '--evidence', '/evidence/owned'])
            self.assertEqual(value, raw)
            self.assertEqual(maximum, 4096)
            close.assert_called_once_with(43)
            with patch.object(contract.fcntl, 'ioctl', return_value=0):
                with self.assertRaisesRegex(RuntimeError, 'namespace witness'):
                    contract.owned_bridge(command, env, raw)
            with patch.object(contract.os, 'fstat', return_value=types.SimpleNamespace(st_uid=0, st_ino=2)):
                with self.assertRaisesRegex(RuntimeError, 'namespace witness'):
                    contract.owned_bridge(command, env, raw)
            execute.assert_called_once()
            for wrong in (command + ['--candidate', '/tmp'], ['/tmp/python', *command[1:]]):
                with self.assertRaisesRegex(RuntimeError, 'canonical'):
                    contract.owned_bridge(wrong, env, raw)
            with self.assertRaisesRegex(RuntimeError, 'canonical'):
                contract.owned_bridge(command, dict(env, PYTHONPATH='/tmp'), raw)

    def test_no_bridge_without_actual_root_owner(self):
        with patch.object(contract.os, 'getuid', return_value=1000):
            with self.assertRaisesRegex(RuntimeError, 'owned root'):
                contract.owned_bridge([], {}, json.dumps(request()).encode())

    def test_missing_removal_or_any_retirement_never_completes(self):
        result = {name: True for name in ('receiving_namespace_retired', 'receiving_services_retired',
                  'source_physical_retired', 'target_physical_retired')}
        result['cloud'] = {'server_upload_observed': True}
        result['service_sessions'] = {'schema': 1, 'conditions': {key: True for key in contract.CONDITIONS}}
        admission = {'request': request()}
        self.assertEqual(contract.complete(admission, result)['schema'], 2)
        for name in contract.CONDITIONS:
            for wrong in (False, 1, None):
                changed = dict(result, service_sessions={'schema': 1,
                    'conditions': dict(result['service_sessions']['conditions'], **{name: wrong})})
                with self.assertRaises(RuntimeError):
                    contract.complete(admission, changed)
        for name in result:
            changed = dict(result)
            del changed[name]
            with self.assertRaises(RuntimeError):
                contract.complete(admission, changed)
        changed = dict(result, service_sessions={'schema': 1,
            'conditions': {key: value for key, value in result['service_sessions']['conditions'].items()
                           if key != 'revocation_verified'}})
        with self.assertRaises(RuntimeError):
            contract.complete(admission, changed)

    def test_native_selection_binding_cannot_be_replaced_by_source_hint(self):
        saved_path = list(sys.path)
        try:
            sys.path.insert(0, str(FIXTURE))
            native = {'schema': 1, 'sessions': [
                {'id': kind + '-actual', 'service': kind, 'setup_binding': 'actual-binding'}
                for kind in ('slack', 'discord')]}
            attested = delivery.AuthenticatedStagingPlan({}, {}, native, 2)
            scope = contract.candidate_scope({'request': request()})
            self.assertEqual(len(delivery.bind_service_candidate(attested, scope, 'actual-binding')), 2)
            with self.assertRaisesRegex(RuntimeError, 'binding'):
                delivery.bind_service_candidate(attested, scope, 'source-hint')
            attested.service_protocol = True
            with self.assertRaisesRegex(RuntimeError, 'protocol'):
                delivery.bind_service_candidate(attested, scope, 'actual-binding')
        finally:
            sys.path[:] = saved_path


if __name__ == '__main__':
    unittest.main()

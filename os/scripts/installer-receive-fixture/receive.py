#!/usr/bin/python3
"""Fixed installed first-use qualifier; all assertions run on normal action doors."""
import argparse
import array
import socket
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pwd
import stat
import sys
import time

# -I intentionally omits the script directory. Only this root-owned, measured
# fixture directory is imported; no current-directory or caller module search.
HERE = Path(__file__).resolve().parent
_UNKNOWN_NAMESPACES = []
for directory in (HERE, HERE.parent, HERE.parent.parent):
    facts = directory.stat(follow_symlinks=False)
    if facts.st_uid != 0 or facts.st_mode & 0o022 or not stat.S_ISDIR(facts.st_mode):
        raise RuntimeError('fixed receiving script ancestry is not root controlled')
sys.path.insert(0, str(HERE))
import delivery
import firstuse
import namespace
from semantic import Control, bounded_command


def load_source():
    path = HERE.parent / 'installer-source-fixture/source.py'
    specification = importlib.util.spec_from_file_location('qualified_original_source', path)
    source = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(source)
    return source


def hash_file(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_uid != 0 or before.st_mode & 0o022:
            raise RuntimeError('candidate role is not an actual protected regular file')
        if not 0 < before.st_size <= 512 * 1024 * 1024:
            raise RuntimeError('candidate role size exceeds qualification bound')
        digest = hashlib.sha256()
        total = 0
        while total < before.st_size:
            value = os.read(fd, min(65536, before.st_size - total))
            if not value:
                raise RuntimeError('candidate role shortened during observation')
            total += len(value)
            digest.update(value)
        after = os.fstat(fd)
        named = os.stat(path, follow_symlinks=False)
        def stamp(info):
            return (info.st_dev, info.st_ino, info.st_size, info.st_uid, info.st_gid,
                    info.st_mode, info.st_mtime_ns, info.st_ctime_ns)
        if stamp(before) != stamp(after) or stamp(after) != stamp(named):
            raise RuntimeError('candidate role changed during observation')
        return digest.hexdigest()
    finally:
        os.close(fd)


def verify_roles(candidate):
    subjects_path = candidate / 'subjects.json'
    descriptor = os.open(subjects_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(descriptor)
        if (not stat.S_ISREG(before.st_mode) or before.st_uid != 0 or before.st_mode & 0o022
                or not 0 < before.st_size <= 16384):
            raise RuntimeError('candidate subject ledger is not bounded protected data')
        value = os.read(descriptor, before.st_size + 1)
        after = os.fstat(descriptor)
        if (len(value) != before.st_size or (before.st_dev, before.st_ino, before.st_size,
                before.st_mtime_ns, before.st_ctime_ns) != (after.st_dev, after.st_ino,
                after.st_size, after.st_mtime_ns, after.st_ctime_ns)):
            raise RuntimeError('candidate subject ledger changed while read')
        def closed_pairs(pairs):
            output = {}
            for key, item in pairs:
                if key in output:
                    raise RuntimeError('candidate subject ledger duplicates a key')
                output[key] = item
            return output
        subjects = json.loads(value, object_pairs_hook=closed_pairs)
    finally:
        os.close(descriptor)
    bindings = {'shell': '/usr/bin/shell', 'fs': '/usr/bin/fs',
                'control': '/usr/libexec/tilo/tilo-control',
                'credential_helper': '/usr/libexec/tilo-account-credential',
                'prepared_helper': '/usr/libexec/tilo-prepared-setup',
                'home_seeds': '/usr/libexec/tilo-firstboot-home-seeds',
                'prepared_extract': '/usr/libexec/tilo-installer-prepared-setup-extract',
                'files': str(candidate / 'file-explorer'),
                'writer': str(candidate / 'word-processor'),
                'host': str(candidate / 'host/bin/tilo-imager')}
    for role, path in bindings.items():
        expected = subjects.get(role)
        if not isinstance(expected, dict) or hash_file(path) != expected.get('sha256'):
            raise RuntimeError('actual installed candidate role differs: ' + role)
    return subjects


def session(candidate, evidence):
    """Runs only as the real firstboot account, with its normal session bus."""
    import apps
    import cloud
    import packages
    if os.getuid() != 1000 or os.geteuid() != 1000:
        raise RuntimeError('receiving session is not the actual firstboot account')
    raw = sys.stdin.buffer.read(81921)
    if len(raw) > 81920:
        raise RuntimeError('private receiving session input exceeds bound')
    context = json.loads(raw)
    account = pwd.getpwuid(os.getuid())
    if context['account'] != account.pw_name or Path(os.environ['HOME']) != Path(account.pw_dir):
        raise RuntimeError('receiving NSS account differs from delivered setup')
    deadline = time.monotonic() + 1200
    evidence.mkdir(mode=0o700)
    with (evidence / 'session.log').open('xb') as log:
        services = namespace.Services(log)
        result = None
        try:
            secret_fd = os.memfd_create('receiving-keyring-password', os.MFD_CLOEXEC)
            try:
                os.write(secret_fd, context['password'].encode())
                os.lseek(secret_fd, 0, os.SEEK_SET)
                services.spawn(['/usr/bin/gnome-keyring-daemon', '--foreground',
                                '--components=secrets', '--unlock'], stdin=secret_fd)
            finally:
                os.close(secret_fd)
            package_evidence = packages.install(candidate, Path(account.pw_dir), log, deadline)
            shell = services.spawn(['/usr/bin/shell', '--backend', 'headless',
                                    '--headless-owner-control'])
            control_path = Path(os.environ['XDG_RUNTIME_DIR']) / 'tilo-session/headless-owner-control.sock'
            while not control_path.exists():
                services.check_live()
                if time.monotonic() >= deadline:
                    raise RuntimeError('real delivery-bound first-use socket did not publish')
                time.sleep(0.05)
            control = Control(control_path, deadline, log)
            services.answer_authentication = lambda: control.answer_authentication(
                account.pw_name, context['password'])
            observations = firstuse.complete(control, account.pw_name, context['password'],
                                             evidence / 'identity-backup', context['source_operation'],
                                             context['applications_stage'])
            service_evidence = None
            if context.get('native_services') is not None:
                import service_sessions
                service_evidence = service_sessions.verify(
                    control, candidate, account, services, log, deadline,
                    context['native_services'], context['service_subjects'])
            elif context.get('service_qualification') is True:
                raise RuntimeError('service qualifier lacks native authenticated selected sessions')
            app_evidence = apps.verify(control, candidate, account, context['source_observations'],
                                       services, log, deadline)
            cloud_evidence = cloud.verify(control, account, context['dav'], services, log, deadline)
            # Each verifier returns its actual evidence, never a caller-supplied
            # readiness flag. The root collector checks the resulting closed
            # completion only after this session and its services really stop.
            result = {'firstuse': observations, 'packages': package_evidence,
                      'apps': app_evidence, 'cloud': cloud_evidence}
            if service_evidence is not None:
                result['service_sessions'] = service_evidence
        finally:
            services.stop()
        (evidence / 'observations.json').write_text(json.dumps(result, separators=(',', ':')))
        if not services.retired:
            raise RuntimeError('receiving session service retirement is unknown')
    return result


def receiving(candidate, evidence, delivery_context):
    """Source callback: real source RW epoch has already retired; backing stays held."""
    context_fields = set(delivery_context)
    if delivery_context.get('service_qualification') is True:
        context_fields.remove('service_qualification')
        context_fields.remove('service_subjects')
        context_fields.remove('service_candidate')
        context_fields.remove('service_request')
    if context_fields != {'package_directory', 'password', 'source_operation',
                                 'applications_stage', 'source', 'baseline_etc_fd',
                                 'dav', 'source_observations', 'tls_ca_certificate', 'tls_server_name',
                                 'selected_service_kinds', 'setup_binding'}:
        raise RuntimeError('actual source callback context shape differs')
    source = delivery_context['source']
    source.verify()
    if not source.source_epoch_retired:
        raise RuntimeError('Apps reader cannot begin while the source RW epoch is live')
    specification = importlib.util.spec_from_file_location(
        'receiving_owned_gpt', HERE.parent / 'installer-source-fixture/owned_gpt.py')
    owned_gpt = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(owned_gpt)
    child = None
    completed = False
    receiving_namespace = None
    namespace_retired = False
    def observed_target_retirement():
        if child is None or not completed:
            raise RuntimeError('actual receiving child completion and reap have not been observed')
        if namespace_retired:
            owned_gpt.assert_children_retired()
            return
        if receiving_namespace is None:
            raise RuntimeError('actual receiving namespace witness is missing')
        held = os.fstat(receiving_namespace)
        processes = list(Path('/proc').iterdir())
        if len(processes) > 32768:
            raise RuntimeError('receiving namespace process observation exceeds bound')
        for process in processes:
            if not process.name.isdecimal():
                continue
            try:
                current = (process / 'ns/mnt').stat()
            except FileNotFoundError:
                continue
            if (current.st_dev, current.st_ino) == (held.st_dev, held.st_ino):
                raise RuntimeError('receiving namespace still has a live process')
        owned_gpt.assert_children_retired()
    try:
        with (evidence / 'native-staging-authentication.log').open('xb') as log:
            attested = delivery.authenticated_staging_plan(
                delivery_context['package_directory'], delivery_context['password'],
                source, evidence, log)
        if delivery_context.get('service_qualification') is True:
            delivery.bind_service_candidate(attested, delivery_context['service_candidate'],
                                            delivery_context['setup_binding'])
        delivery_context = dict(delivery_context, native_services=attested.native_services)
        if (attested.scope['source_operation'] != delivery_context['source_operation']
                or attested.scope['staging_operation'] != delivery_context['applications_stage']):
            raise RuntimeError('actual staging scope differs from the source callback')
        with source.prepare_receiving_extent(attested, evidence, observed_target_retirement) as target:
            parent_control, child_control = socket.socketpair()
            parent_control.settimeout(30)
            pid = os.fork()
            child = pid
            if pid == 0:
                parent_control.close()
                try:
                    target_child(candidate, evidence, delivery_context, target, child_control)
                except BaseException as error:
                    # Private owned diagnostics, never a success object or secret.
                    with (evidence / 'receiving-refusal.log').open('ab') as log:
                        log.write((type(error).__name__ + ': ' + str(error) + '\n').encode())
                    os._exit(1)
                os._exit(0)
            child_control.close()
            message, ancillary, flags, _ = parent_control.recvmsg(1, socket.CMSG_SPACE(4))
            descriptors = array.array('i')
            if message != b'N' or flags or len(ancillary) != 1:
                raise RuntimeError('owned target namespace bootstrap refused')
            level, kind, values = ancillary[0]
            if level != socket.SOL_SOCKET or kind != socket.SCM_RIGHTS:
                raise RuntimeError('owned target namespace descriptor missing')
            descriptors.frombytes(values)
            if len(descriptors) != 1:
                raise RuntimeError('owned target namespace descriptor count differs')
            receiving_namespace = descriptors[0]
            actual = Path(f'/proc/{pid}/ns/mnt').stat()
            held = os.fstat(receiving_namespace)
            if (actual.st_dev, actual.st_ino) != (held.st_dev, held.st_ino):
                raise RuntimeError('target namespace witness differs from the actual owned child')
            parent_control.sendall(b'N')
            parent_control.close()
            # Keep the exact child owned until actual wait. The outer qualifier's
            # watchdog owns hard failure; this parent never claims a retired
            # target on a timeout, failed wait or an unsuccessful target child.
            deadline = time.monotonic() + 1500
            while time.monotonic() < deadline:
                observed, status = os.waitpid(pid, os.WNOHANG)
                if observed:
                    if not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 0:
                        raise RuntimeError('actual receiving namespace refused the full journey')
                    completed = True
                    break
                source.verify()
                time.sleep(0.05)
            if not completed:
                raise RuntimeError('receiving child retirement remains pending')
            result = json.loads((evidence / 'completed-target.json').read_bytes())
            observed_target_retirement()
            namespace_retired = True
            result['receiving_namespace_retired'] = True
            os.close(receiving_namespace)
            receiving_namespace = None
        result['target_physical_retired'] = True
        return result
    finally:
        if receiving_namespace is not None:
            # On unknown retirement keep the held namespace alive until the
            # outer owned PID-namespace collector observes termination.
            _UNKNOWN_NAMESPACES.append(receiving_namespace)


def target_child(candidate, evidence, delivery_context, target, parent_control):
    source = delivery_context['source']
    package_directory = delivery_context['package_directory']
    password = delivery_context['password']
    source_operation = delivery_context['source_operation']
    applications_stage = delivery_context['applications_stage']
    baseline_etc_fd = delivery_context['baseline_etc_fd']
    dav = delivery_context['dav']
    source_observations = delivery_context['source_observations']
    backing = evidence / 'target-namespace'
    backing.mkdir(mode=0o700)
    target_var = namespace.enter(target, baseline_etc_fd, backing)
    current_namespace = os.open('/proc/self/ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
    try:
        parent_control.sendmsg([b'N'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS,
                                        array.array('i', [current_namespace]))])
        if parent_control.recv(1) != b'N':
            raise RuntimeError('actual namespace owner did not acknowledge custody')
    finally:
        os.close(current_namespace)
        parent_control.close()
    with (evidence / 'receiving-system.log').open('xb') as log:
        services = namespace.Services(log)
        try:
            certificate = delivery_context['tls_ca_certificate']
            if (not isinstance(certificate, bytes) or not 1 <= len(certificate) <= 65536
                    or not certificate.startswith(b'-----BEGIN CERTIFICATE-----')):
                raise RuntimeError('actual owned DAV CA certificate is unavailable')
            import re
            server_name = delivery_context['tls_server_name']
            if (not isinstance(server_name, str) or len(server_name) > 253
                    or re.fullmatch(r'[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?\.test', server_name) is None
                    or '..' in server_name):
                raise RuntimeError('actual owned DAV TLS name is not a disposable fixture domain')
            hosts = Path('/etc/hosts')
            with hosts.open('ab') as output:
                output.write(('\n127.0.0.1 ' + server_name + '\n').encode())
            anchor = Path('/etc/pki/ca-trust/source/anchors/tilo-native-owned-dav.pem')
            delivery.private_write(anchor, certificate)
            os.chmod(anchor, 0o644)
            bounded_command(['/usr/bin/update-ca-trust', 'extract'], time.monotonic() + 30, log)
            services.start_system((source.disk, source.partition, target.partition),
                                  time.monotonic() + 45)
            reviewed, account = delivery.prepare(package_directory, password, target_var, log)
            if delivery_context.get('service_qualification') is True:
                import service_contract
                service_contract.install_native_scope(delivery_context['service_request'],
                    'receiver', Path(delivery.PREPARED), log, authenticated=reviewed)
            pending = reviewed['pending_computer_applications_scope']
            if (pending['source_operation'] != source_operation
                    or pending['staging_operation'] != applications_stage):
                raise RuntimeError('native admitted declaration differs from actual source operation')
            bounded_command(['/usr/bin/systemd-tmpfiles', '--create', '/usr/lib/tmpfiles.d/tilo.conf'],
                            time.monotonic() + 30, log)
            runtime = Path('/run/user') / str(account.pw_uid)
            runtime.mkdir(mode=0o700, parents=True)
            os.chown(runtime, account.pw_uid, account.pw_gid)
            # The immutable measured candidate stays root owned; the login may
            # read it through an independently private receiving mount alias.
            public_candidate = Path('/run/tilo-receiving-candidate')
            # A bind of the original 0700 candidate would stay unreadable to
            # the login. Clone only the already measured public build inputs;
            # source password, receipts and private logs never live here.
            import shutil
            shutil.copytree(candidate, public_candidate, symlinks=True)
            for directory, children, files in os.walk(public_candidate, followlinks=False):
                os.chmod(directory, 0o755)
                for name in files:
                    path = Path(directory) / name
                    facts = path.stat(follow_symlinks=False)
                    if not stat.S_ISREG(facts.st_mode):
                        raise RuntimeError('candidate public clone contains a nonregular input')
                    os.chmod(path, 0o555 if facts.st_mode & 0o111 else 0o444)
            verify_roles(public_candidate)
            import packages
            packages.enroll_runtime(public_candidate, log, time.monotonic() + 90)
            user_evidence = Path(account.pw_dir) / '.receiving-evidence'
            environment = {'PATH': '/usr/bin:/usr/sbin', 'LC_ALL': 'C.UTF-8',
                           'HOME': account.pw_dir, 'USER': account.pw_name, 'LOGNAME': account.pw_name,
                           'XDG_RUNTIME_DIR': str(runtime),
                           'XDG_CONFIG_HOME': str(Path(account.pw_dir) / '.config'),
                           'XDG_DATA_HOME': str(Path(account.pw_dir) / '.local/share'),
                           'XDG_STATE_HOME': str(Path(account.pw_dir) / '.local/state')}
            context = {'account': account.pw_name, 'password': password.decode(),
                       'source_operation': source_operation, 'applications_stage': applications_stage,
                       'source_observations': source_observations, 'dav': dav,
                       'native_services': delivery_context['native_services'],
                       'service_qualification': delivery_context.get('service_qualification', False),
                       'service_subjects': delivery_context.get('service_subjects')}
            private = os.memfd_create('actual-receiving-firstboot-session', os.MFD_CLOEXEC)
            try:
                value = json.dumps(context, separators=(',', ':')).encode()
                if len(value) > 81920:
                    raise RuntimeError('private receiving context exceeds bound')
                os.write(private, value)
                os.lseek(private, 0, os.SEEK_SET)
                user_child = services.spawn([
                    '/usr/bin/runuser', '--user', account.pw_name, '--',
                    '/usr/bin/dbus-run-session', '--', '/usr/bin/python3', '-I',
                    str(public_candidate / 'os/scripts/installer-receive-fixture/receive.py'),
                    '--session', '--candidate', str(public_candidate), '--evidence', str(user_evidence)],
                    environment=environment, stdin=private)
            finally:
                os.close(private)
            until = time.monotonic() + 1250
            fs_started = False
            while time.monotonic() < until:
                # Primary portable identity creates the certified machine only
                # through the actual prepared.import ceremony. Match installed
                # fs.service's cert condition; do not start an identity-less fs.
                if not fs_started and Path('/var/lib/tilo/device/cert.bin').is_file():
                    services.spawn(['/usr/bin/fs', 'serve', '--mount-point', '/var/workspace',
                                    '--cache-dir', '/var/cache/quick-fs', '--storage-sock',
                                    '/run/tilo/fs-storage/storage.sock', '--blob-sock', '/run/tilo/fs-blobs.sock',
                                    '--acl', '/var/lib/tilo/fs-capabilities.json', '--principal',
                                    'person:local', '--data-plane=fuse'])
                    fs_started = True
                if os.waitid(os.P_PID, user_child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT):
                    break
                time.sleep(0.05)
            else:
                raise RuntimeError('actual receiving user session deadline elapsed')
            # Leave leader unreaped until Services.stop retires its group.
            status = os.waitid(os.P_PID, user_child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            if status.si_code != os.CLD_EXITED or status.si_status != 0:
                raise RuntimeError('ordinary target first-use/app session refused')
            observations = json.loads((user_evidence / 'observations.json').read_bytes())
        finally:
            services.stop()
        # The verifiers must have observed each real operation, with no bool
        # accepted merely from an input or the encrypted package metadata.
        required_apps = ('shared_files_readwrite', 'writer_shared_file_edit', 'apps_data_and_launch')
        for key in required_apps:
            if observations['apps'].get(key) is not True:
                raise RuntimeError('actual application verification did not prove ' + key)
        cloud = observations['cloud']
        if not all(cloud.get(key) is True for key in
                   ('files_account_opened', 'cloud_fuse_mounted', 'local_key_retired',
                    'imported_ciphertext_preserved')):
            raise RuntimeError('actual default cloud custody and Files hydration remain unproven')
        if (not observations['firstuse']['rw_available']
                or observations['firstuse'].get('certified_owner_surface') is not True
                or not services.retired):
            raise RuntimeError('first-use migration, certified owner or services retirement is unproven')
        if observations['apps'].get('owned_resources_retired') is not True:
            raise RuntimeError('actual Files/Writer/root helper retirement is unproven')
        # Internal evidence is deliberately not a full completion object.
        # The still-live outer DAV server must observe the exact upload, and
        # the physical owners must finish before main emits the closed result.
        result = {'apps': observations['apps'], 'cloud': cloud,
                  'firstuse': observations['firstuse'], 'receiving_services_retired': True}
        if delivery_context['native_services'] is not None:
            service_evidence = observations.get('service_sessions')
            if not isinstance(service_evidence, dict):
                raise RuntimeError('actual receiving service observations are missing')
            result['service_sessions'] = service_evidence
        (evidence / 'completed-target.json').write_text(json.dumps(result, separators=(',', ':')))


def main():
    arguments = argparse.ArgumentParser()
    arguments.add_argument('--candidate', type=Path, required=True)
    arguments.add_argument('--evidence', type=Path, required=True)
    arguments.add_argument('--session', action='store_true')
    arguments.add_argument('--service-session-qualification', action='store_true')
    args = arguments.parse_args()
    if args.session:
        if args.service_session_qualification:
            raise RuntimeError('service request belongs only to the owned root coordinator')
        session(args.candidate, args.evidence)
        return
    if os.getuid() != 0 or os.geteuid() != 0:
        raise RuntimeError('full receiving fixture requires the actual owned root namespace')
    verify_roles(args.candidate)
    service_admission = None
    if args.service_session_qualification:
        import service_contract
        raw = sys.stdin.buffer.read(16 * 1024 + 1)
        # No source/physical work begins before actual artifact admission.
        evidence_info = args.evidence.lstat()
        if (not stat.S_ISDIR(evidence_info.st_mode) or evidence_info.st_uid != 0
                or stat.S_IMODE(evidence_info.st_mode) != 0o700
                or args.evidence.resolve(strict=True) != args.evidence):
            raise RuntimeError('service evidence is not the actual owned private directory')
        with (args.evidence / 'service-candidate-admission.log').open('xb') as log:
            service_admission = service_contract.admit_request(
                raw, args.candidate, hash_file, log, time.monotonic() + 90)
    source = load_source()
    def deliver(context):
        if service_admission is not None:
            context = dict(context, service_qualification=True,
                           service_subjects=service_admission['subjects'],
                           service_request=service_admission['request'])
        return receiving(args.candidate, args.evidence, context)
    if service_admission is None:
        result = source.run(args.candidate, args.evidence, deliver)
    else:
        result = source.run(args.candidate, args.evidence, deliver, service_qualification=True,
                            service_candidate=service_contract.candidate_scope(service_admission),
                            service_request=service_admission['request'])
    if (result['cloud'].get('server_upload_observed') is not True
            or result.get('receiving_namespace_retired') is not True
            or result.get('receiving_services_retired') is not True
            or result.get('source_physical_retired') is not True
            or result.get('target_physical_retired') is not True):
        raise RuntimeError('outer server or owned physical retirement remains unproven')
    if service_admission is not None:
        print(json.dumps(service_contract.complete(service_admission, result), separators=(',', ':')))
        return
    completed = {'schema': 1, 'status': 'completed',
                 'shared_files_readwrite': result['apps']['shared_files_readwrite'],
                 'writer_shared_file_edit': result['apps']['writer_shared_file_edit'],
                 'apps_data_and_launch': result['apps']['apps_data_and_launch'],
                 'protected_cloud_custody': True, 'cloud_fs_and_files': True,
                 'identity_headless_lifecycle': result['firstuse']['certified_owner_surface'],
                 'owned_resources_retired': True}
    print(json.dumps(completed, separators=(',', ':')))


if __name__ == '__main__':
    main()

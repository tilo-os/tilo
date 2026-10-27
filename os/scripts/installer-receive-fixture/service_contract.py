"""Closed artifact/request bridge for the existing owned receiving coordinator.

Inspection authenticates actual installed candidate subjects. It is not a
consumer capability and cannot be saved as one before real route observations
and all existing outer retirement checks complete.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import time
import tomllib

# The compiled account consumer producer runs this bridge directly with -I,
# which omits the script directory: import only this fixed fixture directory.
HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))
from semantic import bounded_command  # noqa: E402

KEYS = {'schema', 'root', 'shell_path', 'sdk_runtime_version', 'sdk_runtime_root_hash',
        'browser_package_hash', 'browser_publisher', 'cef_runtime_version',
        'cef_runtime_root_hash', 'cef_runtime_publisher', 'required_protocol',
        'receiving_shell_sha256', 'receiving_entry_sha256', 'receiving_dependencies',
        'receiving_python_path', 'receiving_python_sha256',
        'receiving_route_qualification_sha256'}
SHA_FIELDS = {'sdk_runtime_root_hash', 'browser_package_hash', 'browser_publisher',
              'cef_runtime_root_hash', 'cef_runtime_publisher', 'receiving_shell_sha256',
              'receiving_entry_sha256', 'receiving_python_sha256',
              'receiving_route_qualification_sha256'}
CONDITIONS = {'private_list_verified', 'private_resolve_verified', 'isolated_profile_verified',
              'revocation_verified', 'no_vault_mount_verified', 'origin_storage_persisted',
              'chat_destination_navigation_verified'}


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def unique(pairs):
    result = {}
    for name, value in pairs:
        require(name not in result, 'duplicate receiving service request field')
        result[name] = value
    return result


def decode(raw):
    require(type(raw) is bytes and 0 < len(raw) <= 16 * 1024, 'service request exceeds bound')
    value = json.loads(raw, object_pairs_hook=unique)
    require(type(value) is dict and set(value) == KEYS and type(value['schema']) is int
            and value['schema'] == 2 and type(value['required_protocol']) is int
            and value['required_protocol'] == 2 and value['root'] == '/'
            and value['shell_path'] in ('/usr/bin/shell', '/usr/bin/tilo-shell'),
            'service request is not the fixed actual receiving protocol')
    require(all(type(value[name]) is str and re.fullmatch('[0-9a-f]{64}', value[name])
                for name in SHA_FIELDS), 'service request artifact identity differs')
    for name in ('sdk_runtime_version', 'cef_runtime_version'):
        require(type(value[name]) is str
                and re.fullmatch('[A-Za-z0-9][A-Za-z0-9._+-]{0,255}', value[name]),
                'service runtime version is not canonical')
    require(value['cef_runtime_version'].endswith('-chromium'), 'service CEF runtime differs')
    require(type(value['receiving_dependencies']) is dict
            and 0 < len(value['receiving_dependencies']) <= 20
            and all(type(key) is str and type(item) is str
                    and re.fullmatch('[0-9a-f]{64}', item)
                    for key, item in value['receiving_dependencies'].items()),
            'service dependency identity table differs')
    require(type(value['receiving_python_path']) is str
            and re.fullmatch(r'/usr/bin/python3(?:\.[0-9]+)?', value['receiving_python_path']),
            'service interpreter path differs')
    return value


RECEIVING_ENTRY = '/usr/libexec/tilo-installer-receive-fixture/receive.py'
# The measured receiving closure; the installed producer
# (crates/platform/imager-backend, RECEIVING_DEPENDENCIES) binds the same list.
RECEIVING_DEPENDENCIES = tuple(
    '/usr/libexec/tilo-installer-receive-fixture/' + name + '.py'
    for name in ('receive', 'semantic', 'firstuse', 'delivery', 'namespace',
                 'packages', 'apps', 'cloud', 'service_sessions', 'service_contract', 'service_processes')) + tuple(
    '/usr/libexec/tilo-installer-source-fixture/' + name + '.py'
    for name in ('source', 'owned_gpt', 'bounded_native', 'flatpak_material', 'source_sessions'))


def selected(root):
    """The signed receiving Browser and CEF runtime in the canonical image store."""
    registry = root / 'usr/share/tilo-apps/registry.toml'
    if not registry.exists():
        return None
    entries = tomllib.loads(registry.read_text()).get('entry', [])
    browser = [entry for entry in entries if entry.get('id') == 'so.tilo.browser']
    require(len(browser) <= 1, 'ambiguous receiving Browser package')
    if not browser:
        return None
    row = browser[0]
    require(all(type(row.get(name)) is str and re.fullmatch('[0-9a-f]{64}', row[name])
                for name in ('package_id_hex', 'publisher_key_hex')),
            'Browser package has no signed identity')
    payload = '/usr/share/tilo-apps/pkgs/' + row['package_id_hex']
    metadata = payload + '.pkg/manifest.toml'
    require(row.get('payload_root') == payload and row.get('manifest_path') == metadata,
            'Browser package does not use the canonical image store')
    manifest = tomllib.loads((root / metadata.removeprefix('/')).read_text())
    package = manifest.get('package', {})
    require(package.get('id') == 'so.tilo.browser', 'receiving manifest is not Browser')
    engines = [value.removeprefix('tilo-runtime/') for value in package.get('optional-runtimes', [])
               if isinstance(value, str) and value.startswith('tilo-runtime/') and value.endswith('-chromium')]
    require(len(engines) == 1, 'Browser must name one exact receiving CEF runtime')
    version = engines[0]
    require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,255}', version), 'unsafe receiving CEF version')
    bundle = root / 'usr/lib/tilo-runtime' / version
    if not bundle.exists():
        return None
    require(manifest.get('entrypoint', {}).get('bin') == 'bin/browser',
            'receiving Browser executable differs from canonical entrypoint')
    return row, package, root / payload.removeprefix('/') / 'bin/browser', version, bundle


def route_artifacts(root, shell_path, digest, regular):
    """Independently remeasure the receiving Shell and Python closure."""
    require(shell_path in ('/usr/bin/shell', '/usr/bin/tilo-shell'),
            'receiving Shell is not a canonical packaged receiver')
    shell = root / shell_path.removeprefix('/')
    require(regular(shell, 256 * 1024 * 1024).st_mode & 0o111, 'receiving Shell is not executable')
    dependencies = {relative: digest(root / relative.removeprefix('/'), 1024 * 1024)
                    for relative in RECEIVING_DEPENDENCIES}
    python = (root / 'usr/bin/python3').resolve(strict=True)
    require(python.parent == root / 'usr/bin'
            and re.fullmatch(r'python3(?:\.[0-9]+)?', python.name) is not None,
            'receiving Python escapes the canonical image interpreter identity')
    require(regular(python, 256 * 1024 * 1024).st_mode & 0o111, 'receiving Python is not executable')
    facts = {'receiving_shell_sha256': digest(shell, 256 * 1024 * 1024),
             'receiving_entry_sha256': dependencies[RECEIVING_ENTRY],
             'receiving_dependencies': dependencies,
             'receiving_python_path': '/' + str(python.relative_to(root)),
             'receiving_python_sha256': digest(python, 256 * 1024 * 1024)}
    encoded = json.dumps(facts, sort_keys=True, separators=(',', ':')).encode()
    facts['receiving_route_qualification_sha256'] = hashlib.sha256(
        b'tilo-installed-service-receiver-python-v1\0' + encoded).hexdigest()
    return facts


def owned_bridge(command, env, raw):
    """Execute only in the existing disposable owner's admitted layout.

    No image-only caller may construct a second source/receiving lifetime. The
    initial namespace handle and private evidence belong to the outer owner.
    """
    request = decode(raw)
    require(os.getuid() == os.geteuid() == 0, 'service bridge requires owned root')
    require(command == [request['receiving_python_path'], '-I',
                        '/usr/libexec/tilo-installer-receive-fixture/receive.py',
                        '--service-session-qualification']
            and env == {'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C'},
            'service bridge is not the canonical owned coordinator')
    for path, mode in ((Path('/owned'), None), (Path('/owned/candidate'), None),
                       (Path('/evidence'), None), (Path('/evidence/owned'), 0o700)):
        info = path.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == 0
                and not info.st_mode & 0o022 and path.resolve(strict=True) == path
                and (mode is None or stat.S_IMODE(info.st_mode) == mode),
                'service bridge lacks protected owned candidate/evidence')
    descriptor = os.open('/owned/initial-pidns', os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        initial = os.fstat(descriptor)
        require(initial.st_uid == 0 and initial.st_ino != Path('/proc/self/ns/pid').stat().st_ino
                and fcntl.ioctl(descriptor, 0xb703) == 0x20000000,  # NS_GET_NSTYPE / CLONE_NEWPID
                'service bridge lacks the outer initial PID namespace witness')
        with Path('/evidence/owned/service-bridge.log').open('xb') as log:
            os.fchmod(log.fileno(), 0o600)
            return bounded_command([*command, '--candidate', '/owned/candidate',
                                    '--evidence', '/evidence/owned'],
                                   time.monotonic() + 2800, log, raw, 4096)
    finally:
        os.close(descriptor)


def admit_request(raw, candidate, hash_file, log, deadline):
    require(os.getuid() == os.geteuid() == 0, 'service artifact admission requires owned root')
    require(Path(candidate) == Path('/owned/candidate')
            and Path(candidate).resolve(strict=True) == Path(candidate),
            'service admission is not the exact owned candidate')
    request = decode(raw)
    def regular(path, maximum):
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
        try:
            info = os.fstat(fd)
            require(stat.S_ISREG(info.st_mode) and info.st_uid == 0
                    and not info.st_mode & 0o022 and 0 < info.st_size <= maximum,
                    'service subject is not bounded protected data')
            return info
        finally:
            os.close(fd)
    def digest(path, maximum):
        regular(path, maximum)
        return hash_file(path)
    facts = route_artifacts(Path('/'), request['shell_path'], digest, regular)
    require(all(request[name] == item for name, item in facts.items()),
            'actual installed Shell/Python/receiver differs from bound request')
    receiving = selected(Path('/'))
    require(receiving is not None, 'actual signed receiving Browser/CEF is absent')
    row, package, browser, version, cef = receiving
    require(row['package_id_hex'] == request['browser_package_hash']
            and row['publisher_key_hex'] == request['browser_publisher']
            and version == request['cef_runtime_version']
            and package.get('runtime') == 'tilo-runtime/' + request['sdk_runtime_version'],
            'selected signed receiving graph differs from request')
    bounded_command(['/usr/bin/env', 'TILO_APPS_DATA_DIR=/usr/share/tilo-apps',
                     '/usr/libexec/tilo/tilo-install', 'verify', 'so.tilo.browser'], deadline, log)
    for kind, bundle, publisher in (
            ('cef', cef, request['cef_runtime_publisher']),
            ('sdk', Path('/usr/lib/tilo-runtime') / request['sdk_runtime_version'], None)):
        if publisher is None:
            markers = Path('/usr/share/tilo')
            chosen = markers / 'account-consumer-runtime-publisher.hex'
            if not chosen.exists():
                chosen = markers / 'runtime-bundle-publisher.hex'
            regular(chosen, 1024)
            publisher = chosen.read_text().strip()
            require(re.fullmatch('[0-9a-f]{64}', publisher), 'installed SDK publisher differs')
        bounded_command(['/usr/libexec/tilo/tilo-runtime-bundle', 'verify', str(bundle),
                         '--require-publisher', publisher, '--require-signature',
                         '--require-arch', os.uname().machine], deadline, log)
        metadata = bundle / 'bundle.toml'
        regular(metadata, 8 * 1024 * 1024)
        with metadata.open('rb') as stream:
            raw_metadata = stream.read(8 * 1024 * 1024 + 1)
        require(len(raw_metadata) <= 8 * 1024 * 1024, 'runtime metadata exceeds bound')
        actual = tomllib.loads(raw_metadata.decode())
        require(actual['root_hash'] == request[kind + '_runtime_root_hash']
                and actual['version'] == request[kind + '_runtime_version'],
                'actual verified runtime differs from candidate')
    # Kernel observation later binds the actual launched executables to these
    # authenticated subjects, not executable basenames or user-owned PID files.
    subjects = {'browser_sha256': hash_file(browser),
                'cef_sha256': hash_file(cef / 'lib/chromium/tilo-chromium-host'),
                'browser_bytes': regular(browser, 256 * 1024 * 1024).st_size,
                'cef_bytes': regular(cef / 'lib/chromium/tilo-chromium-host', 256 * 1024 * 1024).st_size}
    require(route_artifacts(Path('/'), request['shell_path'], digest, regular) == facts,
            'receiver artifacts changed during candidate inspection')
    return {'request': request, 'subjects': subjects}


def install_native_scope(request, phase, actor, log, *, authenticated=None):
    """Root-only temporary door in this owner's isolated source/target namespace.

    Signed artifacts are replayed here, never replaced with a capability receipt.
    The receiving person/binding comes ONLY from native password authentication.
    """
    require(os.getuid() == os.geteuid() == 0, 'native scope requires owned root')
    require(phase in ('source', 'receiver'), 'native scope phase differs')
    initial = os.open('/owned/initial-pidns', os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        require(os.fstat(initial).st_uid == 0
                and fcntl.ioctl(initial, 0xb703) == 0x20000000
                and os.fstat(initial).st_ino != Path('/proc/self/ns/pid').stat().st_ino,
                'native scope is not the disposable owned PID namespace')
        witness = Path('/run/tilo-service-initial-pidns')
        descriptor = os.open(witness, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o444)
        os.close(descriptor)
        bounded_command(['/usr/bin/mount', '--bind', '/owned/initial-pidns', str(witness)],
                        time.monotonic() + 30, log)
        require(witness.stat().st_ino == os.fstat(initial).st_ino,
                'native initial namespace witness changed')
    finally:
        os.close(initial)
    import hashlib
    def hash_file(path):
        digest = hashlib.sha256()
        with Path(path).open('rb') as stream:
            for part in iter(lambda: stream.read(65536), b''):
                digest.update(part)
        return digest.hexdigest()
    admission = admit_request(json.dumps(request, separators=(',', ':')).encode(),
                              Path('/owned/candidate'), hash_file, log, time.monotonic() + 90)
    actor = Path(actor)
    expected = (Path('/run/tilo-source-public/host/bin/tilo-imager') if phase == 'source'
                else Path('/usr/libexec/tilo-prepared-setup'))
    require(actor == expected, 'native scope actor is not the fixed candidate role')
    for edge in (actor, *actor.parents):
        info = edge.lstat()
        require(info.st_uid == 0 and not info.st_mode & 0o022
                and not stat.S_ISLNK(info.st_mode), 'native scope actor is mutable')
    info = actor.lstat()
    require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= 1024 * 1024 * 1024,
            'native scope actor is not bounded regular data')
    ledger_path = Path('/owned/candidate/subjects.json')
    info = ledger_path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022
            and 0 < info.st_size <= 16384, 'native scope ledger differs from protected candidate')
    ledger = json.loads(ledger_path.read_bytes(), object_pairs_hook=unique)
    role = 'host' if phase == 'source' else 'prepared_helper'
    digest = hash_file(actor)
    require(digest == ledger[role]['sha256'], 'native scope actor differs from admitted candidate')
    person = binding = None
    if phase == 'receiver':
        require(type(authenticated) is dict, 'receiving scope lacks native authentication')
        person, binding = authenticated.get('identity_public_key'), authenticated.get('setup_binding')
        require(type(person) is list and len(person) == 32
                and all(type(byte) is int and 0 <= byte <= 255 for byte in person)
                and any(person) and type(binding) is str and 0 < len(binding.encode()) <= 256
                and all(ord(char) >= 32 and ord(char) != 127 for char in binding), 'native person binding differs')
    else:
        require(authenticated is None, 'source cannot substitute receiving person authority')
    marker = dict(schema=1, phase=phase,
                  mount_namespace=Path('/proc/self/ns/mnt').stat().st_ino,
                  pid_namespace=Path('/proc/self/ns/pid').stat().st_ino,
                  actor_path=str(actor), actor_sha256=digest,
                  scope=candidate_scope(admission), identity_public_key=person, setup_binding=binding)
    path = Path('/run/tilo-service-candidate-admission.json')
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o444)
    try:
        os.fchmod(descriptor, 0o444)
        raw = json.dumps(marker, separators=(',', ':')).encode()
        with os.fdopen(descriptor, 'wb', closefd=False) as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
    finally:
        os.close(descriptor)
    return path


def candidate_scope(admission):
    """Authenticated artifact scope only; deliberately not a ConsumerCapability."""
    request = decode(json.dumps(admission['request'], separators=(',', ':')).encode())
    return {'required_protocol': request['required_protocol'],
            'browser_package_hash': request['browser_package_hash'],
            'cef_runtime_root_hash': request['cef_runtime_root_hash'],
            'sdk_runtime_root_hash': request['sdk_runtime_root_hash'],
            'receiving_route_qualification_sha256': request['receiving_route_qualification_sha256']}


def complete(admission, result):
    # The caller reaches this only after SAME source.run observes all original
    # server, source/target physical, namespace and Services retirements.
    for name in ('receiving_namespace_retired', 'receiving_services_retired',
                 'source_physical_retired', 'target_physical_retired'):
        require(result.get(name) is True, 'service outer lifetime remains unobserved')
    require(type(result.get('cloud')) is dict
            and result['cloud'].get('server_upload_observed') is True,
            'service live server observation remains unproven')
    service = result.get('service_sessions')
    require(type(service) is dict and type(service.get('schema')) is int
            and service['schema'] == 1
            and type(service.get('conditions')) is dict
            and set(service['conditions']) == CONDITIONS
            and all(type(value) is bool and value for value in service['conditions'].values()),
            'actual receiving service conditions remain unproven')
    # No flags are manufactured from generic receiving completion. The actual
    # dedicated observer must supply every mandatory condition independently.
    return dict(schema=2, service_session_protocol=2,
                receiving_route_qualification_sha256=admission['request']['receiving_route_qualification_sha256'],
                **service['conditions'])


if __name__ == '__main__':
    # The compiled account consumer producer's route qualifier: stdin carries
    # the bound request, the remaining arguments name the canonical coordinator.
    require(sys.argv[1:2] == ['--owned-bridge'], 'service bridge accepts only --owned-bridge')
    request_bytes = sys.stdin.buffer.read(16 * 1024 + 1)
    sys.stdout.buffer.write(owned_bridge(sys.argv[2:], dict(os.environ), request_bytes))

"""Closed artifact/request bridge for the existing owned receiving coordinator.

Inspection authenticates actual installed candidate subjects. It is not a
consumer capability and cannot be saved as one before real route observations
and all existing outer retirement checks complete.
"""
import importlib.util
import fcntl
import json
import os
from pathlib import Path
import re
import stat
import time
import tomllib

from semantic import bounded_command

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
    require(type(raw) is bytes and 0 < len(raw) <= 4096, 'service request exceeds bound')
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


def module(path):
    # This fixed installed source, not a request path, participates in the
    # canonical image/receiver dependency ledger and protected image namespace.
    for edge in (path, *path.parents):
        info = edge.lstat()
        require(info.st_uid == 0 and not info.st_mode & 0o022
                and not stat.S_ISLNK(info.st_mode), 'service verifier is not image controlled')
    spec = importlib.util.spec_from_file_location('installed_service_verifier', path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


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
    verifier = module(Path('/usr/libexec/tilo-service-consumer-capability.py'))
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
    facts = verifier.route_artifacts(Path('/'), request['shell_path'],
                                      (lambda path, maximum: (regular(path, maximum), hash_file(path))[1],
                                       None, regular))
    require(all(request[name] == item for name, item in facts.items()),
            'actual installed Shell/Python/receiver differs from bound request')
    selected = verifier.selected(Path('/'))
    require(selected is not None, 'actual signed receiving Browser/CEF is absent')
    row, package, browser, version, cef = selected
    require(row['package_id_hex'] == request['browser_package_hash']
            and row['publisher_key_hex'] == request['browser_publisher']
            and version == request['cef_runtime_version']
            and package.get('runtime') == 'tilo-runtime/' + request['sdk_runtime_version'],
            'selected signed receiving graph differs from request')
    bounded_command(['/usr/bin/env', 'TILO_APPS_DATA_DIR=/usr/share/tilo-apps',
                     '/usr/bin/tilo-install', 'verify', 'so.tilo.browser'], deadline, log)
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
        bounded_command(['/usr/bin/tilo-runtime-bundle', 'verify', str(bundle),
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
    require(verifier.route_artifacts(Path('/'), request['shell_path'],
              (lambda path, maximum: (regular(path, maximum), hash_file(path))[1], None, regular)) == facts,
            'receiver artifacts changed during candidate inspection')
    return {'request': request, 'subjects': subjects}


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

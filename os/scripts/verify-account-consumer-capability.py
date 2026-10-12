#!/usr/bin/python3
"""Execute the installed account consumers before sealing the target rootfs.

Only the final image build runs main. Support is proven by the installed helper
and the SDK-linked probe from the verified signed native runtime, never by a
host environment variable or a declared image version.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import stat
import struct
import subprocess
import tempfile
import tomllib


def require(condition, message):
    if not condition:
        raise ValueError(message)


def regular(path, limit):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= limit,
            'capability artifact is not a bounded regular file: ' + str(path))
    return info


def digest(path, limit):
    regular(path, limit)
    value = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def invoke(command, env):
    completed = subprocess.run(command, env=env, check=True, capture_output=True, timeout=60)
    require(len(completed.stdout) <= 4096 and len(completed.stderr) <= 4096,
            'capability command output exceeded its contract')
    return completed.stdout


def unique(items):
    result = {}
    for key, value in items:
        require(key not in result, 'duplicate capability response key')
        result[key] = value
    return result


def protocol(output):
    value = json.loads(output, object_pairs_hook=unique)
    require(isinstance(value, dict) and set(value) == {'schema', 'protected_grant_protocol'}
            and type(value['schema']) is int and value['schema'] == 1
            and type(value['protected_grant_protocol']) is int
            and value['protected_grant_protocol'] == 1, 'installed account consumer lacks protocol 1')


def interpreter(probe):
    with probe.open('rb') as source:
        header = source.read(64)
        require(len(header) == 64 and header[:6] == b'\x7fELF\x02\x01',
                'capability probe is not a little-endian ELF64 binary')
        machine = struct.unpack_from('<H', header, 18)[0]
        require(machine == {'x86_64': 62, 'aarch64': 183}.get(platform.machine()),
                'capability probe architecture differs from target image')
        offset = struct.unpack_from('<Q', header, 32)[0]
        size, count = struct.unpack_from('<HH', header, 54)
        require(size == 56 and 0 < count <= 128, 'invalid probe ELF program table')
        matches = []
        for index in range(count):
            source.seek(offset + index * size)
            entry = source.read(size)
            require(len(entry) == size, 'truncated probe ELF program table')
            if struct.unpack_from('<I', entry)[0] != 3:
                continue
            start = struct.unpack_from('<Q', entry, 8)[0]
            length = struct.unpack_from('<Q', entry, 32)[0]
            require(1 < length <= 4096, 'invalid probe ELF interpreter')
            source.seek(start)
            name = source.read(length)
            require(len(name) == length and name[-1:] == b'\0' and b'\0' not in name[:-1],
                    'invalid probe ELF interpreter name')
            matches.append(name[:-1].decode('ascii'))
        require(len(matches) == 1, 'probe must have one canonical ELF interpreter')
        name = matches[0]
        require(name.startswith('/') and all(part not in ('', '.', '..') for part in name[1:].split('/')),
                'unsafe probe ELF interpreter path')
        return 'lib/' + name[1:]


def runtime_markers(share):
    # An installer overlay can add a certified account SDK beside the base's
    # app runtimes. It never retargets existing app manifests or primary SDK.
    account = ('account-consumer-runtime-version', 'account-consumer-runtime-publisher.hex')
    present = [os.path.lexists(share / name) for name in account]
    require(all(present) or not any(present), 'incomplete account runtime selection')
    return account if all(present) else ('runtime-version', 'runtime-bundle-publisher.hex')


def produce(root=Path('/'), runner=invoke, qualify_services=False, service_evidence=None):
    # root is an internal fixture/package-staging seam; the installed CLI has no path arguments.
    share = root / 'usr/share/tilo'
    version_name, publisher_name = runtime_markers(share)
    version = (share / version_name).read_text().strip()
    require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,255}', version) is not None,
            'invalid primary runtime version')
    publisher = (share / publisher_name).read_text().strip()
    require(re.fullmatch('[0-9a-f]{64}', publisher) is not None, 'invalid pinned runtime publisher')
    bundle = root / 'usr/lib/tilo-runtime' / version
    env = {'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C'}
    runner([str(root / 'usr/bin/tilo-runtime-bundle'), 'verify', str(bundle),
            '--require-publisher', publisher, '--require-signature',
            '--require-arch', platform.machine()], env)
    manifest = tomllib.loads((bundle / 'bundle.toml').read_text())
    require(manifest['version'] == version and re.fullmatch('[0-9a-f]{64}', manifest['root_hash']),
            'verified runtime identity differs from selected primary runtime')
    entries = {entry['path']: entry for entry in manifest['files']}
    require(len(entries) == len(manifest['files']), 'duplicate runtime inventory path')
    probe_name = 'bin/tilo-runtime-account-capability'
    library_name = 'lib/libruntime_dylib.so'
    probe = bundle / probe_name
    helper = root / 'usr/libexec/tilo-account-credential'
    for name in (probe_name, library_name):
        require(name in entries, 'account consumer is absent from signed runtime inventory')
    regular(probe, 64 * 1024 * 1024)
    loader_name = interpreter(probe)
    require(loader_name in entries, 'account probe interpreter is absent from signed inventory')
    loader = bundle / loader_name
    for path in (probe, helper, loader):
        require(regular(path, 64 * 1024 * 1024).st_mode & 0o111,
                'account consumer is not executable')
    # glibc's explicit loader prevents host cache or host SDK lookup. The
    # producer already verified every DT_NEEDED edge is in this signed lib/.
    protocol(runner([str(loader), '--inhibit-cache', '--library-path', str(bundle / 'lib'),
                     str(probe)], env))
    protocol(runner([str(helper), 'capabilities'], env))
    probe_digest = digest(probe, 64 * 1024 * 1024)
    library_digest = digest(bundle / library_name, 2 * 1024 * 1024 * 1024)
    runner([str(root / 'usr/bin/tilo-runtime-bundle'), 'verify', str(bundle),
            '--require-publisher', publisher, '--require-signature',
            '--require-arch', platform.machine()], env)
    receipt = {'schema': 1, 'protected_grant_protocol': 1,
            'credential_helper_sha256': digest(helper, 64 * 1024 * 1024),
            'runtime_root_hash': manifest['root_hash'],
            'runtime_probe_sha256': probe_digest, 'runtime_library_sha256': library_digest,
            'runtime_version': version, 'runtime_library': library_name}
    service = load_service_consumer()
    proof = service.produce(root, runner, (digest, interpreter, regular), receipt,
                            qualify=qualify_services, evidence=service_evidence)
    if proof is not None:
        receipt['service_sessions'] = proof
    return receipt


def load_service_consumer():
    module_name = 'service-consumer-capability.py' if Path(__file__).name == 'verify-account-consumer-capability.py' else 'tilo-service-consumer-capability.py'
    module_path = Path(__file__).with_name(module_name)
    regular(module_path, 1024 * 1024)
    spec = importlib.util.spec_from_file_location('tilo_service_consumer', module_path)
    service = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(service)
    return service


def read_receipt(path, expected):
    info = regular(path, 64 * 1024)
    require(info.st_mode & 0o022 == 0, 'account capability receipt permits foreign writes')
    value = json.loads(path.read_bytes(), object_pairs_hook=unique)
    require(isinstance(value, dict) and set(value) == set(expected)
            and type(value['schema']) is int and type(value['protected_grant_protocol']) is int
            and value == expected, 'installed account capability receipt differs from actual consumers')
    return value


def image_owned(path):
    info = path.lstat()
    require(not stat.S_ISLNK(info.st_mode) and info.st_uid == 0 and info.st_mode & 0o022 == 0,
            'account capability trust artifact is not image-owned: ' + str(path))


def check_image_trust():
    image_owned(Path('/usr/libexec/tilo-verify-account-consumer-capability'))
    image_owned(Path('/usr/libexec/tilo-service-consumer-capability.py'))
    image_owned(Path('/usr/libexec/tilo-account-credential'))
    image_owned(Path('/usr/bin/tilo-runtime-bundle'))
    for path in (Path('/usr/share/tilo'), Path('/usr/lib/tilo-runtime')):
        image_owned(path)
    share = Path('/usr/share/tilo')
    version_name, publisher_name = runtime_markers(share)
    for name in (version_name, publisher_name, 'account-consumer-capabilities.json'):
        image_owned(share / name)
    version = (share / version_name).read_text().strip()
    require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,255}', version) is not None,
            'invalid primary runtime version')
    bundle = Path('/usr/lib/tilo-runtime') / version
    image_owned(bundle)
    # Every interpreter/library that could execute before probe main belongs
    # to the immutable image namespace, including flat and nested loader paths.
    for directory, children, files in os.walk(bundle, followlinks=False):
        image_owned(Path(directory))
        for name in children + files:
            image_owned(Path(directory) / name)


def check_service_trust(evidence):
    require(isinstance(evidence, dict)
            and re.fullmatch('[0-9a-f]{64}', evidence.get('browser_package_hash', ''))
            and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,255}', evidence.get('cef_runtime_version', '')),
            'unsafe installed service proof identity')
    require(evidence.get('receiving_shell_path') in ('/usr/bin/shell', '/usr/bin/tilo-shell'),
            'unsafe installed receiving Shell')
    image_owned(Path(evidence['receiving_shell_path']))
    for relative in load_service_consumer().RECEIVING_DEPENDENCIES:
        image_owned(Path(relative))
    python = Path('/usr/bin/python3').resolve(strict=True)
    require(str(python) == evidence.get('receiving_python_path') and python.parent == Path('/usr/bin'),
            'installed receiving Python alias changed')
    image_owned(python)
    image_owned(Path('/usr/libexec/tilo-service-session-qualification.py'))
    image_owned(Path('/usr/share/tilo/browser-runtime-publisher.hex'))
    image_owned(Path('/usr/share/tilo-apps'))
    image_owned(Path('/usr/share/tilo-apps/pkgs'))
    roots = (Path('/usr/share/tilo-apps/pkgs') / evidence['browser_package_hash'],
             Path('/usr/share/tilo-apps/pkgs') / (evidence['browser_package_hash'] + '.pkg'),
             Path('/usr/lib/tilo-runtime') / evidence['cef_runtime_version'])
    for root in roots:
        image_owned(root)
        for directory, children, files in os.walk(root, followlinks=False):
            image_owned(Path(directory))
            for name in children + files:
                image_owned(Path(directory) / name)


def main(check=False):
    if not check:
        require(os.geteuid() == 0, 'capability receipt must be produced inside the root image build')
    target = Path('/usr/share/tilo/account-consumer-capabilities.json')
    if check:
        check_image_trust()
        require(regular(target, 64 * 1024).st_uid == 0, 'account capability receipt is not image-owned')
    evidence = None
    if check:
        saved = json.loads(target.read_bytes(), object_pairs_hook=unique)
        require(isinstance(saved, dict), 'invalid installed account capability receipt')
        evidence = saved.get('service_sessions')
        if evidence is not None:
            image_owned(Path('/usr/libexec/tilo-service-session-qualification.py'))
            image_owned(Path('/usr/bin/tilo-install'))
            image_owned(Path('/usr/share/tilo-apps/registry.toml'))
            check_service_trust(evidence)
    receipt = produce(qualify_services=not check, service_evidence=evidence)
    if check:
        receipt = read_receipt(target, receipt)
        # Only this bounded public identity result leaves the verifier.
        print(json.dumps(receipt, sort_keys=True, separators=(',', ':')))
        return
    descriptor, temporary = tempfile.mkstemp(prefix='.account-consumer-', dir=target.parent)
    try:
        with os.fdopen(descriptor, 'w') as output:
            json.dump(receipt, output, sort_keys=True, separators=(',', ':'))
            output.write('\n')
            output.flush()
            os.fchmod(output.fileno(), 0o644)
            os.fsync(output.fileno())
        os.replace(temporary, target)
        directory = os.open(target.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


if __name__ == '__main__':
    import sys
    require(sys.argv[1:] in ([], ['--check']), 'account capability verifier accepts only --check')
    main(check=sys.argv[1:] == ['--check'])

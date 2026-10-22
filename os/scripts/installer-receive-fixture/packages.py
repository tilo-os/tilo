"""Install measured ordinary apps through the real signing and package pipeline.

Only the disposable operator's private receiving namespace enrolls the exact
verified runtime publisher. App installation then runs as the real firstboot
user; no app ELF is executed directly and no registry row is written here.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import pwd
import re
import shutil
import stat
import tarfile
import tempfile
import tomllib

from semantic import bounded_command as _bounded_command

APPS = {'files': ('file-explorer', 'so.tilo.file-explorer'),
        'writer': ('word-processor', 'so.tilo.word-processor')}


def bounded_command(argv, deadline, private_log):
    # The common owned-child driver intentionally starts with a clean native
    # environment. Supply only this actual NSS login's ordinary paths, never
    # developer store/runtime/trust overrides or caller-selected authorities.
    if os.getuid() == 1000:
        home = Path(pwd.getpwuid(os.getuid()).pw_dir)
        require(Path(os.environ['HOME']) == home, 'package child differs from actual login home')
        values = {'HOME': str(home)}
        for key, fallback in (('XDG_DATA_HOME', '.local/share'), ('XDG_CONFIG_HOME', '.config'),
                              ('XDG_STATE_HOME', '.local/state')):
            path = Path(os.environ.get(key, str(home / fallback)))
            require(path.is_absolute() and path.is_relative_to(home), 'package child path escapes actual login')
            values[key] = str(path)
        runtime = Path(os.environ['XDG_RUNTIME_DIR'])
        require(runtime == Path('/run/user') / str(os.getuid()), 'package child runtime differs from actual login')
        values['XDG_RUNTIME_DIR'] = str(runtime)
        if os.environ.get('DBUS_SESSION_BUS_ADDRESS'):
            values['DBUS_SESSION_BUS_ADDRESS'] = os.environ['DBUS_SESSION_BUS_ADDRESS']
        argv = ['/usr/bin/env', *[key + '=' + value for key, value in values.items()], *argv]
    return _bounded_command(argv, deadline, private_log)


def require(value, message):
    if not value:
        raise RuntimeError(message)


def unique(pairs):
    value = {}
    for key, item in pairs:
        require(key not in value, 'duplicate package qualification field')
        value[key] = item
    return value


def read(path, maximum):
    path = Path(path)
    for edge in (path, *path.parents):
        require(not edge.is_symlink(), 'redirected package qualification artifact')
    before = path.stat()
    require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= maximum
            and not before.st_mode & 0o022, 'unprotected or unbounded package artifact')
    value = path.read_bytes()
    after = path.stat()
    identity = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns, item.st_ctime_ns)
    require(identity(before) == identity(after) and len(value) == before.st_size,
            'package artifact changed during observation')
    return value


def digest(path, maximum=1024 ** 3):
    path = Path(path)
    for edge in (path, *path.parents):
        require(not edge.is_symlink(), 'redirected measured package artifact')
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(descriptor)
        require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= maximum
                and not before.st_mode & 0o022, 'unprotected or unbounded measured package artifact')
        value = hashlib.sha256()
        total = 0
        while block := os.read(descriptor, 1024 * 1024):
            total += len(block)
            require(total <= maximum, 'measured package artifact grew beyond bounds')
            value.update(block)
        after = os.fstat(descriptor)
        identity = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns, item.st_ctime_ns)
        require(identity(before) == identity(after) == identity(path.stat()) and total == before.st_size,
                'measured package artifact changed during observation')
        return value.hexdigest()
    finally:
        os.close(descriptor)


def source(candidate, relative, maximum=2 * 1024 * 1024):
    hashes = json.loads(read(candidate / 'source-inputs.json', 32 * 1024 * 1024), object_pairs_hook=unique)
    expected = hashes.get(relative)
    require(isinstance(expected, str) and re.fullmatch('[0-9a-f]{64}', expected),
            'package source is absent from actual compiler cohort')
    path = candidate / relative
    require(digest(path, maximum) == expected, 'package source differs from actual compiler cohort')
    return path


def sdk(candidate, log, deadline):
    pins = json.loads(read(candidate / 'capture-service.json', 16384), object_pairs_hook=unique)
    subjects = json.loads(read(candidate / 'subjects.json', 16384), object_pairs_hook=unique)
    require(type(pins.get('schema')) is int and pins['schema'] == 1
            and pins.get('arch') == platform.machine(), 'candidate SDK pins differ')
    version = pins.get('runtime_version')
    publisher = pins.get('runtime_publisher')
    root_hash = pins.get('runtime_root_hash')
    require(isinstance(version, str) and re.fullmatch(r'[0-9A-Za-z.-]+\+native\.[0-9a-f]{32}', version)
            and all(isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value)
                    for value in (publisher, root_hash)), 'candidate runtime identity is invalid')
    root = candidate / 'host/share/tilo-imager/capture/runtime' / version
    bounded_command(['/usr/libexec/tilo/tilo-runtime-bundle', 'verify', str(root),
                     '--require-publisher', publisher, '--require-signature',
                     '--require-arch', platform.machine()], deadline, log)
    bundle = tomllib.loads(read(root / 'bundle.toml', 8 * 1024 * 1024).decode())
    require(bundle['version'] == version and bundle['root_hash'] == root_hash
            and bundle['publisher_hex'] == publisher, 'actual signed SDK differs from pins')
    expected = subjects.get('runtime', {}).get('sha256')
    actual = digest(root / 'lib/libruntime_dylib.so')
    require(isinstance(expected, str) and actual == expected, 'actual signed SDK differs from compiled role')
    return root, {'version': version, 'publisher': publisher, 'root_hash': root_hash, 'sdk_sha256': actual}


def enroll_runtime(candidate, private_log, deadline):
    """Root-only operator trust, called after entering the private target /etc."""
    require(os.getuid() == 0 and os.geteuid() == 0, 'runtime enrollment requires actual receiving root')
    # The receiver owns a distinct mount namespace with /etc cloned from the
    # baseline. Refuse the inherited namespace: no ordinary image trust edit.
    require(Path('/proc/self/ns/mnt').stat().st_ino != Path('/proc/1/ns/mnt').stat().st_ino,
            'runtime enrollment is outside the owned receiving namespace')
    actual_etc = Path('/etc').stat()
    baseline_etc = Path('/proc/1/root/etc').stat()
    require((actual_etc.st_dev, actual_etc.st_ino) != (baseline_etc.st_dev, baseline_etc.st_ino),
            'runtime enrollment has no private cloned target etc')
    _, evidence = sdk(candidate, private_log, deadline)
    trust = Path('/etc/tilo/runtime-bundle-trusted-publishers.txt')
    original = read(trust, 1024 * 1024)
    info = trust.stat()
    require(info.st_uid == 0 and info.st_mode & 0o022 == 0, 'actual runtime trust file is not root controlled')
    existing = {line.strip().lower() for line in original.decode().splitlines()
                if line.strip() and not line.lstrip().startswith('#')}
    if evidence['publisher'] not in existing:
        descriptor, temporary = tempfile.mkstemp(prefix='.capture-runtime-trust-', dir=trust.parent)
        try:
            os.fchmod(descriptor, stat.S_IMODE(info.st_mode))
            with os.fdopen(descriptor, 'wb') as output:
                output.write(original.rstrip(b'\n') + b'\n' + evidence['publisher'].encode() + b'\n')
                output.flush()
                os.fsync(output.fileno())
            require(read(trust, 1024 * 1024) == original, 'runtime trust changed during operator enrollment')
            os.replace(temporary, trust)
            directory = os.open(trust.parent, os.O_DIRECTORY | os.O_RDONLY | os.O_CLOEXEC)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    enrolled = {line.strip().lower() for line in trust.read_text().splitlines()
                if line.strip() and not line.lstrip().startswith('#')}
    require(evidence['publisher'] in enrolled, 'exact actual runtime publisher was not enrolled')
    return evidence


def archive_metadata(path):
    with tarfile.open(path) as archive:
        result = {}
        for name, limit in (('tapp.toml', 4096), ('manifest.toml', 2 * 1024 * 1024)):
            matches = [entry for entry in archive.getmembers() if entry.name == name]
            require(len(matches) == 1 and matches[0].isfile() and matches[0].size <= limit,
                    'signed package metadata is absent or ambiguous')
            result[name] = archive.extractfile(matches[0]).read()
    return result


def install(candidate, user_profile, private_log, deadline):
    require(os.getuid() == 1000 and os.geteuid() == 1000, 'app installation is not the real firstboot user')
    require(Path(pwd.getpwuid(os.getuid()).pw_dir) == user_profile == Path(os.environ['HOME']),
            'app installation home differs from actual NSS account')
    for name in ('TILO_APPS_DATA_DIR', 'TILO_APPS_SYSTEM_DIR', 'TILO_RUNTIME_BUNDLE_DIR',
                 'TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE'):
        require(not os.environ.get(name), 'app install must use ordinary registry/runtime/trust paths')
    bundle, runtime = sdk(candidate, private_log, deadline)
    subjects = json.loads(read(candidate / 'subjects.json', 16384), object_pairs_hook=unique)
    for relative in ('tools/apps/pack-runtime-package.sh', 'os/scripts/stamp-runtime.sh',
                     'tools/apps/stage_sdk_services.py'):
        source(candidate, relative)
    data = Path(os.environ.get('XDG_DATA_HOME', str(user_profile / '.local/share')))
    require(data.is_absolute() and data.is_relative_to(user_profile), 'actual app data home escapes the login')
    installed_runtime = data / 'tilo-runtime' / runtime['version']
    store = data / 'tilo-apps'
    result = {'runtime': runtime, 'registry': str(store / 'registry.toml'), 'packages': {}}
    with tempfile.TemporaryDirectory(prefix='tilo-native-packages-', dir=user_profile) as temporary:
        work = Path(temporary)
        signer = work / 'operator-signing-key.json'
        bounded_command(['/usr/libexec/tilo/tilo-app-package', 'keygen', '--out', str(signer)], deadline, private_log)
        signer.chmod(0o600)
        publisher = bounded_command(['/usr/libexec/tilo/tilo-runtime-bundle', 'key', 'pubkey', '--identity', str(signer)],
                                    deadline, private_log).decode().strip()
        require(re.fullmatch('[0-9a-f]{64}', publisher), 'actual operator app publisher is invalid')
        # Use the canonical runtime package producer, then retain its unchanged
        # signed manifest while packing the full offline form for normal install.
        runtime_package_version = tomllib.loads(read(
            source(candidate, 'apps/file-explorer/tilo-app.toml'), 2 * 1024 * 1024).decode())['package']['version']
        runtime_output = work / 'runtime-package'
        printed = bounded_command(['bash', str(candidate / 'tools/apps/pack-runtime-package.sh'), str(bundle),
                                   'so.tilo.runtime', 'Tilo runtime', runtime_package_version, str(signer), '/usr/bin', str(runtime_output)],
                                  deadline, private_log).decode().strip()
        thin = Path(printed)
        require(thin.is_relative_to(runtime_output) and thin.is_file(), 'canonical runtime package output differs')
        runtime_manifest = work / 'runtime-manifest.toml'
        runtime_manifest.write_bytes(archive_metadata(thin)['manifest.toml'])
        full_runtime = work / 'runtime.tapp'
        bounded_command(['/usr/libexec/tilo/tilo-app-package', 'pack', '--manifest', str(runtime_manifest), '--payload', str(bundle),
                         '--identity', str(signer), '--output', str(full_runtime)], deadline, private_log)
        bounded_command(['/usr/libexec/tilo/tilo-app-package', 'verify', '--trusted-key', publisher, str(full_runtime)], deadline, private_log)
        bounded_command(['/usr/libexec/tilo/tilo-install', 'install', str(full_runtime), '--pinned', '--require-publisher', publisher,
                         '--allow-self-signed'], deadline, private_log)
        bounded_command(['/usr/libexec/tilo/tilo-runtime-bundle', 'verify', str(installed_runtime),
                         '--require-publisher', runtime['publisher'], '--require-signature',
                         '--require-arch', platform.machine()], deadline, private_log)
        actual_bundle = tomllib.loads(read(installed_runtime / 'bundle.toml', 8 * 1024 * 1024).decode())
        require(actual_bundle['root_hash'] == runtime['root_hash']
                and digest(installed_runtime / 'lib/libruntime_dylib.so') == runtime['sdk_sha256'],
                'normally installed SDK differs from actual signed cohort')
        for role, (name, app_id) in APPS.items():
            expected = subjects.get(role, {}).get('sha256')
            executable = candidate / name
            require(digest(executable) == expected, 'app executable differs from actual measured role')
            authored = source(candidate, 'apps/' + name + '/tilo-app.toml')
            surface = source(candidate, 'apps/' + name + '/surface.toml')
            manifest = work / (name + '.toml')
            manifest.write_bytes(read(authored, 2 * 1024 * 1024))
            bounded_command(['bash', str(candidate / 'os/scripts/stamp-runtime.sh'), str(manifest),
                             'tilo-runtime/' + runtime['version']], deadline, private_log)
            payload = work / name
            (payload / 'bin').mkdir(parents=True)
            shutil.copyfile(executable, payload / 'bin' / name)
            (payload / 'bin' / name).chmod(0o755)
            shutil.copyfile(surface, payload / 'surface.toml')
            package = work / (name + '.tapp')
            bounded_command(['/usr/libexec/tilo/tilo-app-package', 'pack', '--manifest', str(manifest), '--payload', str(payload),
                             '--identity', str(signer), '--output', str(package)], deadline, private_log)
            bounded_command(['/usr/libexec/tilo/tilo-app-package', 'verify', '--trusted-key', publisher, str(package)], deadline, private_log)
            metadata = archive_metadata(package)
            package_hash = tomllib.loads(metadata['tapp.toml'].decode())['package_id'].removeprefix('tapp:')
            require(re.fullmatch('[0-9a-f]{64}', package_hash), 'actual signed app root is invalid')
            signed_manifest = tomllib.loads(metadata['manifest.toml'].decode())
            require(signed_manifest['package']['id'] == app_id
                    and signed_manifest['package']['runtime'] == 'tilo-runtime/' + runtime['version']
                    and signed_manifest['publisher']['identity'] == 'ed25519:' + publisher,
                    'actual signed app declaration differs')
            bounded_command(['/usr/libexec/tilo/tilo-install', 'install', str(package), '--force', '--pinned',
                             '--require-package-root', package_hash, '--require-publisher', publisher,
                             '--allow-self-signed'], deadline, private_log)
            registry = tomllib.loads(read(store / 'registry.toml', 4 * 1024 * 1024).decode())
            rows = [row for row in registry.get('entry', []) if row.get('id') == app_id]
            require(len(rows) == 1, 'ordinary installed app registry is ambiguous')
            row = rows[0]
            installed = store / 'pkgs' / package_hash
            metadata_path = store / 'pkgs' / (package_hash + '.pkg') / 'manifest.toml'
            require(row['package_id_hex'] == package_hash and row['publisher_key_hex'] == publisher
                    and row['runtime'] == 'tilo-runtime/' + runtime['version']
                    and row['payload_root'] == str(installed) and row['manifest_path'] == str(metadata_path),
                    'ordinary installed app differs from signed candidate')
            require(digest(installed / 'bin' / name) == expected
                    and read(metadata_path, 2 * 1024 * 1024) == metadata['manifest.toml'],
                    'normally installed app payload or metadata differs')
            result['packages'][app_id] = {'package_id': package_hash, 'publisher': publisher,
                                         'elf_sha256': expected, 'payload_root': str(installed),
                                         'manifest_path': str(metadata_path), 'runtime': row['runtime']}
    return result

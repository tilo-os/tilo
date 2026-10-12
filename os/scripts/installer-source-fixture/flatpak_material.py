"""Real signed runnable USER Flatpak material, only in the owned source Home.

The actual source UID invokes this builder. It supplies no migration authority
and never launches the app. Receiving launch-result.json must come from the
canonical runnable payload inside the actual restored sandbox.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time

from bounded_native import bounded, observe_exit, retire_child, assert_children_retired

RUNTIME_ROOT = Path('/run/tilo-flatpak-runnable-runtime')
APP_ASSET = Path('/usr/libexec/tilo-installer-source-fixture/fixture-app')
APP_SHA = '51102c91a2305b65f8069d8450f93f3ba09cde5cda9dee6521f52b21d9ec9a5a'
APP = 'app/org.tilo.Fixture/x86_64/stable'
RUNTIME = 'runtime/org.tilo.FixtureRuntime/x86_64/stable'
COLLECTION = 'org.tilo.NativeFixture'
_STATE = b'genuine selected application state'
_AGENTS = []


def require(value, message):
    if not value:
        raise RuntimeError(message)


def stamp(info):
    return (info.st_dev, info.st_ino, info.st_uid, info.st_gid, info.st_mode,
            info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def held_bytes(path, limit, uid, mode):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and before.st_uid == uid
                and before.st_mode & 0o7777 == mode and before.st_nlink == 1
                and 0 < before.st_size <= limit, 'runnable material file custody refused')
        data = bytearray()
        end = time.monotonic() + 5
        while len(data) <= limit:
            require(time.monotonic() < end, 'runnable material read deadline')
            part = os.read(fd, min(1048576, limit + 1 - len(data)))
            if not part:
                break
            data.extend(part)
        require(len(data) == before.st_size and stamp(before) == stamp(os.fstat(fd))
                and stamp(before) == stamp(path.lstat()), 'runnable material changed')
        return bytes(data)
    finally:
        os.close(fd)


def private(path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.geteuid()
            and info.st_mode & 0o7022 == 0, 'source material directory custody refused')


def write_new(path, data, mode=0o600):
    private(path.parent)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, mode)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def runtime_material(contents):
    root = RUNTIME_ROOT.lstat()
    require(stat.S_ISDIR(root.st_mode) and root.st_uid == root.st_gid == 0
            and root.st_mode & 0o7777 == 0o555, 'reviewed runnable runtime root refused')
    manifest = json.loads(held_bytes(RUNTIME_ROOT / 'runtime.json', 65536, 0, 0o444))
    require(set(manifest) == {'schema', 'entries', 'native_app_executed'}
            and manifest['schema'] == 1 and manifest['native_app_executed'] is False
            and isinstance(manifest['entries'], list) and 0 < len(manifest['entries']) <= 64,
            'reviewed runnable runtime manifest refused')
    seen = set()
    total = 0
    for entry in manifest['entries']:
        require(set(entry) == {'path', 'sha256', 'bytes'}, 'runtime entry schema refused')
        relative = Path(entry['path'])
        require(not relative.is_absolute() and all(part not in ('', '.', '..') for part in relative.parts)
                and str(relative) == entry['path']
                and (entry['path'].startswith(('lib/', 'lib64/')) or entry['path'] in ('bin/bash', 'bin/sleep'))
                and entry['path'] not in seen, 'runtime entry path refused')
        seen.add(entry['path'])
        data = held_bytes(RUNTIME_ROOT / 'usr' / relative, 16 << 20, 0, 0o555)
        require(data.startswith(b'\x7fELF') and len(data) == entry['bytes']
                and hashlib.sha256(data).hexdigest() == entry['sha256'], 'actual runtime ELF differs')
        total += len(data)
        require(total <= 64 << 20, 'actual runtime closure too large')
        write_new(contents / relative, data, 0o755)
    require({'bin/bash', 'bin/sleep'} <= seen, 'actual runtime shell closure incomplete')


def install(home, deadline, owned_runner=bounded):
    require(os.getuid() == os.geteuid() == 1000, 'actual original source UID1000 required')
    require(home.is_absolute() and home.resolve(strict=True) == home, 'actual source Home path refused')
    info = home.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == 1000 and info.st_mode & 0o7022 == 0,
            'actual source Home custody refused')
    installation = home / '.local/share/flatpak'
    require(not os.path.lexists(installation), 'fresh isolated user Flatpak installation required')
    root = home / '.cache/tilo-native-flatpak-material'
    require(not os.path.lexists(root), 'fresh source material branch required')
    private(root)
    gnupg = root / 'gnupg'
    private(gnupg)
    repo = root / 'origin'

    def command(program, args, *, as_home=home):
        remaining = deadline - time.monotonic()
        require(remaining > 0, 'source Flatpak material deadline exceeded')
        env = ['HOME=' + str(as_home), 'XDG_DATA_HOME=' + str(as_home / '.local/share'),
               'XDG_CONFIG_HOME=' + str(as_home / '.config'),
               'XDG_CACHE_HOME=' + str(as_home / '.cache'),
               'XDG_RUNTIME_DIR=/run/user/1000', 'GNUPGHOME=' + str(gnupg),
               'FLATPAK_USER_DIR=' + str(as_home / '.local/share/flatpak')]
        code, out, err = owned_runner(['/usr/bin/env', *env, program, *args],
                                      timeout=min(120, remaining), limit=8 << 20)
        require(code == 0, 'actual source material command failed: ' + Path(program).name
                + ': ' + err[:512].decode('utf-8', errors='replace'))
        return out

    agent = None
    try:
        # The signing agent is our actual foreground Child, not an autostarted
        # daemon. Keep its custody through every export and retire it before capture.
        agent = subprocess.Popen(['/usr/bin/gpg-agent', '--homedir', str(gnupg),
                                  '--daemon', '--no-detach', '--disable-scdaemon'],
                                 env={'PATH': '/usr/bin', 'HOME': str(home), 'LC_ALL': 'C',
                                      'XDG_RUNTIME_DIR': '/run/user/1000'},
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, start_new_session=True)
        _AGENTS.append(agent)
        socket_path = command('/usr/bin/gpgconf', ['--homedir', str(gnupg), '--list-dirs', 'agent-socket']).decode().strip()
        end = min(deadline, time.monotonic() + 10)
        while not Path(socket_path).exists():
            require(observe_exit(agent) is None and time.monotonic() < end, 'owned signing agent readiness failed')
            time.sleep(.025)
        command('/usr/bin/gpg', ['--homedir', str(gnupg), '--no-autostart', '--batch',
                                '--pinentry-mode', 'loopback', '--passphrase', '',
                                '--quick-generate-key', 'Tilo Native Fixture', 'ed25519', 'sign', '0'])
        listing = command('/usr/bin/gpg', ['--homedir', str(gnupg), '--no-autostart',
                                          '--with-colons', '--list-secret-keys', 'Tilo Native Fixture']).decode()
        fingerprints = [line.split(':')[9] for line in listing.splitlines() if line.startswith('fpr:')]
        require(len(fingerprints) == 1 and re.fullmatch('[A-F0-9]{40,64}', fingerprints[0]), 'actual signing fingerprint refused')
        fingerprint = fingerprints[0]
        public = command('/usr/bin/gpg', ['--homedir', str(gnupg), '--no-autostart', '--export', fingerprint])
        require(0 < len(public) <= 65536, 'actual signing public key refused')
        write_new(root / 'signing.gpg', public)
        payload = held_bytes(APP_ASSET, 65536, 0, 0o444)
        require(hashlib.sha256(payload).hexdigest() == APP_SHA, 'canonical runnable app bytes differ')
        for runtime in (True, False):
            build = root / ('runtime-build' if runtime else 'app-build')
            private(build / 'files')
            contents = build / ('usr' if runtime else 'files')
            private(contents / 'bin')
            private(build / 'export')
            name = 'org.tilo.FixtureRuntime' if runtime else 'org.tilo.Fixture'
            section = 'Runtime' if runtime else 'Application'
            metadata = f'[{section}]\nname={name}\nruntime=org.tilo.FixtureRuntime/x86_64/stable\nsdk=org.tilo.FixtureRuntime/x86_64/stable\n'
            if not runtime:
                metadata += 'command=fixture\n'
            write_new(build / 'metadata', metadata.encode())
            if runtime:
                runtime_material(contents)
            write_new(contents / 'bin/fixture', payload, 0o755)
            if not runtime:
                write_new(build / 'export/share/applications/org.tilo.Fixture.desktop',
                          b'[Desktop Entry]\nType=Application\nName=Tilo Runnable Fixture\nExec=fixture\nTerminal=false\n')
            args = ['build-export', '--collection-id=' + COLLECTION]
            if runtime:
                args.append('--runtime')
            args += ['--gpg-sign=' + fingerprint, '--gpg-homedir=' + str(gnupg), str(repo), str(build), 'stable']
            command('/usr/bin/flatpak', args)
        command('/usr/bin/flatpak', ['build-update-repo', '--collection-id=' + COLLECTION,
                                   '--gpg-sign=' + fingerprint, '--gpg-homedir=' + str(gnupg), str(repo)])
        definition = f'[Flatpak Repo]\nTitle=Native Fixture\nUrl={repo.as_uri()}/\nGPGKey={base64.b64encode(public).decode()}\n'
        write_new(root / 'source.flatpakrepo', definition.encode())
        command('/usr/bin/flatpak', ['remote-add', '--user', '--from', '--collection-id=' + COLLECTION,
                                   '--gpg-import=' + str(root / 'signing.gpg'), 'fixture', str(root / 'source.flatpakrepo')])
        for reference in (RUNTIME, APP):
            command('/usr/bin/flatpak', ['install', '--user', '--noninteractive', '--assumeyes',
                                       '--no-deps', '--no-related', 'fixture', reference])
        command('/usr/bin/flatpak', ['remote-modify', '--user', '--url=https://127.0.0.1:1/offline-fixture/', 'fixture'])
        commits = {reference: command('/usr/bin/flatpak', ['info', '--user', '--show-commit', reference]).decode().strip()
                   for reference in (RUNTIME, APP)}
        require(all(re.fullmatch('[a-f0-9]{64}', value) for value in commits.values()), 'actual installed Flatpak commits refused')
        write_new(home / '.var/app/org.tilo.Fixture/data/selected-document', _STATE)
        write_new(home / '.var/app/org.tilo.Excluded/data/document', b'excluded application state')
        write_new(home / 'owner-document', b'original owner document must not enter app sandbox')
        return {'app_ref': APP, 'runtime_ref': RUNTIME, 'commits': commits,
                'selected_state': 'data/selected-document', 'native_app_executed': False}
    finally:
        if agent is not None:
            retire_child(agent)
            _AGENTS.remove(agent)
        assert_children_retired()

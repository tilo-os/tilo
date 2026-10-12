"""Seed native source formats through the admitted official Electron APIs.

This is source material, not an authenticated login or a session declaration.
The normal Imager discovers these profiles, obtains explicit source consent,
reads them through its protected adapters, and seals the resulting sessions.
"""
import ctypes
import json
import os
from pathlib import Path
import stat
import time

from bounded_native import bounded, assert_children_retired

PUBLIC_ROOT = Path('/run/tilo-source-public/service-source')


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def public(path, regular):
    for component in (path, *path.parents):
        info = component.lstat()
        require(info.st_uid == 0 and not info.st_mode & 0o022
                and not stat.S_ISLNK(info.st_mode), 'official source path custody refused')
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) if regular else stat.S_ISDIR(info.st_mode),
            'official source path type refused')


def generate(home, runtime, deadline):
    require(os.getuid() == os.geteuid() == 1000, 'actual source account required')
    electron = PUBLIC_ROOT / 'electron/electron'
    app = PUBLIC_ROOT / 'app'
    public(electron, True)
    public(app / 'main.cjs', True)
    public(app / 'package.json', True)
    # The qualifier verifies the entire fixed Electron distribution and JS
    # source ledgers before staging this readonly public root. Do not replace
    # it with a system Electron or relax Chromium's sandbox.
    generated = home / '.cache/tilo-official-session-source'
    generated.mkdir(mode=0o700, parents=True, exist_ok=False)
    (generated / 'source-format').mkdir(mode=0o700)
    require(ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0,
            'official generator descendant ownership unavailable')
    remaining = min(60, deadline - time.monotonic())
    require(remaining > 0, 'official source generation deadline elapsed')
    code, output, _ = bounded(['/usr/bin/env', 'HOME=' + str(generated),
        'XDG_RUNTIME_DIR=' + str(runtime),
        'DBUS_SESSION_BUS_ADDRESS=unix:path=' + str(runtime / 'bus'),
        str(electron), '--ozone-platform=headless', '--disable-gpu', str(app)],
        timeout=remaining, limit=65536)
    require(code == 0, 'actual official source generation refused')
    observed = json.loads(output)
    require(observed == {'schema': 1, 'status': 'generated', 'electron': '44.4.5',
        'app_name': 'Slack', 'storage_backend': 'gnome_libsecret',
        'slack_cookie_count': 1, 'discord_cookie_count': 0,
        'discord_origin_storage_count': 1, 'authenticated': False},
        'official source generation observation refused')
    # No source capture begins while a generator descendant can still write
    # these stores. Reap only our actual children; never infer numeric owners.
    while True:
        require(time.monotonic() < deadline, 'official source descendant retirement unknown')
        try:
            child, status = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            break
        if child == 0:
            time.sleep(.025)
        else:
            require(status == 0, 'official source descendant failed')
    assert_children_retired()
    config = home / '.config'
    config.mkdir(mode=0o700, exist_ok=True)
    for source, target in (('slack', 'Slack'), ('discord', 'discord')):
        original = generated / 'source-format' / source
        info = original.lstat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == 1000
                and info.st_mode & 0o777 == 0o700 and original.resolve() == original,
                'official source profile ownership refused')
        destination = config / target
        require(not os.path.lexists(destination), 'native source profile already exists')
        os.rename(original, destination)
    return observed

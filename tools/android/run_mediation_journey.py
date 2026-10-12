#!/usr/bin/env python3
"""Execute the owned Android API acceptance APK in an isolated real Tilo session.

No installs, grants, emulator, desktop capture or builds. This proves denial and
lifecycle; it does not qualify approved file access or notification delivery.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import tomllib

from qualify_apk import digest, pinned, verify_runtime
from execution_host import add_architecture, native_architecture
from run_arity_journey import REPO, Journey, wait_for, require_ok, checked_output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    add_architecture(parser)
    for name in ('payload', 'runtime-receipt', 'runtime-parent', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('apk', 'signer', 'runtime-receipt'):
        parser.add_argument('--' + name + '-sha256', required=True)
    args = parser.parse_args()
    native_architecture(args.architecture)
    output = checked_output(args.output)
    payload = args.payload.resolve(strict=True)
    pinned(payload / 'app.apk', args.apk_sha256)
    qualification = json.loads((payload / 'qualification.json').read_text())
    if not qualification.get('signature_verified') or qualification.get('signer_sha256') != args.signer_sha256:
        raise RuntimeError('fixture signature qualification does not match')
    if qualification['runtime_receipt_sha256'] != args.runtime_receipt_sha256:
        raise RuntimeError('qualification runtime differs')
    verify_runtime(payload / 'runtime', args.runtime_receipt, args.runtime_receipt_sha256, args.architecture)
    package = tomllib.loads((payload / 'tilo.toml').read_text())['package']['id']
    parent = args.runtime_parent.resolve(strict=True)
    if parent.stat().st_uid != os.geteuid() or parent.stat().st_mode & 0o077:
        raise RuntimeError('runtime parent must be private and user-owned')
    output.mkdir(mode=0o700)
    profile = output / 'profile'
    catalogue = profile / 'data/tilo-apps'
    catalogue.mkdir(parents=True)
    (catalogue / 'registry.toml').write_text('[[entry]]\nid = ' + json.dumps(package)
        + '\nversion = "0.0.0"\npayload_root = ' + json.dumps(str(payload))
        + '\nmanifest_path = ' + json.dumps(str(payload / 'tilo.toml')) + '\n')
    with tempfile.TemporaryDirectory(prefix='tilo-android-api-', dir=parent) as runtime:
        socket = Path(runtime) / 'control.sock'
        env = {**os.environ, 'XDG_RUNTIME_DIR': runtime, 'TILO_HEADLESS_PROFILE': str(profile),
               'TILO_APPS_DATA_DIR': str(catalogue), 'TILO_LAUNCHER_BIN': str(REPO / 'target/debug/tilo-launcher'),
               'LP_NUM_THREADS': '2', 'RAYON_NUM_THREADS': '2'}
        # No desktop bus or extra portal capability is inherited by this fixture.
        env.pop('DBUS_SESSION_BUS_ADDRESS', None)
        with (output / 'compositor.log').open('w') as log:
            process = subprocess.Popen([str(REPO / 'scripts/headless-examples/spawn-headless.sh'), str(socket)],
                cwd=REPO, env=env, stdout=log, stderr=log, start_new_session=True)
            try:
                wait_for(lambda: socket.is_socket() if process.poll() is None else
                    (_ for _ in ()).throw(RuntimeError('compositor exited; inspect log')), 'control socket')
                journey = Journey(socket, output, package, profile)
                frame = journey.launch()
                room = journey.workspace()['room']
                private = profile / 'data/tilo-rooms' / room / package
                def accepted():
                    files = list(private.rglob('acceptance.txt'))
                    return files[0] if len(files) == 1 and 'complete=true' in files[0].read_text() else None
                acceptance = wait_for(accepted, 'actual Android permission and picker callbacks')
                text = acceptance.read_text()
                expected = ['notification-capability=unavailable;backend=filtered-Tilo',
                    'permissions=denied;callback=19', 'picker=cancelled;callback=23;uri=none', 'complete=true']
                if sorted(text.splitlines()) != sorted(expected):
                    raise RuntimeError('unexpected Android API results: ' + text)
                (output / 'acceptance.txt').write_text(text)
                require_ok(journey.request('snapshot', path=str(output / 'android-api.png')))
                journey.close()
                def closed():
                    files = list(private.rglob('lifecycle.txt'))
                    return files[0] if len(files) == 1 and 'destroy' in files[0].read_text() else None
                lifecycle = wait_for(closed, 'actual Android close lifecycle')
                stages = lifecycle.read_text().splitlines()
                if stages != ['pause', 'stop', 'destroy']:
                    raise RuntimeError('unexpected lifecycle ordering: ' + repr(stages))
                (output / 'lifecycle.txt').write_text(lifecycle.read_text())
                log.flush()
                runtime_log = (output / 'compositor.log').read_text()
                if 'reply from authenticated chooser: failed' not in runtime_log:
                    raise RuntimeError('denial did not come from the authenticated chooser service')
                if 'landlock: fully enforced' not in runtime_log or 'no_new_privs: true' not in runtime_log:
                    raise RuntimeError('mandatory confinement evidence absent')
                result = {'schema': 1, 'completed_at_utc': datetime.now(timezone.utc).isoformat(),
                    'execution': 'headless production Tilo, actual Android ART, ' + args.architecture + ' software graphics',
                    'production_binaries': {name: digest(REPO / 'target/debug' / name) for name in ('shell', 'tilo-launcher')},
                    'apk_sha256': args.apk_sha256, 'runtime_receipt_sha256': args.runtime_receipt_sha256,
                    'room': room, 'android_results': expected, 'lifecycle': stages, 'presented_frame': frame,
                    'png_sha256': digest(output / 'android-api.png'), 'additional_grants': False,
                    'real_device': False, 'approved_file_access': False, 'notification_delivery': False}
            finally:
                if process.poll() is None: os.killpg(process.pid, signal.SIGTERM)
                try: process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
        if socket.exists(): raise RuntimeError('owned control socket leaked')
    result['owned_compositor_stopped'] = True
    (output / 'journey.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()

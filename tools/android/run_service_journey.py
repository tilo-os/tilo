#!/usr/bin/env python3
"""Execute the owned Android service APK through the real Tilo launcher.

No installs, grants, emulator, desktop capture or builds. The same APK must
reproduce the old service failure and exercise the fixed production framework.
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
    parser.add_argument('--expect-incompatibility', action='store_true')
    parser.add_argument('--queued-starts', action='store_true',
        help='exercise QueuedServiceActivity on the unchanged signed APK')
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
                first = journey.workspace()
                frames = []
                results = []
                def private_file(room, name):
                    paths = list((profile / 'data/tilo-rooms' / room / package).rglob(name))
                    return paths[0] if len(paths) == 1 else None
                def visit(room, number):
                    frames.append(journey.launch())
                    def completed():
                        path = private_file(room, 'services.txt')
                        marker = 'journey-complete' if args.queued_starts else 'current-stop='
                        if path and path.read_text().count(marker) == number:
                            return path
                        return None
                    path = wait_for(completed, 'actual Android service stop results')
                    text = path.read_text()
                    current = text.split('activity-create=')[-1].splitlines()
                    if args.queued_starts:
                        expected = [str(number), 'camera-denied=true',
                            'queued-stop=' + ('false' if args.expect_incompatibility else 'true'),
                            'queued-repeat-stop=false', 'service-create;main=true']
                        if args.expect_incompatibility:
                            expected += ['start=1;action=cancelled-first;main=true',
                                'start=2;action=cancelled-second;main=true']
                        expected += ['start=3;action=restart;main=true', 'service-destroy;main=true',
                            'running-stop=true', 'running-repeat-stop=false', 'service-create;main=true',
                            'start=4;action=late-restart;main=true', 'service-destroy;main=true',
                            'late-stop=true', 'late-repeat-stop=false', 'journey-complete']
                    elif args.expect_incompatibility:
                        expected = ['1', 'camera-denied=true', 'service-create;main=true',
                            'start=0;main=true', 'start=0;main=true', 'stale-stop=true', 'current-stop=true']
                    else:
                        expected = [str(number), 'camera-denied=true', 'service-create;main=true',
                            'start=1;main=true', 'start=2;main=true', 'stale-stop=false',
                            'service-destroy;main=true', 'current-stop=true']
                    if current != expected:
                        raise RuntimeError('unexpected actual service results: ' + repr(current))
                    require_ok(journey.request('snapshot', path=str(output / ('visit-' + str(len(frames)) + '.png'))))
                    journey.close()
                    lifecycle = wait_for(lambda: private_file(room, 'activity-lifecycle.txt'), 'activity lifecycle')
                    wait_for(lambda: lifecycle.read_text().splitlines() == ['pause', 'stop', 'destroy'] * number,
                        'actual ordered activity lifecycle')
                    visits = private_file(room, 'visits.txt')
                    if visits.read_text().splitlines() != ['visit=' + str(i) for i in range(1, number + 1)]:
                        raise RuntimeError('private user data was not preserved')
                    results.append({'room': room, 'visit': number, 'service_results': current,
                        'lifecycle': lifecycle.read_text().splitlines()})
                    return path.read_bytes(), visits.read_bytes()
                preserved = visit(first['room'], 1)
                if not args.expect_incompatibility:
                    preserved = visit(first['room'], 2)
                    wheel = next(t for t in journey.request('list_targets')['targets'] if t['name'] == 'wheel_disc')
                    x, y, _, h = wheel['rect']
                    require_ok(journey.request('click_at', x=x + 24, y=y + h - 34))
                    second = wait_for(lambda: journey.workspace() if journey.workspace()['id'] != first['id'] else None,
                        'new isolated workspace')
                    visit(second['room'], 1)
                    if preserved != (private_file(first['room'], 'services.txt').read_bytes(),
                            private_file(first['room'], 'visits.txt').read_bytes()):
                        raise RuntimeError('second workspace modified first workspace data')
                    journey.invoke('actions', 'workspace.previous')
                    wait_for(lambda: journey.workspace()['id'] == first['id'], 'return to first workspace')
                    visit(first['room'], 3)
                log.flush()
                runtime_log = (output / 'compositor.log').read_text()
                if 'landlock: fully enforced' not in runtime_log or 'no_new_privs: true' not in runtime_log:
                    raise RuntimeError('mandatory confinement evidence absent')
                result = {'schema': 1, 'completed_at_utc': datetime.now(timezone.utc).isoformat(),
                    'execution': 'headless production Tilo, actual Android ART, ' + args.architecture + ' software graphics',
                    'production_binaries': {name: digest(REPO / 'target/debug' / name) for name in ('shell', 'tilo-launcher')},
                    'apk_sha256': args.apk_sha256, 'runtime_receipt_sha256': args.runtime_receipt_sha256,
                    'runtime_accepted': not args.expect_incompatibility, 'results': results,
                    'queued_start_cancellation': args.queued_starts,
                    'presented_frames': frames, 'additional_grants': False, 'real_device': False}

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

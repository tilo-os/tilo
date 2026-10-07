#!/usr/bin/env python3
"""Exercise approved fixture documents and notifications in a private Tilo session.

Creates and grants only its own new fixture files, revokes both grants before
completion, and uses an owned session bus. Never touches user documents or buses.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import signal
import select
import subprocess
import tempfile
import time
import tomllib
import sys

from qualify_apk import digest, pinned, verify_runtime
from execution_host import add_architecture, native_architecture
from run_arity_journey import REPO, Journey, wait_for, require_ok, checked_output

sys.path.insert(0, str(REPO / 'tools'))
from journey_support import AccessiblePicker


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)


def stop_bus_helpers(runtime):
    """Stop activated daemons only when their environment names this owned session."""
    names = {'at-spi-bus-laun', 'at-spi2-registr', 'dbus-daemon',
             'xdg-desktop-por', 'pipewire', 'wireplumber', 'pulseaudio'}
    handles = []
    try:
        for entry in Path('/proc').iterdir():
            if not entry.name.isdigit(): continue
            try:
                if (entry / 'comm').read_text().strip() not in names: continue
                handle = os.pidfd_open(int(entry.name))
                try:
                    if select.select([handle], [], [], 0)[0]: continue
                    try:
                        environment = (entry / 'environ').read_bytes().split(b'\0')
                    except PermissionError:
                        # Exit can revoke /proc access before the pidfd becomes
                        # ready. Wait briefly for proof of that exact exit; an
                        # unreadable helper that stays live must still fail.
                        if select.select([handle], [], [], 1)[0]: continue
                        raise
                    if os.fsencode('XDG_RUNTIME_DIR=' + runtime) not in environment: continue
                    signal.pidfd_send_signal(handle, signal.SIGTERM)
                    handles.append(handle)
                    handle = None
                finally:
                    if handle is not None: os.close(handle)
            except (FileNotFoundError, ProcessLookupError):
                continue
        # Readiness on pidfds means exit, without relying on reusable process IDs.
        pending = set(handles)
        deadline = time.monotonic() + 3
        while pending and time.monotonic() < deadline:
            ready, _, _ = select.select(list(pending), [], [], 0.05)
            pending.difference_update(ready)
        for handle in pending:
            signal.pidfd_send_signal(handle, signal.SIGKILL)
        if pending and len(select.select(list(pending), [], [], 3)[0]) != len(pending):
            raise RuntimeError('owned bus helper did not stop')
    finally:
        for handle in handles: os.close(handle)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    add_architecture(parser)
    for name in ('payload', 'runtime-receipt', 'runtime-parent', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('apk', 'signer', 'runtime-receipt'):
        parser.add_argument('--' + name + '-sha256', required=True)
    parser.add_argument('--fixture-grants', action='store_true', required=True,
                        help='explicitly authorize grants only on newly created fixture documents')
    args = parser.parse_args()
    native_architecture(args.architecture)
    output = checked_output(args.output)
    payload = args.payload.resolve(strict=True)
    pinned(payload / 'app.apk', args.apk_sha256)
    qualification = json.loads((payload / 'qualification.json').read_text())
    if not qualification.get('signature_verified') or qualification.get('signer_sha256') != args.signer_sha256:
        raise RuntimeError('fixture signature qualification does not match')
    if not qualification.get('notification_provider_declared'):
        raise RuntimeError('fixture did not declare its filtered notification provider')
    if qualification['runtime_receipt_sha256'] != args.runtime_receipt_sha256:
        raise RuntimeError('qualification runtime differs')
    verify_runtime(payload / 'runtime', args.runtime_receipt, args.runtime_receipt_sha256, args.architecture)
    package = tomllib.loads((payload / 'tilo.toml').read_text())['package']['id']
    parent = args.runtime_parent.resolve(strict=True)
    if parent.stat().st_uid != os.geteuid() or parent.stat().st_mode & 0o077:
        raise RuntimeError('runtime parent must be private and user-owned')
    output.mkdir(mode=0o700)
    output = output.resolve(strict=True)
    profile = output / 'profile'
    catalogue = profile / 'data/tilo-apps'
    catalogue.mkdir(parents=True)
    (catalogue / 'registry.toml').write_text('[[entry]]\nid = ' + json.dumps(package)
        + '\nversion = "0.0.0"\npayload_root = ' + json.dumps(str(payload))
        + '\nmanifest_path = ' + json.dumps(str(payload / 'tilo.toml')) + '\n')
    documents = output / 'fixture-documents'
    documents.mkdir(mode=0o700)
    source, destination, canary = [documents / name for name in
        ('android-input.txt', 'android-output.txt', 'private-canary')]
    source.write_text('Tilo approved Android fixture\n')
    destination.write_text('Empty disposable output\n')
    canary.write_text('Ungrantable sibling fixture\n')
    initial_source, initial_canary = digest(source), digest(canary)
    grants = []
    journey = None
    with tempfile.TemporaryDirectory(prefix='tilo-android-integration-', dir=parent) as runtime:
        socket, bus_socket = Path(runtime) / 'control.sock', Path(runtime) / 'session-bus.sock'
        env = {**os.environ, 'XDG_RUNTIME_DIR': runtime, 'TILO_HEADLESS_PROFILE': str(profile),
               'TILO_APPS_DATA_DIR': str(catalogue), 'TILO_LAUNCHER_BIN': str(REPO / 'target/debug/tilo-launcher'),
               'DBUS_SESSION_BUS_ADDRESS': 'unix:path=' + str(bus_socket),
               'LP_NUM_THREADS': '2', 'RAYON_NUM_THREADS': '2',
               'RUST_LOG': 'info,shell::notifications=debug'}
        with (output / 'bus.log').open('w') as bus_log, (output / 'compositor.log').open('w') as log:
            bus = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--nopidfile',
                '--address=' + env['DBUS_SESSION_BUS_ADDRESS']], env=env, stdout=bus_log,
                stderr=bus_log, start_new_session=True)
            process = None
            try:
                wait_for(lambda: bus_socket.is_socket(), 'owned bus socket')
                picker = AccessiblePicker(env['DBUS_SESSION_BUS_ADDRESS'])
                described, activate = picker.described, picker.activate
                process = subprocess.Popen([str(REPO / 'scripts/headless-examples/spawn-headless.sh'), str(socket)],
                    cwd=REPO, env=env, stdout=log, stderr=log, start_new_session=True)
                wait_for(lambda: socket.is_socket() if process.poll() is None else
                    (_ for _ in ()).throw(RuntimeError('compositor exited; inspect log')), 'control socket')
                journey = Journey(socket, output, package, profile)
                for path, verb, identifier in [(source, 'read', 'android-fixture-read'),
                                              (destination, 'write', 'android-fixture-write')]:
                    journey.request('grant', subject='app:' + package, scope='fs:' + str(path),
                                    verb=verb, id=identifier)
                    grants.append(identifier)
                frame = journey.launch()
                room = journey.workspace()['room']
                private = profile / 'data/tilo-rooms' / room / package
                def notification():
                    return next((n for n in journey.state()['chrome']['notifications']
                        if n['title'] == 'Android integration fixture'
                        and n['body'] == 'Approved document round trip'), None)
                delivered = wait_for(notification, 'actual compositor notification delivery')
                require_ok(journey.request('snapshot', path=str(output / 'notification-and-picker.png')))
                picker_accessibility = activate('android-input.txt')
                def recorded(marker):
                    files = list(private.rglob('integration.txt'))
                    return files[0] if len(files) == 1 and marker in files[0].read_text() else None
                wait_for(lambda: recorded('document-read=confirmed'), 'actual approved document read')
                activate('Save')
                activate('Replace')
                acceptance = wait_for(lambda: recorded('complete=true'), 'actual approved document write')
                if destination.read_text() != 'Android document write confirmed\n':
                    raise RuntimeError('host fixture output differs from Android write')
                if digest(source) != initial_source or digest(canary) != initial_canary:
                    raise RuntimeError('read-only document or ungranted sibling changed')
                wait_for(lambda: not notification(), 'actual notification withdrawal')
                client, path, tree = wait_for(lambda: described('Approved Android document status'),
                    'external AT-SPI Android content description')
                accessibility = {'bus_name': client.bus_name, 'path': path, 'tree': tree,
                                 'picker': picker_accessibility}
                (output / 'accessibility.json').write_text(json.dumps(accessibility, indent=2) + '\n')
                require_ok(journey.request('snapshot', path=str(output / 'document-complete.png')))
                journey.close()
                lifecycle = wait_for(lambda: next(iter(private.rglob('integration-lifecycle.txt')), None),
                                     'actual integration lifecycle file')
                wait_for(lambda: lifecycle.read_text().splitlines() == ['pause', 'stop', 'destroy'],
                         'actual integration close lifecycle')
                (output / 'lifecycle.txt').write_text(lifecycle.read_text())
                for identifier in list(grants):
                    journey.request('revoke', id=identifier)
                    grants.remove(identifier)
                (acceptance.parent / 'revoke-fixture').write_text('Only disposable acceptance documents\n')
                journey.launch()
                wait_for(lambda: recorded('revocation-complete=true'), 'revoked Android document authority')
                require_ok(journey.request('snapshot', path=str(output / 'revocation.png')))
                journey.close()
                (output / 'acceptance.txt').write_text(acceptance.read_text())
                log.flush()
                runtime_log = (output / 'compositor.log').read_text()
                if 'reply from authenticated chooser: picked' not in runtime_log or \
                        'reply from authenticated chooser: failed' not in runtime_log:
                    raise RuntimeError('authenticated chooser success/refusal evidence absent')
                if 'landlock: fully enforced' not in runtime_log or 'no_new_privs: true' not in runtime_log:
                    raise RuntimeError('mandatory confinement evidence absent')
                if 'Notifications.Notify:' not in runtime_log or 'caller_ws=Some' not in runtime_log:
                    raise RuntimeError('production notification caller attribution absent')
                result = {'schema': 1, 'completed_at_utc': datetime.now(timezone.utc).isoformat(),
                    'execution': 'headless production Tilo, actual ART, ' + args.architecture + ' software graphics',
                    'production_binaries': {n: digest(REPO / 'target/debug' / n) for n in ('shell', 'tilo-launcher')},
                    'apk_sha256': args.apk_sha256, 'runtime_receipt_sha256': args.runtime_receipt_sha256,
                    'room': room, 'presented_frame': frame, 'notification': delivered,
                    'fixture_document_read': True, 'fixture_document_write': True,
                    'read_only_write_denied': True, 'ungranted_sibling_denied': True,
                    'notification_delivery': True, 'notification_withdrawal': True,
                    'file_grants_revoked': True, 'revoked_relaunch_read_denied': True,
                    'revoked_relaunch_write_denied': True,
                    'external_atspi_description': True,
                    'fixture_grants_only': True, 'real_device': False,
                    'png_sha256': {n: digest(output / n) for n in
                        ('notification-and-picker.png', 'document-complete.png', 'revocation.png')}}
            finally:
                try:
                    if journey is not None and process is not None and process.poll() is None:
                        for identifier in grants:
                            journey.request('revoke', id=identifier)
                finally:
                    try:
                        if process is not None: stop(process)
                    finally:
                        try: stop(bus)
                        finally: stop_bus_helpers(runtime)
        if socket.exists(): raise RuntimeError('owned control socket leaked')
    result['owned_compositor_and_bus_stopped'] = True
    (output / 'journey.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()

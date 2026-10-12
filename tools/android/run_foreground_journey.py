#!/usr/bin/env python3
"""Observe real ART foreground work and revocation of disposable fixture grants.

Evidence is private output, never a source fixture. No user documents or buses.
"""
import argparse
from contextlib import ExitStack
from datetime import datetime, timezone
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib

from qualify_apk import digest, pinned, verify_runtime
from execution_host import add_architecture, native_architecture
from run_arity_journey import REPO, Journey, wait_for, require_ok, checked_output
from run_integration_journey import stop, stop_bus_helpers
from hydration_support import ColdDocumentMount


def probe_notification_ownership(output, bus_address):
    ids = re.findall(r'Notifications.Notify:.*? id=(\d+) caller_ws=Some',
        (output / 'compositor.log').read_text())
    if len(ids) != 2:
        raise RuntimeError('cannot identify the two actual fixture notifications')
    command = ['gdbus', 'call', '--address', bus_address,
        '--dest', 'org.freedesktop.Notifications', '--object-path', '/org/freedesktop/Notifications', '--method']
    # GIO batches in a hash table: SDK notification order does not prove the
    # host id. Exactly the ongoing one must refuse this unrelated sender.
    denied = []
    for identifier in ids:
        close = subprocess.run(command + ['org.freedesktop.Notifications.CloseNotification', identifier],
            text=True, capture_output=True, timeout=10)
        if close.returncode != 0:
            if 'AccessDenied' not in close.stderr: raise RuntimeError('notification ownership probe failed: ' + close.stderr)
            denied.append(int(identifier))
    if len(denied) != 1:
        raise RuntimeError('required ongoing notification did not have exclusive sender ownership')
    foreground_id = denied[0]
    update = subprocess.run(command + ['org.freedesktop.Notifications.Notify', '--', 'Untrusted sender',
        str(foreground_id), '', 'Untrusted ongoing attempt', 'No verified Android launch', '[]',
        "{'category': <'x-tilo.android-ongoing'>}", '-1'], text=True, capture_output=True, timeout=10, check=True)
    numbers = re.findall(r'uint32 (\d+)', update.stdout)
    if len(numbers) != 1 or int(numbers[0]) == foreground_id:
        raise RuntimeError('unrelated bus sender could replace the foreground notification')


def exercise_work(journey, output, profile, package, args, documents, grants, cold=None):
    source, destination = documents
    if cold:
        cold.require_projected()
    frame = journey.launch()
    private = profile / 'data/tilo-rooms' / journey.workspace()['room'] / package
    def recorded(marker):
        paths = list(private.rglob('foreground.txt'))
        return paths[0] if len(paths) == 1 and marker in paths[0].read_text() else None
    record = wait_for(lambda: recorded('foreground=returned'), 'actual startForeground return')
    def notification(title):
        return next((n for n in journey.state()['chrome']['notifications'] if n['title'] == title), None)
    control = wait_for(lambda: notification('Foreground transport control'), 'filtered notification transport')
    (record.parent / 'scope-inputs').write_text(str(source) + '\n' + str(destination) + '\n')
    wait_for(lambda: recorded('scoped-descriptors=open'), 'actual scoped descriptors')
    wait_for(lambda: len(destination.read_text().splitlines()) >= 3, 'actual worker writes')
    if cold:
        # No harness read may hydrate the received file before this observation.
        cold.state(b'local')
        cold.steps.append({'phase': 'actual-foreground-descriptor-read', 'local': True})
    if args.expect_incompatibility:
        if ('zero-id-rejected=false' not in record.read_text()
                or 'null-notification-rejected=false' not in record.read_text()):
            raise RuntimeError('baseline API defect no longer reproduced')
        time.sleep(1)
        if notification('Android foreground fixture'):
            raise RuntimeError('baseline foreground notification defect no longer reproduced')
    else:
        foreground = wait_for(lambda: notification('Android foreground fixture'), 'required foreground notification')
        if not foreground['persistent']:
            raise RuntimeError('foreground notice would expire while service is active')
        if ('zero-id-rejected=true' not in record.read_text()
                or 'null-notification-rejected=true' not in record.read_text()):
            raise RuntimeError('invalid foreground arguments accepted')
        # The probe uses this fixture bus, never the user's session bus.
        probe_notification_ownership(output, 'unix:path=' + str(journey.socket.parent / 'bus.sock'))
        untrusted = wait_for(lambda: notification('Untrusted ongoing attempt'), 'untrusted notification probe')
        if untrusted['persistent'] or not notification('Android foreground fixture'):
            raise RuntimeError('untrusted category gained ongoing authority or changed foreground notice')
        # Outlast the ordinary server-default toast lifetime while
        # the actual worker continues through scoped descriptors.
        time.sleep(12)
        if not notification('Android foreground fixture'):
            raise RuntimeError('active foreground service lost its required notification')
        (record.parent / 'finish-worker').write_text('Stop disposable worker\n')
        wait_for(lambda: recorded('service-destroy;main=true'), 'normal foreground self-stop')
        wait_for(lambda: not notification('Android foreground fixture'), 'normal foreground withdrawal')
        first_ticks = len(destination.read_text().splitlines())
        journey.close()
        (record.parent / 'finish-worker').unlink()
        journey.launch()
        wait_for(lambda: record.read_text().count('scoped-descriptors=open') == 2, 'second scoped worker')
        wait_for(lambda: notification('Android foreground fixture'), 'second foreground notice')
        wait_for(lambda: len(destination.read_text().splitlines()) > first_ticks, 'second actual worker writes')
    require_ok(journey.request('snapshot', path=str(output / 'active-worker.png')))
    before = len(destination.read_text().splitlines())
    for identifier in list(grants):
        if journey.request('revoke', id=identifier).get('answer', {}).get('revoked') is not True:
            raise RuntimeError('fixture grant was not revoked: ' + identifier)
        grants.remove(identifier)
    if args.expect_incompatibility:
        wait_for(lambda: len(destination.read_text().splitlines()) >= before + 5,
            'baseline live write survives revocation')
        after = len(destination.read_text().splitlines())
        journey.close()
    else:
        wait_for(lambda: not journey.tabs(), 'revocation interrupts old sandbox')
        wait_for(lambda: not notification('Android foreground fixture'), 'interrupted foreground withdrawal')
        stopped_bytes = destination.read_bytes()
        after = len(stopped_bytes.splitlines())
        time.sleep(1)
        if destination.read_bytes() != stopped_bytes:
            raise RuntimeError('revoked open descriptor changed exact output bytes')
        if journey.tabs() or record.read_text().count('service-create;main=true') != 2:
            raise RuntimeError('interrupted sticky service restarted without authority')
    settled_output = destination.read_bytes()
    (record.parent / 'revoke-fixture').write_text('Disposable fixture only\n')
    journey.launch()
    wait_for(lambda: recorded('revoked-relaunch=complete;service-not-started'), 'denied revoked relaunch')
    require_ok(journey.request('snapshot', path=str(output / 'revoked-relaunch.png')))
    journey.close()
    if destination.read_bytes() != settled_output:
        raise RuntimeError('revoked relaunch changed the stopped worker output')
    visits = 2 if args.expect_incompatibility else 3
    if source.read_text() != 'Tilo disposable scoped source\n' or (record.parent / 'visits.txt').read_text().splitlines() != ['visit=' + str(n) for n in range(1, visits + 1)]:
        raise RuntimeError('scoped source or private visits changed unexpectedly')
    next_tick, cycles = 1, 0
    for line in destination.read_bytes().splitlines(keepends=True):
        if line == b'tick=1\n': next_tick, cycles = 1, cycles + 1
        if line != ('tick=' + str(next_tick) + '\n').encode():
            raise RuntimeError('foreground output bytes differ from the actual worker sequence')
        next_tick += 1
    if cycles != (1 if args.expect_incompatibility else 2):
        raise RuntimeError('foreground output does not contain the expected worker visits')
    (output / 'foreground.txt').write_text(record.read_text())
    result = {'schema': 1, 'completed_at_utc': datetime.now(timezone.utc).isoformat(),
        'execution': 'production headless Tilo, actual ART, ' + args.architecture + ' software graphics',
        'apk_sha256': args.apk_sha256, 'runtime_receipt_sha256': args.runtime_receipt_sha256,
        'production_binaries': {n: digest(REPO / 'target/debug' / n) for n in ('shell', 'tilo-launcher')},
        'presented_frame': frame, 'control_notification': control,
        'runtime_accepted': not args.expect_incompatibility, 'ticks_before_revoke': before,
        'ticks_after_revoke': after, 'revoked_relaunch_denied': True,
        'private_visits_preserved': True, 'fixture_grants_only': True,
        'worker_output_bytes_verified': True, 'worker_output_sha256': digest(destination), 'real_device': False}
    if not args.expect_incompatibility:
        result.update(foreground_persistent=True, normal_foreground_stop=True,
            interrupted_foreground_withdrawal=True, unauthorized_sticky_restart=False,
            unrelated_notification_close_denied=True, unrelated_notification_replace_denied=True,
            unverified_ongoing_category_denied=True)
    return result


def exercise_denial(journey, output, profile, package, args):
    frame = journey.launch()
    private = profile / 'data/tilo-rooms' / journey.workspace()['room'] / package
    def denied():
        files = list(private.rglob('foreground.txt'))
        return files[0] if len(files) == 1 and 'foreground=denied;no-notification-capability' in files[0].read_text() else None
    record = wait_for(denied, 'foreground admission without notification capability denied')
    wait_for(lambda: 'service-destroy;main=true' in record.read_text(), 'denied service destroyed on main looper')
    if ('foreground=returned' in record.read_text() or 'scoped-descriptors=open' in record.read_text()
            or journey.state()['chrome']['notifications']):
        raise RuntimeError('denied foreground service obtained authority or emitted a notification')
    require_ok(journey.request('snapshot', path=str(output / 'foreground-denied.png')))
    journey.close()
    (output / 'foreground.txt').write_text(record.read_text())
    return {'schema': 1, 'completed_at_utc': datetime.now(timezone.utc).isoformat(),
        'execution': 'production headless Tilo, actual ART, ' + args.architecture + ' software graphics',
        'apk_sha256': args.apk_sha256, 'runtime_receipt_sha256': args.runtime_receipt_sha256,
        'production_binaries': {n: digest(REPO / 'target/debug' / n) for n in ('shell', 'tilo-launcher')},
        'presented_frame': frame, 'foreground_without_notification_capability': 'denied',
        'scoped_worker_started': False, 'additional_grants': False, 'real_device': False}


def main(work_driver=None):
    parser = argparse.ArgumentParser(description=__doc__)
    add_architecture(parser)
    for name in ('payload', 'runtime-receipt', 'runtime-parent', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('apk', 'signer', 'runtime-receipt'):
        parser.add_argument('--' + name + '-sha256', required=True)
    parser.add_argument('--fixture-grants', action='store_true', required=True)
    parser.add_argument('--cold-hydration', action='store_true',
                        help='require an actual cold FUSE source before foreground descriptor reads')
    expectation = parser.add_mutually_exclusive_group()
    expectation.add_argument('--expect-incompatibility', action='store_true')
    expectation.add_argument('--expect-notification-denial', action='store_true')
    if work_driver is not None:
        expectation.add_argument('--expect-sticky-incompatibility', action='store_true')
    args = parser.parse_args()
    if args.cold_hydration and (work_driver is not None or args.expect_incompatibility
                               or args.expect_notification_denial):
        parser.error('cold foreground interruption requires the compatible work journey')
    native_architecture(args.architecture)
    output = checked_output(args.output)
    payload = args.payload.resolve(strict=True)
    pinned(payload / 'app.apk', args.apk_sha256)
    qualification = json.loads((payload / 'qualification.json').read_text())
    if (not qualification.get('signature_verified')
            or qualification.get('signer_sha256') != args.signer_sha256
            or qualification.get('runtime_receipt_sha256') != args.runtime_receipt_sha256
            or bool(qualification.get('notification_provider_declared')) != (not args.expect_notification_denial)):
        raise RuntimeError('fixture signature, receipt or notification declaration differs')
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
    source, destination = output / 'source.txt', output / 'destination.txt'
    source.write_text('Tilo disposable scoped source\n')
    destination.write_text('')
    grants, journey = [], None
    prefix = 'tah-' if args.cold_hydration else 'tilo-android-foreground-'
    with tempfile.TemporaryDirectory(prefix=prefix, dir=parent) as runtime, ExitStack() as stack:
        cold = None
        if args.cold_hydration:
            cold = stack.enter_context(ColdDocumentMount(Path(runtime) / 'f', source, output))
            source = cold.document
        socket, bus_socket = Path(runtime) / 'control.sock', Path(runtime) / 'bus.sock'
        env = {**os.environ, 'XDG_RUNTIME_DIR': runtime, 'TILO_HEADLESS_PROFILE': str(profile),
            'TILO_APPS_DATA_DIR': str(catalogue), 'TILO_LAUNCHER_BIN': str(REPO / 'target/debug/tilo-launcher'),
            'DBUS_SESSION_BUS_ADDRESS': 'unix:path=' + str(bus_socket),
            'LP_NUM_THREADS': '2', 'RAYON_NUM_THREADS': '2', 'RUST_LOG': 'info,shell::notifications=debug'}
        with (output / 'bus.log').open('w') as bus_log, (output / 'compositor.log').open('w') as log:
            bus = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--nopidfile',
                '--address=' + env['DBUS_SESSION_BUS_ADDRESS']], env=env, stdout=bus_log,
                stderr=bus_log, start_new_session=True)
            process = None
            try:
                wait_for(lambda: bus_socket.is_socket(), 'owned bus socket')
                process = subprocess.Popen([str(REPO / 'scripts/headless-examples/spawn-headless.sh'), str(socket)],
                    cwd=REPO, env=env, stdout=log, stderr=log, start_new_session=True)
                wait_for(lambda: socket.is_socket() if process.poll() is None else
                    (_ for _ in ()).throw(RuntimeError('compositor exited; inspect log')), 'control socket')
                journey = Journey(socket, output, package, profile)
                fixture_grants = [] if args.expect_notification_denial else [(source, 'read', 'foreground-fixture-read'),
                    (destination, 'write', 'foreground-fixture-write')]
                for path, verb, identifier in fixture_grants:
                    answer = journey.request('grant', subject='app:' + package,
                        scope='fs:' + str(path), verb=verb, id=identifier).get('answer', {})
                    if answer.get('granted') != identifier:
                        raise RuntimeError('fixture grant was not issued: ' + identifier)
                    grants.append(identifier)
                if args.expect_notification_denial:
                    result = exercise_denial(journey, output, profile, package, args)
                elif work_driver is not None:
                    result = work_driver(journey, output, profile, package, args, (source, destination), grants)
                else:
                    result = exercise_work(journey, output, profile, package, args, (source, destination), grants, cold)
                if cold:
                    cold.verify_restored(phase='interrupted-foreground-source-verified')
                    result['cold_hydration'] = cold.report()
                log.flush()
                text = (output / 'compositor.log').read_text()
                if 'landlock: fully enforced' not in text or 'no_new_privs: true' not in text:
                    raise RuntimeError('mandatory confinement evidence absent')
                if work_driver is None and 'foreground sticky recovery pending' in text:
                    raise RuntimeError('normal stop or withdrawal incorrectly entered recovery')
            finally:
                try:
                    if journey is not None and process is not None and process.poll() is None:
                        for identifier in grants: journey.request('revoke', id=identifier)
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

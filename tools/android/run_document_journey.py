#!/usr/bin/env python3
"""Exercise one pinned APK against baseline and fixed private document runtimes.

Only newly created fixture files can receive grants. No user data or live buses.
"""
import argparse
from contextlib import ExitStack
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib

from qualify_apk import digest, pinned, verify_runtime
from execution_host import add_architecture, native_architecture
from run_arity_journey import REPO, Journey, checked_output, wait_for, require_ok
from run_integration_journey import stop, stop_bus_helpers
from journey_support import AccessiblePicker
from hydration_support import ColdDocumentMount


def revoke_fixture(journey, identifier):
    answer = journey.request('revoke', id=identifier).get('answer', {})
    if answer.get('revoked') is not True:
        raise RuntimeError('fixture grant was not revoked: ' + identifier)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    add_architecture(parser)
    for name in ('payload', 'runtime-receipt', 'runtime-parent', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('apk', 'signer', 'runtime-receipt'):
        parser.add_argument('--' + name + '-sha256', required=True)
    parser.add_argument('--fixture-grants', action='store_true', required=True)
    parser.add_argument('--expect-incompatibility', action='store_true',
                        help='require the known baseline API failures; never classify it compatible')
    parser.add_argument('--cold-hydration', action='store_true',
                        help='require real FUSE backend eviction and actual APK relaunch restoration')
    args = parser.parse_args()
    native_architecture(args.architecture)
    if args.cold_hydration and args.expect_incompatibility:
        parser.error('cold restoration requires the compatible runtime')
    output = checked_output(args.output)
    payload = args.payload.resolve(strict=True)
    pinned(payload / 'app.apk', args.apk_sha256)
    qualification = json.loads((payload / 'qualification.json').read_text())
    if not qualification.get('signature_verified') or qualification.get('signer_sha256') != args.signer_sha256:
        raise RuntimeError('fixture signature qualification does not match')
    if qualification['runtime_receipt_sha256'] != args.runtime_receipt_sha256:
        raise RuntimeError('qualification runtime differs')
    verify_runtime(payload / 'runtime', args.runtime_receipt, args.runtime_receipt_sha256, args.architecture)
    parent = args.runtime_parent.resolve(strict=True)
    if parent.stat().st_uid != os.geteuid() or parent.stat().st_mode & 0o077:
        raise RuntimeError('runtime parent must be private and user-owned')
    package = tomllib.loads((payload / 'tilo.toml').read_text())['package']['id']
    output.mkdir(mode=0o700)
    profile = output / 'profile'
    catalogue = profile / 'data/tilo-apps'
    catalogue.mkdir(parents=True)
    (catalogue / 'registry.toml').write_text('[[entry]]\nid = ' + json.dumps(package)
        + '\nversion = "0.0.0"\npayload_root = ' + json.dumps(str(payload))
        + '\nmanifest_path = ' + json.dumps(str(payload / 'tilo.toml')) + '\n')
    fixtures = output / 'fixtures'
    fixtures.mkdir()
    source, destination, canary = [fixtures / name for name in
        ('android-π-📄.txt', 'android-modes.txt', 'private-canary')]
    source.write_text('Tilo Android document metadata\n')
    destination.write_text('Untouched disposable output\n')
    canary.write_text('Ungrantable sibling fixture\n')
    initial = {name: digest(path) for name, path in
               (('source', source), ('destination', destination), ('canary', canary))}
    grants = []
    journey = None
    result = None
    prefix = 'tah-' if args.cold_hydration else 'tilo-android-documents-'
    with tempfile.TemporaryDirectory(prefix=prefix, dir=parent) as runtime, ExitStack() as stack:
        cold = None
        if args.cold_hydration:
            cold = stack.enter_context(ColdDocumentMount(Path(runtime) / 'f', source, output))
            source = cold.document
        socket, bus_socket = Path(runtime) / 'control.sock', Path(runtime) / 'session-bus.sock'
        env = {**os.environ, 'XDG_RUNTIME_DIR': runtime, 'TILO_HEADLESS_PROFILE': str(profile),
               'TILO_APPS_DATA_DIR': str(catalogue), 'TILO_LAUNCHER_BIN': str(REPO / 'target/debug/tilo-launcher'),
               'DBUS_SESSION_BUS_ADDRESS': 'unix:path=' + str(bus_socket),
               'LP_NUM_THREADS': '2', 'RAYON_NUM_THREADS': '2'}
        with (output / 'bus.log').open('w') as bus_log, (output / 'compositor.log').open('w') as log:
            bus = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--nopidfile',
                '--address=' + env['DBUS_SESSION_BUS_ADDRESS']], env=env, stdout=bus_log,
                stderr=bus_log, start_new_session=True)
            process = None
            try:
                wait_for(lambda: bus_socket.is_socket(), 'owned bus socket')
                picker = AccessiblePicker(env['DBUS_SESSION_BUS_ADDRESS'])
                process = subprocess.Popen([str(REPO / 'scripts/headless-examples/spawn-headless.sh'), str(socket)],
                    cwd=REPO, env=env, stdout=log, stderr=log, start_new_session=True)
                wait_for(lambda: socket.is_socket() if process.poll() is None else
                    (_ for _ in ()).throw(RuntimeError('compositor exited; inspect log')), 'control socket')
                journey = Journey(socket, output, package, profile)
                for path, verb, identifier in [(source, 'read', 'android-document-read'),
                                              (destination, 'write', 'android-document-write')]:
                    answer = journey.request('grant', subject='app:' + package,
                        scope='fs:' + str(path), verb=verb, id=identifier).get('answer', {})
                    if answer.get('granted') != identifier:
                        raise RuntimeError('fixture grant was not issued: ' + identifier)
                    grants.append(identifier)
                room = journey.workspace()['room']
                private = profile / 'data/tilo-rooms' / room / package
                if cold:
                    # Fixture input is authored before launch, not inferred from
                    # ContentResolver's answer or a private runtime shortcut.
                    files = private / '.local/share/android_translation_layer/app.apk_/files'
                    files.mkdir(parents=True, exist_ok=True)
                    (files / 'expected-document-name.txt').write_text(source.name)
                frame = journey.launch()
                def recorded(marker):
                    files = list(private.rglob('documents.txt'))
                    return files[0] if len(files) == 1 and marker in files[0].read_text() else None
                picker.activate(source.name, before_click=cold.require_projected if cold else None)
                acceptance = wait_for(lambda: recorded('source-read=confirmed'), 'actual approved document read')
                picker.activate('Save')
                picker.activate('Replace')
                wait_for(lambda: recorded('complete='), 'actual document metadata and mode results')
                lines = acceptance.read_text().splitlines()
                if args.expect_incompatibility:
                    expected = ['metadata-failure=java.lang.NullPointerException',
                                'write-failure=java.lang.NoSuchMethodError', 'complete=false']
                    if not all(line in lines for line in expected):
                        raise RuntimeError('baseline did not exhibit the known API failures: ' + repr(lines))
                    if digest(destination) != initial['destination']:
                        raise RuntimeError('baseline changed destination despite missing API')
                else:
                    name_check = 'exact-record-name' if cold else 'unicode-name'
                    expected = ['metadata=' + name_check + ',byte-size,projection,cancellation,bounds',
                        'write-modes=wt,wa,rw,rwt;invalid-denied',
                        'isolation=read-only-modes,sibling-read,sibling-metadata-denied', 'complete=true']
                    if not all(line in lines for line in expected) or any('failure=' in line for line in lines):
                        raise RuntimeError('document APIs did not pass: ' + repr(lines))
                    if destination.read_bytes() != b'final\n':
                        raise RuntimeError('actual output bytes differ')
                saved = list(private.rglob('document-uris.txt'))
                if not args.expect_incompatibility and len(saved) != 1:
                    raise RuntimeError('missing actual private document URIs')
                private_uris = saved[0].read_bytes() if saved else None
                if digest(source) != initial['source'] or digest(canary) != initial['canary']:
                    raise RuntimeError('source or ungranted sibling bytes changed')
                _, _, tree = wait_for(lambda: picker.described('Android document resource status'),
                    'actual external Android accessible description')
                (output / 'accessibility.json').write_text(json.dumps(tree, indent=2) + '\n')
                require_ok(journey.request('snapshot', path=str(output / 'document-results.png')))
                journey.close()
                wait_for(lambda: acceptance.read_text().splitlines()[-3:] ==
                    ['lifecycle=pause', 'lifecycle=stop', 'lifecycle=destroy'], 'actual close lifecycle')
                if cold:
                    cold.evict_and_restart()
                    journey.launch()
                    picker.activate(source.name, before_click=cold.require_projected)
                    wait_for(lambda: acceptance.read_text().splitlines().count('source-read=confirmed') == 2,
                             'actual cold APK relaunch source read')
                    picker.activate('Save')
                    picker.activate('Replace')
                    wait_for(lambda: acceptance.read_text().splitlines().count('complete=true') == 2,
                             'actual cold APK relaunch metadata and output modes')
                    restored = acceptance.read_text().splitlines()
                    if any('failure=' in line for line in restored) or any(restored.count(line) != 2 for line in expected):
                        raise RuntimeError('cold relaunch did not repeat the complete document assertions')
                    if digest(source) != initial['source'] or destination.read_bytes() != b'final\n':
                        raise RuntimeError('cold restored source/output bytes differ')
                    if saved[0].read_bytes() != private_uris:
                        raise RuntimeError('private document URIs changed during cold relaunch')
                    require_ok(journey.request('snapshot', path=str(output / 'cold-restored.png')))
                    journey.close()
                    wait_for(lambda: acceptance.read_text().splitlines().count('lifecycle=destroy') == 2,
                             'actual cold relaunch close')
                    cold.verify_restored()
                for identifier in list(grants):
                    revoke_fixture(journey, identifier); grants.remove(identifier)
                if not args.expect_incompatibility:
                    saved = list(private.rglob('document-uris.txt'))
                    if len(saved) != 1: raise RuntimeError('missing actual saved document URIs')
                    (saved[0].parent / 'documents-revoke-test.txt').write_text('owned test marker\n')
                    journey.launch()
                    wait_for(lambda: recorded('revocation-complete=true'), 'actual revoked document API denials')
                    if 'failure=' in acceptance.read_text(): raise RuntimeError('revoked authority accepted')
                    if saved[0].read_bytes() != private_uris:
                        raise RuntimeError('revoked relaunch changed private document URIs')
                    require_ok(journey.request('snapshot', path=str(output / 'document-revocation.png')))
                    journey.close()
                    closes = 3 if cold else 2
                    wait_for(lambda: acceptance.read_text().splitlines().count('lifecycle=destroy') == closes,
                             'actual relaunch close')
                    if destination.read_bytes() != b'final\n':
                        raise RuntimeError('revoked relaunch changed exact output bytes')
                if digest(source) != initial['source'] or digest(canary) != initial['canary']:
                    raise RuntimeError('source or sibling bytes changed after withdrawal')
                (output / 'acceptance.txt').write_text(acceptance.read_text())
                log.flush()
                runtime_log = (output / 'compositor.log').read_text()
                for evidence in ('reply from authenticated chooser: picked',
                                 'landlock: fully enforced', 'no_new_privs: true'):
                    if evidence not in runtime_log: raise RuntimeError('missing production evidence: ' + evidence)
                result = {'schema': 1, 'completed_at_utc': datetime.now(timezone.utc).isoformat(),
                    'execution': 'headless production Tilo, actual ART, ' + args.architecture + ' software graphics',
                    'apk_sha256': args.apk_sha256, 'runtime_receipt_sha256': args.runtime_receipt_sha256,
                    'production_binaries': {n: digest(REPO / 'target/debug' / n) for n in ('shell', 'tilo-launcher')},
                    'baseline_incompatibility_observed': args.expect_incompatibility,
                    'document_resource_apis_qualified': not args.expect_incompatibility,
                    'fixture_grants_revoked': True, 'source_and_canary_unchanged': True,
                    'ungranted_sibling_exists': not args.cold_hydration,
                    'revoked_metadata_reads_writes_denied': not args.expect_incompatibility,
                    'private_document_uris_preserved': not args.expect_incompatibility,
                    'real_device': False, 'presented_frame': frame,
                    'png_sha256': {p.name: digest(p) for p in output.glob('*.png')}}
                if cold: result['cold_hydration'] = cold.report()
            finally:
                try:
                    if journey:
                        for identifier in grants: revoke_fixture(journey, identifier)
                finally:
                    try:
                        if process: stop(process)
                    finally:
                        try: stop(bus)
                        finally: stop_bus_helpers(runtime)
        if socket.exists(): raise RuntimeError('owned compositor left a control socket')
    if result is None: raise RuntimeError('document journey produced no observed result')
    result['owned_compositor_and_bus_stopped'] = True
    (output / 'journey.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))


if __name__ == '__main__':
    main()

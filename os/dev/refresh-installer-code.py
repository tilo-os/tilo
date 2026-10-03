#!/usr/bin/env python3
"""Send a tiny helper refresh to a running tilo-vm Windows install scenario.

The medium stays unchanged. Every resulting install is developer test evidence
and is rejected by Windows-direct physical admission.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import time

REPO = Path(__file__).resolve().parents[2]
MODULE = REPO / 'os/usr/libexec/tilo-installer-code-protocol.py'
spec = importlib.util.spec_from_file_location('installer_code_protocol', MODULE)
protocol = importlib.util.module_from_spec(spec)
spec.loader.exec_module(protocol)


def _refresh(source, port, receipt_path, timeout=45):
    revision = subprocess.run(['git', '-C', str(source), 'rev-parse', 'HEAD'],
                              capture_output=True, check=True, text=True).stdout.strip()
    dirty = bool(subprocess.run(['git', '-C', str(source), 'status', '--porcelain', '--', 'os/usr/libexec', 'os/dev/hardware_profile.py'],
                               capture_output=True, check=True).stdout)
    channel_path = receipt_path.parent / 'installer-code-channel.json'
    channel = json.loads(channel_path.read_text())
    protocol.require(channel.get('schema') == 'tilo.installer-code-runtime.v1'
                     and channel.get('enabled') is True and channel.get('port') == port,
                     'code refresh requires its scenario-owned opted-in channel receipt')
    history_path = receipt_path.parent / 'installer-code-history.json'
    history = json.loads(history_path.read_text()) if history_path.exists() else []
    protocol.require(type(history) is list and len(history) < 32, 'invalid or exhausted refresh history')
    raw = protocol.bundle(source, revision, dirty)
    _, expected = protocol.validate(raw)
    # Record the host attempt BEFORE the guest can execute local code. A failed
    # or deceptive guest cannot erase the physical-admission exclusion.
    attempt = {'requested': expected, 'acknowledged': False}
    bootstrap = receipt_path.parent / 'installer-bootstrap.json'
    if bootstrap.exists() or bootstrap.is_symlink():
        protocol.require(bootstrap.is_file() and not bootstrap.is_symlink() and bootstrap.stat().st_size <= 65536,
                         'unsafe host bootstrap provenance')
        seeded = json.loads(bootstrap.read_text())
        protocol.require(seeded.get('schema') == 'tilo.installer-code-bootstrap.v1'
                         and seeded.get('physical_admission') is False,
                         'invalid host bootstrap provenance')
        attempt['bootstrap'] = seeded
    fixture = receipt_path.parent / 'installer-code-uki-fixture.json'
    if fixture.exists() or fixture.is_symlink():
        protocol.require(fixture.is_file() and not fixture.is_symlink() and fixture.stat().st_size <= 16384,
                         'unsafe host UKI fixture provenance')
        fixture_record = json.loads(fixture.read_text())
        protocol.require(fixture_record.get('schema') == 'tilo.installer-code-uki-fixture.v1'
                         and fixture_record.get('physical_admission') is False
                         and fixture_record.get('readonly') is True,
                         'invalid host UKI fixture provenance')
        attempt['uki_fixture'] = fixture_record
    history.append(attempt)
    history_path.write_text(json.dumps(history, indent=2) + '\n')
    deadline = time.monotonic() + timeout
    while True:
        try:
            client = socket.create_connection(('127.0.0.1', port), timeout=3)
            break
        except OSError:
            if time.monotonic() >= deadline:
                raise ValueError('installer code channel did not become ready')
            time.sleep(0.1)
    with client:
        client.settimeout(max(1, deadline - time.monotonic()))
        if bootstrap.exists():
            # QEMU accepts host TCP before the guest virtio port is open.
            # Retry a small resynchronizable hello; send file framing only
            # after the guest acknowledges this connection.
            greeting = b''
            while time.monotonic() < deadline:
                client.sendall(b'TILO-CODE-HELLO-1\n')
                client.settimeout(min(1, max(0.1, deadline-time.monotonic())))
                try:
                    greeting += client.recv(1025)
                except socket.timeout:
                    continue
                protocol.require(len(greeting) <= 4096, 'oversized bootstrap greeting')
                lines = greeting.split(b'\n')
                greeting = lines[-1]
                if b'{"schema":"tilo.installer-code-ready.v1"}' in lines[:-1]:
                    break
            else:
                raise ValueError('bootstrap code channel did not announce readiness')
            client.settimeout(max(1, deadline-time.monotonic()))
        stream = client.makefile('rwb', buffering=0)
        protocol.send(stream, raw)
        for attempt in range(64):
            response = stream.readline(65537)
            if response != b'{"schema":"tilo.installer-code-ready.v1"}\n':
                break
        protocol.require(0 < len(response) <= 65536, 'oversized or missing refresh response')
        result = json.loads(response)
        protocol.require(type(result) is dict, 'invalid refresh response object')
        protocol.require(result.get('ok') is True, result.get('error', 'code refresh failed'))
        protocol.require(result.get('receipt') == expected, 'guest refresh receipt differs from sent code')
    runtime = result.get('runtime')
    protocol.require('uki_fixture' not in history[-1] or runtime is not None,
                     'guest omitted required read-only fixture evidence')
    if runtime is not None:
        protocol.require(type(runtime) is dict and runtime.get('schema') == 'tilo.installer-code-runtime-probe.v1',
                         'invalid guest runtime probe')
        if 'uki_fixture' in history[-1]:
            reported = runtime.get('backend', {}).get('cached_target', {}).get('mount', {}).get('uki_fixture', {})
            protocol.require(reported.get('sha256') == history[-1]['uki_fixture']['sha256']
                             and reported.get('bytes') == history[-1]['uki_fixture']['bytes']
                             and reported.get('physical_admission') is False,
                             'guest UKI fixture differs from host read-only input')
        history[-1]['runtime'] = runtime
    history[-1]['acknowledged'] = True
    history_path.write_text(json.dumps(history, indent=2) + '\n')
    receipt_path.write_text(json.dumps(expected, indent=2) + '\n')
    return expected


def refresh(source, port, receipt_path, timeout=45):
    # The driver closes this host-owned window BEFORE issuing any installer
    # action. An exclusive create serializes that transition with transports.
    lock_path = receipt_path.parent / 'installer-code-window.lock'
    try:
        descriptor = os.open(lock_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError as error:
        raise ValueError('installer code window is busy') from error
    try:
        phase = json.loads((receipt_path.parent / 'installer-code-phase.json').read_text())
        protocol.require(phase.get('phase') in ('startup', 'paused'),
                         'installer actions have started; code refresh window is closed')
        if phase['phase'] == 'paused':
            protocol.require(type(phase.get('resume')) is str and not Path(phase['resume']).exists(),
                             'continue-install requested; code refresh window is closed')
        return _refresh(source, port, receipt_path, timeout)
    finally:
        os.close(descriptor)
        lock_path.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--scenario', type=Path, help='retained scenario vm-cycles directory for hot refresh')
    parser.add_argument('--port', type=int)
    parser.add_argument('--receipt', type=Path)
    args = parser.parse_args()
    try:
        if args.scenario is not None:
            protocol.require(args.port is None and args.receipt is None, 'use --scenario or explicit port/receipt')
            channel = json.loads((args.scenario / 'installer-code-channel.json').read_text())
            args.port = channel.get('port')
            args.receipt = args.scenario / 'installer-code-refresh.json'
        protocol.require(type(args.port) is int and args.receipt is not None, 'scenario or port and receipt required')
        protocol.require(1024 <= args.port <= 65535, 'local unprivileged code port required')
        print(json.dumps(refresh(args.source, args.port, args.receipt)))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f'installer code refresh: {error}\n')


if __name__ == '__main__':
    main()

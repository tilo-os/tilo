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
    dirty = bool(subprocess.run(['git', '-C', str(source), 'status', '--porcelain', '--', 'os/usr/libexec'],
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
    history.append({'requested': expected, 'acknowledged': False})
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
        stream = client.makefile('rwb', buffering=0)
        protocol.send(stream, raw)
        response = stream.readline(65537)
        protocol.require(0 < len(response) <= 65536, 'oversized or missing refresh response')
        result = json.loads(response)
        protocol.require(type(result) is dict, 'invalid refresh response object')
        protocol.require(result.get('ok') is True, result.get('error', 'code refresh failed'))
        protocol.require(result.get('receipt') == expected, 'guest refresh receipt differs from sent code')
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

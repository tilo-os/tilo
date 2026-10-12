#!/usr/bin/env python3
"""Sign receipt-backed private runtime apps on owned copies, without launching them."""
from __future__ import annotations

import sys

if sys.version_info < (3, 11):
    raise SystemExit('staged signing requires Python 3.11+; invoke an existing compatible interpreter explicitly')

import argparse
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tempfile

import collection as c
import collection_local_apps as apps
import collection_local_runtime as runtime
import collection_native as native
from collection_signing_macho import compare_signed_macho


def checked_json(path, expected):
    if not re.fullmatch(r'[0-9a-f]{64}', expected):
        raise ValueError('an exact SHA-256 is required')
    path = canonical(path)
    if path.stat().st_size > 16 * 1024**2 or c.digest(path) != expected:
        raise ValueError(f'changed or oversized input: {path}')
    return json.loads(path.read_text())


def canonical(path):
    path = Path(path).absolute()
    if any(p.is_symlink() for p in (path, *path.parents)):
        raise ValueError(f'symlink input/output path: {path}')
    return path.resolve()


def selection(manifest, expected):
    manifest = canonical(manifest)
    data = checked_json(manifest, expected)
    if data['format'] != 1 or not isinstance(data['apps'], list):
        raise ValueError('unsupported signing manifest')
    root = canonical(data['source_root'])
    apps.source_check(root, data['source'])
    c.app_catalog = apps.source_catalog(root)
    catalog = c.make_plan(root, '', 'arm64', 'distribution')
    holds = checked_json(Path(data['holds']['path']), data['holds']['sha256'])['apps']
    eligible = {entry['name']: entry for entry in catalog['apps']}
    if set(holds) - eligible.keys():
        raise ValueError('hold names must be in the eligible source catalog')
    names = [item['name'] for item in data['apps']]
    if len(names) != len(set(names)) or set(names) != eligible.keys() - holds.keys():
        raise ValueError('selection must contain every eligible app exactly once, excluding all holds')
    records = []
    directories = set()
    for item in sorted(data['apps'], key=lambda item: item['name']):
        directory = canonical(item['directory'])
        if directory in directories:
            raise ValueError('duplicate staged input directory')
        directories.add(directory)
        record = checked_json(directory / 'receipt.json', item['receipt_sha256'])
        plan = record['plan']
        if (plan['source'] != data['source'] or canonical(plan['source_root']) != root or
                plan['app'] != eligible[item['name']] or plan['signed'] is not False):
            raise ValueError('unsigned receipt differs from the pinned source catalog')
        c.check_receipt(directory, plan, 'app-' + item['name'])
        records.append({'directory': str(directory), 'receipt_sha256': item['receipt_sha256'],
                        'record': record})
    return {'format': 1, 'manifest': str(manifest), 'manifest_sha256': expected,
            'source': data['source'], 'holds': data['holds'], 'held_apps': sorted(holds),
            'excluded': catalog['excluded'], 'apps': records,
            'mode': 'distribution', 'architecture': 'arm64', 'notarized': False}


def unsigned(item):
    directory = Path(item['directory'])
    record = checked_json(directory / 'receipt.json', item['receipt_sha256'])
    if record != item['record']:
        raise ValueError('unsigned receipt changed')
    plan = record['plan']
    c.check_receipt(directory, plan, record['stage'])
    app = directory / plan['app']['bundle']
    facts = apps.inspect_app(app, plan)
    for key in ('runtime', 'app', 'code_objects', 'source'):
        if facts[key] != record['facts'][key]:
            raise ValueError('unsigned app metadata changed')
    policy = directory / app.with_suffix('.entitlements').name
    native.read_entitlements(policy)
    if c.digest(policy) != record['facts']['entitlements_sha256']:
        raise ValueError('unsigned entitlement policy changed')
    inventory = apps.signing_inventory(app, policy, facts)
    if json.loads((directory / 'signing-inventory.json').read_text()) != inventory:
        raise ValueError('signing inventory differs from derived code and policy')
    return app, policy, facts, inventory


def policies(app, policy, inventory):
    main = 'Contents/MacOS/' + plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleExecutable']
    value = native.read_entitlements(policy)
    result = {relative: {} for relative in inventory['nested_code_in_order']}
    for relative, filename in inventory['nested_process_entitlements'].items():
        if relative not in result or filename != policy.name:
            raise ValueError('unexpected per-process entitlement assignment')
        result[relative] = value
    if main in result:
        raise ValueError('outer executable also present in nested signing order')
    result[main] = value
    return result


def verify_entitlements(app, mapping):
    for relative, expected in mapping.items():
        output = native.run(['/usr/bin/codesign', '-d', '--entitlements', '-', '--xml',
                             app / relative], combine=False)
        actual = plistlib.loads(output.encode()) if output.strip() else {}
        if actual != expected:
            raise ValueError(f'signed entitlements differ: {relative}')


def sign_code(app, policy, inventory, timeout):
    # Explicit empty policy replaces any pre-existing entitlement metadata.
    empty = app.parent / 'empty.entitlements'
    empty.write_bytes(plistlib.dumps({}))
    mapping = policies(app, policy, inventory)
    common = ['/usr/bin/codesign', '--force', '--sign', inventory['identity']['sha1'],
              '--options', 'runtime', '--timestamp']
    for relative in inventory['nested_code_in_order']:
        runtime.floor(app.parent)
        native.run(common + ['--entitlements', policy if mapping[relative] else empty,
                             app / relative], timeout=timeout)
    runtime.floor(app.parent)
    native.run(common + ['--entitlements', policy, app], timeout=timeout)
    empty.unlink()


def verify_copy(original, signed, plan, facts, policy, order, identity):
    before, after = c.inventory(original), c.inventory(signed)
    main, _, info, code = native.inspect_bundle_metadata(signed, plan['app'])
    main_relative = str(main.relative_to(signed))
    code_paths = set(order['nested_code_in_order']) | {main_relative}
    if {str(path.relative_to(signed)) for path in code} != code_paths:
        raise ValueError('signed code set changed')
    seal = {'Contents/_CodeSignature', 'Contents/_CodeSignature/CodeResources'}
    if set(after) != set(before) | seal or set(before) & seal:
        raise ValueError('signed bundle layout changed beyond the resource seal')
    if after['Contents/_CodeSignature'] != {'directory': True}:
        raise ValueError('invalid signature directory')
    if 'sha256' not in after['Contents/_CodeSignature/CodeResources']:
        raise ValueError('resource seal must be a regular file')
    for relative, entry in before.items():
        if relative in code_paths:
            if entry.get('mode') != after[relative].get('mode'):
                raise ValueError('signed code mode changed')
            compare_signed_macho((original / relative).read_bytes(), (signed / relative).read_bytes())
        elif entry != after[relative]:
            raise ValueError(f'signed resource or layout changed: {relative}')
    if info['LSMinimumSystemVersion'] != '27.0':
        raise ValueError('private deployment floor changed')
    actual = runtime.inspect_payload(signed, plan['runtime_plan'], (main_relative,))
    if set(actual) != set(facts['runtime']):
        raise ValueError('signed runtime closure changed')
    for relative, item in actual.items():
        if item['slices'] != facts['runtime'][relative]['slices']:
            raise ValueError('signed runtime load commands changed')
    if runtime.image_info(main, 'arm64', 15)['slices'] != facts['app']['slices']:
        raise ValueError('signed app load commands changed')
    native.verify_signatures(signed, identity, 'distribution', 'arm64')
    verify_entitlements(signed, policies(signed, policy, order))


def signed_plan(plan, item, identity):
    return {'format': 1, 'source': plan['source'], 'manifest_sha256': plan['manifest_sha256'],
            'unsigned_receipt_sha256': item['receipt_sha256'], 'identity': identity,
            'mode': 'distribution', 'architecture': 'arm64', 'notarized': False,
            'runtime_qualified': False}


def current_inputs(plan, item):
    checked_json(Path(plan['manifest']), plan['manifest_sha256'])
    checked_json(Path(plan['holds']['path']), plan['holds']['sha256'])
    apps.source_check(Path(item['record']['plan']['source_root']), plan['source'])


def process_app(out, plan, item, identity, resume, timeout, verify_only=False):
    original, policy, facts, order = unsigned(item)
    app_plan = item['record']['plan']
    label = 'signed-' + app_plan['app']['name']
    lineage = signed_plan(plan, item, identity)
    if verify_only:
        c.check_receipt(out / label, lineage, label)
        verify_copy(original, out / label / original.name, app_plan, facts, policy, order, identity)
        current_inputs(plan, item)
        return
    # All operations mutate transaction-owned copies. Published stages are never overwritten.
    needed = sum(path.stat().st_size for path in original.rglob('*') if path.is_file())
    if shutil.disk_usage(out.parent).free < 20 * 1024**3 + needed + 64 * 1024**2:
        raise ValueError('insufficient copy headroom above the 20 GiB floor')
    with c.transaction(out, label, resume, lineage) as work:
        if work is None:
            verify_copy(original, out / label / original.name, app_plan, facts, policy, order, identity)
            current_inputs(plan, item)
            return
        shutil.copytree(original, work / original.name, symlinks=True)
        shutil.copy2(policy, work / policy.name)
        copied = work / original.name
        # Recheck source and copied bytes immediately before invoking codesign.
        c.check_receipt(Path(item['directory']), app_plan, item['record']['stage'])
        if c.inventory(copied) != c.inventory(original) or c.digest(work / policy.name) != c.digest(policy):
            raise ValueError('copy differs from verified unsigned inputs')
        print(f'Signing {app_plan["app"]["name"]}: {len(order["nested_code_in_order"])} nested objects then app', flush=True)
        sign_code(copied, work / policy.name, {**order, 'identity': identity}, timeout)
        verify_copy(original, copied, app_plan, facts, policy, order, identity)
        c.check_receipt(Path(item['directory']), app_plan, item['record']['stage'])
        current_inputs(plan, item)
        c.write_receipt(work, lineage, label, {'code_objects': facts['code_objects'],
                        'identity': identity, 'notarized': False, 'runtime_qualified': False,
                        'installed': False, 'signing_inventory': order})


@contextmanager
def run_lock(out):
    out.mkdir(parents=True, exist_ok=True)
    path = out / '.staged-signing.lock'
    canonical(path)
    with path.open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError('another staged signer owns this output') from None
        yield


def process_fleet(args, plan, out):
    if args.stage == 'verify':
        # Verify needs only the pinned public identity; no Keychain access.
        result = json.loads(canonical(out / 'signing-result.json').read_text())
        stored = result['identity']
        identity = native.select_identity(f'{stored["sha1"]} "{stored["name"]}"', args.identity, 'distribution')
        if stored != identity or result['manifest_sha256'] != plan['manifest_sha256']:
            raise ValueError('verification manifest or identity changed')
    else:
        identity = native.select_identity(native.run(['/usr/bin/security', 'find-identity', '-v', '-p', 'codesigning']),
                                          args.identity, 'distribution')
    for item in plan['apps']:
        current_inputs(plan, item)
        process_app(out, plan, item, identity, args.resume, args.timeout, args.stage == 'verify')
    result = {'manifest_sha256': plan['manifest_sha256'], 'identity': identity,
              'apps': [item['record']['plan']['app']['name'] for item in plan['apps']],
              'held_apps': plan['held_apps'], 'developer_id_signed': True,
              'runtime_qualified': False, 'notarized': False, 'installed': False}
    if args.stage == 'sign':
        with tempfile.NamedTemporaryFile(mode='w', prefix='.signing-result-', dir=out, delete=False) as stream:
            pending = Path(stream.name)
            try:
                stream.write(json.dumps(result, indent=2) + '\n')
                stream.flush()
                os.fsync(stream.fileno())
                pending.replace(out / 'signing-result.json')
            finally:
                pending.unlink(missing_ok=True)
    print(json.dumps(result, indent=2))


def execute(args):
    plan = selection(args.manifest, args.manifest_sha256)
    out = canonical(args.out)
    inputs = [Path(item['directory']) for item in plan['apps']]
    inputs += [Path(plan['manifest']), Path(plan['holds']['path'])]
    if any(out == path or out.is_relative_to(path) or path.is_relative_to(out) for path in inputs):
        raise ValueError('output overlaps pinned inputs')
    if args.stage == 'plan':
        print(json.dumps({'apps': [item['record']['plan']['app']['name'] for item in plan['apps']],
                          'held_apps': plan['held_apps'], 'excluded': plan['excluded'],
                          'manifest_sha256': plan['manifest_sha256'], 'out': str(out),
                          'signed': False, 'runtime_qualified': False}, indent=2))
        return
    with run_lock(out):
        process_fleet(args, plan, out)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('stage', choices=['plan', 'sign', 'verify'])
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--manifest-sha256', dest='manifest_sha256', required=True)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--identity', required=True, help='exact existing Developer ID Application identity')
    parser.add_argument('--resume', action='store_true')
    parser.add_argument('--timeout', type=int, default=120, choices=range(30, 301), metavar='30..300')
    args = parser.parse_args(argv)
    try:
        execute(args)
        return 0
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f'staged signing: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(main())

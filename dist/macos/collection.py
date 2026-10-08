#!/usr/bin/env python3
"""Explicit, local SDK app collection stages. See collection.md."""
from __future__ import annotations

import sys

if sys.version_info < (3, 11):
    raise SystemExit('macOS collection requires Python 3.11+; invoke an existing compatible interpreter explicitly')

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/apps'))
from app_release import app_catalog

SPECIAL = {
    'browser': 'Requires scripts/browser/package.py: CEF engine, helpers and per-process JIT entitlement qualification; generic SDK bundle is incomplete.',
    'imager': 'Requires apps/imager/packaging/package-host.sh: privileged writer and host assets; generic SDK bundle is not the host installer.',
}


def safe_name(value):
    if not value or value in {'.', '..'} or any(c in value for c in '/\\\n\r\0'):
        raise ValueError(f'unsafe bundle or app name: {value!r}')
    return value


def make_plan(root, selection, arch, mode):
    catalog = app_catalog(root)
    requested = set(selection.split(',')) if selection else set(catalog)
    if requested - catalog.keys():
        raise ValueError('unknown app: ' + ', '.join(sorted(requested - catalog.keys())))
    apps, excluded, names, ids = [], [], set(), set()
    for name, entry in catalog.items():
        manifest = root / entry['manifest']
        data = tomllib.loads(manifest.read_text())
        package = data['package']
        reason = None
        if 'macos' not in package.get('platforms', ['linux', 'macos', 'web']):
            reason = package.get('platform-reason', 'Manifest does not support macOS')
        elif name in SPECIAL:
            reason = SPECIAL[name]
        if reason:
            excluded.append({'name': name, 'id': entry['id'], 'reason': reason})
            if selection and name in requested:
                raise ValueError(f'{name}: {reason}')
            continue
        if name not in requested:
            continue
        display = safe_name(package.get('name', entry['package']))
        safe_name(name)
        bundle = display + '.app'
        if bundle.casefold() in names or entry['id'] in ids:
            raise ValueError(f'case-insensitive bundle name or identifier collision: {bundle}')
        names.add(bundle.casefold())
        ids.add(entry['id'])
        icon = manifest.parent / 'assets/AppIcon.icns'
        apps.append({**entry, 'bundle': bundle, 'manifest_sha256': digest(manifest),
                     'icon_sha256': digest(icon) if icon.is_file() else None})
    if not apps:
        raise ValueError('no eligible macOS SDK apps selected')
    source = subprocess.run(['git', '-C', str(root), 'rev-parse', 'HEAD'], capture_output=True, text=True, check=True).stdout.strip()
    return {'format': 1, 'source': source, 'architecture': arch, 'mode': mode,
            'apps': apps, 'excluded': excluded, 'notarized': False}


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def inventory(directory):
    entries = {}
    for path in sorted(directory.rglob('*')):
        rel = str(path.relative_to(directory))
        if rel == 'receipt.json':
            continue
        if path.is_symlink():
            entries[rel] = {'link': os.readlink(path)}
        elif path.is_file():
            entries[rel] = {'sha256': digest(path), 'mode': path.stat().st_mode & 0o777}
        elif path.is_dir():
            entries[rel] = {'directory': True}
        else:
            raise ValueError(f'unsupported filesystem object: {path}')
    return entries


def write_receipt(directory, plan, stage, facts=None):
    record = {'plan': plan, 'stage': stage, 'facts': facts or {}, 'files': inventory(directory)}
    (directory / 'receipt.json').write_text(json.dumps(record, indent=2, sort_keys=True) + '\n')


def check_receipt(directory, plan, stage):
    if directory.is_symlink() or (directory / 'receipt.json').is_symlink():
        raise ValueError('receipt directories must not be symlinks')
    record = json.loads((directory / 'receipt.json').read_text())
    if record.get('plan') != plan or record.get('stage') != stage or record.get('files') != inventory(directory):
        raise ValueError(f'{stage}: inputs or outputs changed; choose a fresh --out (resume refused)')
    return record


@contextmanager
def transaction(out, stage, resume, plan):
    out.mkdir(parents=True, exist_ok=True)
    if out.is_symlink() or (out / ".collection.lock").is_symlink():
        raise ValueError('output must not be a symlink')
    with (out / '.collection.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError('another collection stage owns this output') from None
        destination = out / stage
        if destination.exists():
            if not resume:
                raise ValueError(f'{destination} exists; use --resume for hash-checked reuse or a fresh --out')
            check_receipt(destination, plan, stage)
            yield None
            return
        with tempfile.TemporaryDirectory(prefix=f'.{stage}-', dir=out) as temp:
            work = Path(temp)
            yield work
            if not (work / 'receipt.json').is_file():
                raise ValueError('stage produced no receipt')
            work.rename(destination)


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('stage', nargs='?', default='plan', choices=['plan', 'audit-runtime', 'build', 'sign', 'package', 'verify', 'notarize'])
    p.add_argument('--apps', default='', help='comma-separated catalog names; default all eligible SDK apps')
    p.add_argument('--arch', choices=['arm64', 'x86_64', 'universal'], default={'aarch64': 'arm64'}.get(platform.machine(), platform.machine()))
    p.add_argument('--mode', choices=['development', 'distribution'], default='development')
    p.add_argument('--out', type=Path, default=ROOT / 'target/macos-collection')
    p.add_argument('--dry-run', action='store_true')
    p.add_argument('--resume', action='store_true')
    p.add_argument('--reuse-bundles', action='store_true', help='import existing SDK outputs; never compile')
    p.add_argument('--tilo-app', type=Path, help='existing tilo-app CLI executable; never installed automatically')
    p.add_argument('--compiler-guard', type=Path, help='existing shared rustc-budget.py wrapper')
    p.add_argument('--identity', help='exact Keychain SHA-1 or full identity name; development also accepts ad-hoc')
    p.add_argument('--offline', action='store_true', help='development signatures without secure timestamp; builds are always offline')
    p.add_argument('--per-app', action='store_true')
    p.add_argument('--notary-profile', help='existing notarytool Keychain profile, never raw credentials')
    p.add_argument('--submit-to-apple', action='store_true', help='explicitly upload DMGs to Apple notary service')
    p.add_argument('--runtime-binary', type=Path, help='audit-runtime: existing executable, inspected but never launched')
    p.add_argument('--runtime-prefix', type=Path, help='audit-runtime: existing GStreamer prefix, not downloaded')
    p.add_argument('--dependency-root', type=Path, action='append', default=[], help='audit-runtime: permitted dependency directory; repeatable')
    p.add_argument('--minimum-macos', default='13.0', help='audit-runtime: required minimum OS; never changed automatically')
    p.add_argument('--max-images', type=int, default=512, help='audit-runtime: maximum inspected Mach-O images (up to 2048)')
    p.add_argument('--audit-seconds', type=int, default=120, help='audit-runtime: traversal time budget (up to 600 seconds)')
    return p


def main():
    args = parser().parse_args()
    try:
        if args.stage == 'audit-runtime':
            if platform.system() != 'Darwin':
                raise ValueError('audit-runtime requires macOS inspection tools; it never builds, signs or launches code')
            if not args.runtime_binary or not args.runtime_prefix or not args.dependency_root:
                raise ValueError('audit-runtime needs --runtime-binary, --runtime-prefix and --dependency-root')
            from collection_runtime import AuditConfig, audit_runtime
            report = audit_runtime(AuditConfig(args.runtime_binary, args.runtime_prefix, tuple(args.dependency_root),
                                               args.arch, args.minimum_macos, args.max_images, args.audit_seconds))
            print(json.dumps(report, indent=2, sort_keys=True))
            # A static graph cannot approve an unqualified plugin runtime.
            return 2
        plan = make_plan(ROOT, args.apps, args.arch, args.mode)
        if args.offline and args.mode == 'distribution':
            raise ValueError('distribution requires online secure timestamps; use development for offline signing')
        if args.stage == 'plan' or args.dry_run:
            print(json.dumps(plan, indent=2, sort_keys=True))
            return 0
        if platform.system() != 'Darwin':
            raise ValueError('execution requires macOS; plan and fixture tests are portable')
        from collection_stages import execute
        execute(args, plan)
        return 0
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f'collection: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    # Keep a single module instance when collection_stages imports helpers.
    sys.modules['collection'] = sys.modules[__name__]
    raise SystemExit(main())

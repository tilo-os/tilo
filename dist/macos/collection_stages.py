"""Transactional collection orchestration; no install, mount or launch operation."""
from __future__ import annotations

import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import time

from collection import ROOT, check_receipt, digest, transaction, write_receipt
from collection_native import inspect_bundle, read_entitlements, run, select_identity, sign_bundle, verify_signatures


def disk_floor(path):
    if shutil.disk_usage(path).free < 20 * 1024**3:
        raise ValueError('20 GiB free disk floor reached; stopped owned stage')


def bounded_build(command, env, *, cwd=None, seconds=1800, stdout=None, stderr=None):
    directory = cwd or ROOT
    disk_floor(directory)
    child = subprocess.Popen(command, cwd=directory, env=env, start_new_session=True,
                             stdout=stdout, stderr=stderr)
    started = time.monotonic()
    try:
        while child.poll() is None:
            disk_floor(directory)
            if time.monotonic() - started > seconds:
                raise ValueError(f'build exceeded {seconds} second bound; reuse compiled cache on retry')
            time.sleep(1)
        if child.returncode:
            raise ValueError(f'build failed with exit {child.returncode}')
    finally:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()


def build_environment(args, work):
    guard = args.compiler_guard or Path(os.environ.get('RUSTC_WRAPPER', ''))
    if not guard.is_file() or guard.name != 'rustc-budget.py':
        raise ValueError('build needs --compiler-guard pointing to the existing shared rustc-budget.py')
    if os.environ.get('CARGO_TARGET_DIR') or os.environ.get('CARGO_BUILD_TARGET'):
        raise ValueError('unset CARGO_TARGET_DIR/CARGO_BUILD_TARGET; the SDK uses this worktree and host architecture')
    if args.arch != platform.machine():
        raise ValueError('SDK build supports host architecture only; use --reuse-bundles for independently assembled universal bundles')
    if not args.tilo_app or not args.tilo_app.is_file():
        raise ValueError('--tilo-app must name an existing SDK CLI; this script never installs a toolchain')
    cargo = shutil.which('cargo')
    if not cargo:
        raise ValueError('cargo is not available')
    # The SDK shell-outs through CARGO. Enforce --locked even on its metadata
    # and native build calls, without changing its bundler or installing tools.
    shim = work / 'cargo-locked'
    shim.write_text('#!/usr/bin/env python3\nimport os,sys\n'
                    f'cargo={cargo!r}\n'
                    'os.execv(cargo, [cargo, sys.argv[1], "--locked", "--offline", *sys.argv[2:]])\n')
    shim.chmod(0o700)
    env = dict(os.environ)
    env.update(CARGO=str(shim), CARGO_NET_OFFLINE='true', RUSTUP_AUTO_INSTALL='0', MACOSX_DEPLOYMENT_TARGET='13.0',
               RUSTC_WRAPPER=str(guard.resolve()), CARGO_BUILD_JOBS='1', CLOUD_RUSTC_JOBS='2')
    # Rust #157750: stripping proc-macros can misalign LINKEDIT, rejected by
    # macOS 27 dyld. Keep build-time tools unstripped; app release flags stay.
    env['CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_STRIP'] = 'none'
    # Share the guard's default lock namespace; never invent another budget.
    env.pop('CLOUD_RUSTC_LOCK_DIR', None)
    return env


def check_stage(out, plan, stage):
    record = check_receipt(out / stage, plan, stage)
    parent = {'signed': 'built', 'images': 'signed', 'notarized': 'images'}.get(stage)
    if parent:
        check_stage(out, plan, parent)
        if record['facts'].get(parent + '_receipt_sha256') != digest(out / parent / 'receipt.json'):
            raise ValueError(f'{stage}: parent receipt changed')
    return record


def build(args, plan, work):
    environment = None if args.reuse_bundles else build_environment(args, work)
    if environment and run(['git', 'status', '--porcelain', '--', 'apps', 'crates', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml'], cwd=ROOT).strip():
        raise ValueError('commit app/build inputs before compiling a reproducible collection')
    lock_digest = digest(ROOT / 'Cargo.lock')
    facts = []
    (work / 'apps').mkdir()
    (work / 'entitlements').mkdir()
    for entry in plan['apps']:
        if environment:
            bounded_build([str(args.tilo_app.resolve()), 'build', str(ROOT / 'apps' / entry['name']),
                           '--target', 'macos', '--release'], environment)
        source = ROOT / 'target/tilo-app/macos' / entry['package'] / entry['bundle']
        details = inspect_bundle(source, entry, args.arch)
        policy = source.with_suffix('.entitlements')
        read_entitlements(policy)
        app = work / 'apps' / entry['bundle']
        shutil.copytree(source, app, symlinks=True)
        shutil.copy2(policy, work / 'entitlements' / policy.name)
        # Validate copied bytes, never infer correctness from a successful copy.
        inspect_bundle(app, entry, args.arch)
        facts.append({'app': entry['name'], 'code': details})
    if digest(ROOT / 'Cargo.lock') != lock_digest:
        raise ValueError('Cargo.lock changed during build')
    shim = work / 'cargo-locked'
    if shim.exists():
        shim.unlink()
    return {'apps': facts, 'origin': 'existing SDK outputs' if args.reuse_bundles else 'SDK build',
            'host_dependency_strip': 'none' if environment else None,
            'notarized': False}


def sign(args, plan, work, identity):
    source = args.out / 'built'
    check_stage(args.out, plan, 'built')
    shutil.copytree(source / 'apps', work / 'apps', symlinks=True)
    shutil.copytree(source / 'entitlements', work / 'entitlements')
    for entry in plan['apps']:
        app = work / 'apps' / entry['bundle']
        inspect_bundle(app, entry, args.arch)
        sign_bundle(app, work / 'entitlements' / Path(entry['bundle']).with_suffix('.entitlements'), identity, args.offline)
        verify_signatures(app, identity, args.mode, args.arch)
    return {'identity': identity, 'offline': args.offline, 'notarized': False,
            'built_receipt_sha256': digest(source / 'receipt.json')}


def verify_apps(directory, plan, identity):
    for entry in plan['apps']:
        app = directory / 'apps' / entry['bundle']
        inspect_bundle(app, entry, plan['architecture'])
        verify_signatures(app, identity, plan['mode'], plan['architecture'])


def verify_image(path, identity):
    run(['/usr/bin/hdiutil', 'verify', path])
    command = ['/usr/bin/codesign', '--verify', '--strict']
    if identity['sha1'] != '-':
        command += ['-R', f'=anchor apple generic and certificate leaf = H"{identity["sha1"]}"']
    run(command + [path])


def package(args, plan, work):
    source = args.out / 'signed'
    receipt = check_stage(args.out, plan, 'signed')
    identity = receipt['facts']['identity']
    verify_apps(source, plan, identity)
    sets = [('tilo-apps', plan['apps'])]
    if args.per_app:
        sets += [('tilo-' + entry['name'], [entry]) for entry in plan['apps']]
    for name, entries in sets:
        stage = work / 'payload'
        stage.mkdir()
        for entry in entries:
            shutil.copytree(source / 'apps' / entry['bundle'], stage / entry['bundle'], symlinks=True)
        (stage / 'Applications').symlink_to('/Applications')
        (stage / 'collection.json').write_text(json.dumps({'plan': plan, 'included': [e['name'] for e in entries],
                                                          'notarized': False}, indent=2) + '\n')
        (stage / 'Read Me.txt').write_text('Copy the apps you want to Applications. No apps are installed automatically.\n'
                                           'This image is signed but has not been notarized. See the external receipt.\n')
        path = work / f'{name}-{args.arch}.dmg'
        run(['/usr/bin/hdiutil', 'create', '-srcfolder', stage, '-volname', 'Tilo Apps', '-format', 'UDZO', path], timeout=600)
        stamp = '--timestamp=none' if args.offline or identity['sha1'] == '-' else '--timestamp'
        run(['/usr/bin/codesign', '--sign', identity['sha1'], stamp, path], timeout=60)
        verify_image(path, identity)
        shutil.rmtree(stage)  # Only this invocation's payload, never input apps.
        (work / (path.name + '.sha256')).write_text(f'{digest(path)}  {path.name}\n')
    return {'identity': identity, 'per_app': args.per_app, 'notarized': False,
            'signed_receipt_sha256': digest(source / 'receipt.json')}


def submit_dmg(path, profile, journal):
    if journal.is_symlink():
        raise ValueError('notary journal must not be a symlink')
    records = json.loads(journal.read_text()) if journal.exists() else {}
    sha256 = digest(path)
    record = records.get(sha256)

    def save(record):
        records[sha256] = record
        temporary = journal.with_suffix('.pending')
        with temporary.open('x') as stream:
            stream.write(json.dumps(records, indent=2) + '\n')
        temporary.replace(journal)

    base = ['/usr/bin/xcrun', 'notarytool']
    authentication = ['--keychain-profile', profile, '--output-format', 'json']
    if record is None:
        # An uncertain submit must never be automatically repeated on retry.
        save({'file': path.name, 'id': None, 'status': 'upload outcome uncertain'})
        record = json.loads(run(base + ['submit', path] + authentication, timeout=600, combine=False))
        record = {'file': path.name, 'id': record.get('id'), 'status': record.get('status')}
        save(record)
    if not record.get('id'):
        raise ValueError('previous Apple upload outcome uncertain; inspect notarytool history and reconcile notary-submissions.json before retry')
    if record.get('status') != 'Accepted':
        response = json.loads(run(base + ['wait', record['id'], '--timeout', '20m'] + authentication,
                                  timeout=1260, combine=False))
        record['status'] = response.get('status')
        save(record)
    if record['status'] != 'Accepted':
        raise ValueError(f'Apple notarization not accepted: {record}; inspect notarytool log')
    return record


def notarize(args, plan, work):
    source = args.out / 'images'
    receipt = check_stage(args.out, plan, 'images')
    identity = receipt['facts']['identity']
    verify_apps(args.out / 'signed', plan, identity)
    submissions = []
    for original in sorted(source.glob('*.dmg')):
        verify_image(original, identity)
        path = work / original.name
        shutil.copy2(original, path)
        submission = submit_dmg(path, args.notary_profile, args.out / 'notary-submissions.json')
        (work / (path.name + '.submission.json')).write_text(json.dumps(submission) + '\n')
        log = json.loads(run(['/usr/bin/xcrun', 'notarytool', 'log', submission['id'],
                              '--keychain-profile', args.notary_profile], combine=False))
        (work / (path.name + '.notary-log.json')).write_text(json.dumps(log, indent=2) + '\n')
        if log.get('issues'):
            raise ValueError(f"Review Apple notary log for {submission['id']}; issues prevent completion")
        run(['/usr/bin/xcrun', 'stapler', 'staple', path])
        run(['/usr/bin/xcrun', 'stapler', 'validate', path])
        verify_image(path, identity)
        run(['/usr/sbin/spctl', '--assess', '--type', 'open', '--context', 'context:primary-signature', path])
        (work / (path.name + '.sha256')).write_text(f'{digest(path)}  {path.name}\n')
        submissions.append(submission)
    if not submissions:
        raise ValueError('no DMGs to notarize')
    return {'notarized': True, 'submissions': submissions, 'identity': identity,
            'images_receipt_sha256': digest(source / 'receipt.json')}


def execute(args, plan):
    if args.stage == 'notarize' and (not args.submit_to_apple or args.mode != 'distribution' or not args.notary_profile):
        raise ValueError('notarize requires --mode distribution --submit-to-apple --notary-profile NAME; uploads DMGs to Apple')
    args.out = args.out.absolute()
    disk_floor(ROOT)
    if args.stage == 'verify':
        signed = check_stage(args.out, plan, 'signed')
        verify_apps(args.out / 'signed', plan, signed['facts']['identity'])
        images = check_stage(args.out, plan, 'images')
        if images['facts']['signed_receipt_sha256'] != digest(args.out / 'signed/receipt.json'):
            raise ValueError('image inputs changed')
        for path in sorted((args.out / 'images').glob('*.dmg')):
            verify_image(path, signed['facts']['identity'])
        if (args.out / 'notarized').exists():
            record = check_stage(args.out, plan, 'notarized')
            if record['facts']['images_receipt_sha256'] != digest(args.out / 'images/receipt.json'):
                raise ValueError('notarization inputs changed')
            for path in sorted((args.out / 'notarized').glob('*.dmg')):
                verify_image(path, signed['facts']['identity'])
                run(['/usr/bin/xcrun', 'stapler', 'validate', path])
        print('Verified hashes, architectures, bundle closure and signatures; no apps mounted or launched.')
        return
    identity = None
    if args.stage == 'sign':
        metadata = '' if args.identity == 'ad-hoc' else run(['/usr/bin/security', 'find-identity', '-v', '-p', 'codesigning'])
        identity = select_identity(metadata, args.identity, args.mode)
    stage = {'build': 'built', 'sign': 'signed', 'package': 'images', 'notarize': 'notarized'}[args.stage]
    with transaction(args.out, stage, args.resume, plan) as work:
        if work is None:
            record = check_stage(args.out, plan, stage)
            if stage == 'signed' and (record['facts']['identity'] != identity or record['facts']['offline'] != args.offline):
                raise ValueError('resume signing options changed')
            if stage == 'images' and record['facts']['per_app'] != args.per_app:
                raise ValueError('resume packaging options changed')
            print(f'Reused verified {args.out / stage}')
            return
        if stage == 'built':
            facts = build(args, plan, work)
        elif stage == 'signed':
            facts = sign(args, plan, work, identity)
        elif stage == 'images':
            facts = package(args, plan, work)
        else:
            facts = notarize(args, plan, work)
        write_receipt(work, plan, stage, facts)
    print(f'Created {args.out / stage}; notarized={facts["notarized"]}')

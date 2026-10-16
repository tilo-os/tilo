#!/usr/bin/env python3
"""Assemble receipt-backed private macOS 27 apps, stopping before signing."""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys

import collection
import collection_app_runtime as app_runtime
import collection_font_catalog as fonts
import collection_local_runtime as runtime
from collection_native import inspect_bundle_metadata, load_paths, read_entitlements, run
from collection_stages import bounded_build


def source_check(root, revision):
    if not re.fullmatch(r'[0-9a-f]{40}', revision):
        raise ValueError('an exact source commit is required')
    if run(['git', '-C', root, 'rev-parse', 'HEAD']).strip() != revision:
        raise ValueError('app source revision changed')
    if run(['git', '-C', root, 'status', '--porcelain', '--untracked-files=no']).strip():
        raise ValueError('app source has tracked changes')


def source_catalog(root):
    path = root / 'tools/apps/app_release.py'
    spec = importlib.util.spec_from_file_location('local_qualified_catalog', path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module.app_catalog


def make_plan(args):
    root = args.source_root.resolve(strict=True)
    source_check(root, args.revision)
    collection.app_catalog = source_catalog(root)
    catalog = collection.make_plan(root, args.app, 'arm64', 'distribution')
    if len(catalog['apps']) != 1:
        raise ValueError('assemble one explicitly named eligible app at a time')
    entry = catalog['apps'][0]
    runtime_plan = runtime.make_plan(args.profile, args.profile_sha256)
    if runtime_plan['source'] != args.revision:
        raise ValueError('runtime and app source revisions differ')
    payload = args.payload.resolve(strict=True)
    record = collection.check_receipt(payload, runtime_plan, 'local-runtime')
    if runtime.inspect_payload(payload, runtime_plan) != record['facts']['images']:
        raise ValueError('runtime payload metadata changed')
    binary = runtime.regular_file(args.binary, runtime.LIMIT)
    executable(binary)
    if not re.fullmatch(r'[0-9a-f]{64}', args.binary_sha256) or collection.digest(binary) != args.binary_sha256:
        raise ValueError('compiled app SHA-256 mismatch')
    facts = runtime.image_info(binary, 'arm64', 15)
    if facts['slices']['arm64']['filetype'] != 'EXECUTE':
        raise ValueError('app input is not an executable Mach-O')
    if runtime.version(facts['slices']['arm64']['minimum_macos']) > runtime.version('13.0'):
        raise ValueError('app binary exceeds its original SDK macOS 13 policy')
    if not args.app_profile:
        load_paths(binary, binary, binary.parent, 'arm64')
    sdk = runtime.regular_file(args.sdk, runtime.LIMIT)
    plan = {'format': 1, 'stage': 'private-local-app', 'source': args.revision,
            'source_root': str(root), 'app': entry, 'binary': str(binary),
            'binary_sha256': args.binary_sha256, 'binary_facts': facts,
            'sdk': str(sdk), 'sdk_sha256': collection.digest(sdk),
            'runtime_plan': runtime_plan, 'payload': str(payload),
            'payload_receipt_sha256': collection.digest(payload / 'receipt.json'),
            'font_catalog': fonts.make_plan(root),
            'minimum_macos': '27.0', 'general_product_minimum_unchanged': '13.0',
            'signed': False, 'notarized': False, 'runtime_qualified': False}
    if args.app_profile:
        if not args.app_profile_sha256:
            raise ValueError('app profile requires its exact SHA-256')
        plan['base_runtime_plan'] = runtime_plan
        plan['runtime_plan'], plan['app_runtime'] = app_runtime.prepare(
            args.app_profile, args.app_profile_sha256, entry,
            {'path': str(binary), 'facts': facts}, runtime_plan)
    elif args.app_profile_sha256:
        raise ValueError('app profile SHA-256 requires an app profile')
    return plan


def executable(path):
    if not path.stat().st_mode & 0o100 or not os.access(path, os.X_OK):
        raise ValueError(f'missing executable permission: {path.name}')


def pinned_payload(plan):
    payload = Path(plan['payload'])
    record = runtime.checked_json(payload / 'receipt.json', plan['payload_receipt_sha256'])
    if (record['plan'] != plan.get('base_runtime_plan', plan['runtime_plan']) or record['stage'] != 'local-runtime' or
            record['files'] != collection.inventory(payload)):
        raise ValueError('pinned payload changed')
    return record


def inspect_app(app, plan):
    main, helper, info, binaries = inspect_bundle_metadata(app, plan['app'])
    if main == helper or info['LSMinimumSystemVersion'] != '27.0':
        raise ValueError('private bundle must have distinct app/helper and explicit macOS 27 minimum')
    extended = plan.get('app_runtime')
    if not extended and collection.digest(main) != plan['binary_sha256']:
        raise ValueError('app executable changed')
    executable(main)
    if extended:
        app_runtime.inspect_launcher(main)
    else:
        load_paths(main, main, app, 'arm64')
    # The payload validator permits only this one independently checked app executable.
    facts = runtime.inspect_payload(app, plan['runtime_plan'], (str(main.relative_to(app)),))
    payload_receipt = pinned_payload(plan)
    originals = payload_receipt['facts']['images']
    if extended:
        unchanged = set(originals) - {runtime.SHIM, runtime.LAUNCHER}
        if any(facts.get(path) != originals[path] for path in unchanged):
            raise ValueError('base runtime library bytes changed')
    elif facts != originals:
        raise ValueError('integrated runtime bytes differ from reviewed payload')
    for relative, item in facts.items():
        if item['slices']['arm64']['filetype'] == 'EXECUTE':
            executable(app / relative)
    code = set(binaries)
    allowed_files = {app / relative for relative in plan['runtime_plan']['assets']}
    allowed_files |= code | {app / 'Contents/Info.plist', app / 'Contents/Resources/tilo-app.toml'}
    if plan.get('font_catalog'):
        fonts.inspect(app, plan['font_catalog'])
        allowed_files |= {app / relative for relative in plan['font_catalog']['files']}
    if plan['app']['icon_sha256']:
        allowed_files.add(app / 'Contents/Resources/AppIcon.icns')
    for path in app.rglob('*'):
        if plan.get('font_catalog') and path == app / fonts.LOOKUP_LINK:
            # The exact relative in-bundle target was checked above. Other
            # symlinks remain forbidden by this private bundle adapter.
            continue
        if path.is_symlink() or (path.is_file() and path not in allowed_files):
            raise ValueError(f'unexpected bundle content: {path.relative_to(app)}')
        if path.is_file() and os.access(path, os.X_OK) and path not in code:
            raise ValueError('executable non-Mach-O resource')
    for relative, asset in plan['runtime_plan']['assets'].items():
        if collection.digest(app / relative) != asset['sha256']:
            raise ValueError(f'integrated resource changed: {relative}')
    macos_files = {main, helper, app / 'Contents/MacOS/tilo-runtime-mcp.real'}
    if extended:
        macos_files.add(app / extended['main_real'])
    if plan.get('font_catalog'):
        macos_files.add(app / fonts.LOOKUP_LINK)
    if set((app / 'Contents/MacOS').iterdir()) != macos_files:
        raise ValueError('unexpected helper directory contents')
    result = {'runtime': facts, 'app': runtime.image_info(main, 'arm64', 15),
            'code_objects': len(code), 'source': plan['source'], 'runtime_qualified': False}
    if extended:
        result['real_app'] = extended['main_real']
    return result


def sdk_environment(work):
    env = dict(os.environ)
    cargo = run(['/usr/bin/which', 'cargo']).strip()
    shim = work / 'cargo-metadata-locked'
    shim.write_text('#!/usr/bin/env python3\nimport os,sys\n'
                    f'cargo={cargo!r}\n'
                    'if sys.argv[1:2] != ["metadata"]: raise SystemExit("assembly may only inspect Cargo metadata")\n'
                    'os.execv(cargo, [cargo,"metadata","--locked","--offline",*sys.argv[2:]])\n')
    shim.chmod(0o700)
    env.update(CARGO=str(shim), CARGO_NET_OFFLINE='true', RUSTUP_AUTO_INSTALL='0')
    return env, shim


def overlay(app, plan):
    payload = Path(plan['payload'])
    record = pinned_payload(plan)
    runtime_plan = plan['runtime_plan']
    helper = app / 'Contents/MacOS/tilo-runtime-mcp'
    originals = [item for item in runtime_plan['images'].values()
                 if item['destination'] == 'Contents/MacOS/tilo-runtime-mcp.real']
    if len(originals) != 1 or collection.digest(helper) != originals[0]['sha256']:
        raise ValueError('SDK helper does not match the qualified original')
    helper.unlink()  # Only the new SDK copy in this transaction.
    for relative, item in sorted(record['files'].items()):
        relative = Path(relative)
        path = payload / relative
        target = app / relative
        if path.is_symlink():
            raise ValueError('runtime payload symlink')
        if item.get('directory'):
            target.mkdir(parents=True, exist_ok=True)
        else:
            runtime.copy_checked(path, target, item['sha256'])
            target.chmod(item['mode'])
    plist = app / 'Contents/Info.plist'
    info = plistlib.loads(plist.read_bytes())
    if info.get('LSMinimumSystemVersion') != '13.0':
        raise ValueError('SDK minimum policy changed; review before local overlay')
    info['LSMinimumSystemVersion'] = '27.0'
    plist.write_bytes(plistlib.dumps(info))


def signing_inventory(app, policy, facts):
    paths = list(facts['runtime'])
    # Flat libraries/plugins first, then helper processes, finally the enclosing app.
    paths.sort(key=lambda p: (facts['runtime'][p]['slices']['arm64']['filetype'] == 'EXECUTE', p))
    return {'identity_class': 'Developer ID Application', 'secure_timestamp': True,
            'hardened_runtime': True, 'nested_code_in_order': paths,
            'final_bundle': app.name, 'app_entitlements': policy.name,
            'nested_process_entitlements': ({facts['real_app']: policy.name} if facts.get('real_app') else {}),
            'signature_verification': 'every code object, then outer bundle',
            'keychain_access': 'normal user authorization; never unlock/import/change ACLs',
            'runtime_validation_after_signing': ['libintl forwarding and three catalog domains',
                                               'non-English catalog lookup', 'scanner subprocess and codec discovery',
                                               'relocated paths with spaces; no Homebrew code/catalog reads',
                                               'ORC/PCRE2 hardened runtime behavior'],
            'notarization': 'not authorized', 'installation': 'not performed'}


def assemble(out, plan, resume=False, guard=None):
    root = Path(plan['source_root'])
    source_check(root, plan['source'])
    if plan.get('app_runtime') and (not guard or not guard.is_file() or guard.name != 'rustc-budget.py'):
        raise ValueError('main runtime assembly requires the existing shared compiler guard')
    runtime.floor(out.parent)
    # Admission covers both SDK copies and the subsequent runtime overlay;
    # the continuous supervisor/copy checks also enforce the live floor.
    required = plan.get('binary_facts', {}).get('bytes', 0)
    required += sum(item.get('bytes', 0) for item in plan['runtime_plan']['images'].values())
    required += sum(item['bytes'] for item in plan['runtime_plan'].get('assets', {}).values())
    required += sum(item['bytes'] for item in plan.get('font_catalog', {}).get('files', {}).values())
    required += 64 * 1024**2
    if shutil.disk_usage(out.parent).free < 20 * 1024**3 + required:
        raise ValueError('insufficient headroom above the 20 GiB floor for SDK copies and runtime overlay')
    label = 'app-' + plan['app']['name']
    with collection.transaction(out, label, resume, plan) as work:
        if work is None:
            return
        env, shim = sdk_environment(work)
        original_helper = next(source for source, item in plan['runtime_plan']['images'].items()
                               if item['destination'] == 'Contents/MacOS/tilo-runtime-mcp.real')
        if collection.digest(Path(plan['sdk'])) != plan['sdk_sha256']:
            raise ValueError('SDK tool changed')
        output = work / 'sdk-output'
        command = [plan['sdk'], '--json', 'bundle-macos', root / 'apps' / plan['app']['name'],
                   '--binary', plan['binary'], '--runtime-host', original_helper, '--output', output]
        stdout_path, stderr_path = work / 'sdk-result.json', work / 'sdk-diagnostics.txt'
        with stdout_path.open('wb') as stdout, stderr_path.open('wb') as stderr:
            try:
                bounded_build(command, env, cwd=work, seconds=120, stdout=stdout, stderr=stderr)
            except ValueError as error:
                diagnostic = stderr_path.read_text(errors='replace')[-8192:]
                raise ValueError(f'{error}: {diagnostic}') from error
        runtime.regular_file(stdout_path, 4 * 1024**2)
        result = json.loads(stdout_path.read_text())
        app = output / plan['app']['bundle']
        if not result.get('ok') or Path(result['result']['artifact']) != app:
            raise ValueError('SDK returned an unexpected bundle')
        # Metadata is generated by the SDK, not duplicated here.
        inspect_bundle_metadata(app, plan['app'])
        policy = app.with_suffix('.entitlements')
        read_entitlements(policy)
        original_info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
        overlay(app, plan)
        if plan.get('font_catalog'):
            fonts.stage(app, plan['font_catalog'])
        if plan.get('app_runtime'):
            app_runtime.augment(app, plan, guard)
        facts = inspect_app(app, plan)
        final_info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
        if final_info != {**original_info, 'LSMinimumSystemVersion': '27.0'}:
            raise ValueError('local integration changed SDK metadata beyond the private minimum')
        app.rename(work / app.name)
        policy.rename(work / policy.name)
        output.rmdir()
        shim.unlink()
        stdout_path.unlink()
        stderr_path.unlink()
        source_check(root, plan['source'])
        (work / 'signing-inventory.json').write_text(json.dumps(signing_inventory(app, policy, facts), indent=2) + '\n')
        facts['sdk_original_info'] = original_info
        facts['entitlements_sha256'] = collection.digest(work / policy.name)
        collection.write_receipt(work, plan, label, facts)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('stage', choices=['plan', 'assemble', 'verify'])
    parser.add_argument('--source-root', type=Path, required=True)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--app', required=True)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--binary-sha256', required=True)
    parser.add_argument('--sdk', type=Path, required=True)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--profile-sha256', required=True)
    parser.add_argument('--app-profile', type=Path)
    parser.add_argument('--app-profile-sha256')
    parser.add_argument('--compiler-guard', type=Path)
    parser.add_argument('--payload', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--resume', action='store_true')
    args = parser.parse_args(argv)
    try:
        plan = make_plan(args)
        if args.stage == 'plan':
            print(json.dumps(plan, indent=2))
            return 0
        if args.stage == 'assemble':
            assemble(args.out.absolute(), plan, args.resume, args.compiler_guard)
        stage = 'app-' + plan['app']['name']
        directory = args.out / stage
        record = collection.check_receipt(directory, plan, stage)
        facts = inspect_app(directory / plan['app']['bundle'], plan)
        for key in ('runtime', 'app', 'code_objects', 'source'):
            if facts[key] != record['facts'][key]:
                raise ValueError('integrated app metadata changed')
        print(json.dumps({'app': plan['app']['name'], 'source': plan['source'], 'code_objects': facts['code_objects'],
                          'minimum_macos': '27.0', 'status': 'assembled; awaiting signing and runtime qualification'}))
        return 0
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f'local app: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(main())

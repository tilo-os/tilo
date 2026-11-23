#!/usr/bin/env python3
"""Relocate a reviewed GStreamer closure beside the binary that loads it.

`tilolang` records the closure tilolang-app and GStreamer's scanner load from
this Mac's Homebrew, refuses one that differs from the reviewed lists, and
stages it as the tilolang runtime's libraries (`lib/gstreamer/`).
`plan`/`stage`/`verify` stage the pinned private macOS 27 helper payload.
Neither signs or executes what it stages.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

from collection import check_receipt, digest, transaction, write_receipt
from collection_native import machos, run, system_library
from collection_runtime import (bounded_hash, directory_entries, expand, formula_evidence, image_info,
                                regular_file, remaining, resolve_load, version)
from collection_stages import bounded_build

HERE = Path(__file__).resolve().parent
PLUGINS = {'libgstvpx.dylib', 'libgstaom.dylib', 'libgstrav1e.dylib',
           'libgstdav1d.dylib', 'libgstlibav.dylib'}
SHIM = 'Contents/Frameworks/libtilo-local-intl.dylib'
LAUNCHER = 'Contents/MacOS/tilo-runtime'
LIMIT = 1024**3
HOMEBREW = Path('/opt/homebrew')
# The tilolang runtime's libraries, under its bundle: the binary is `bin/tilolang-app`.
GSTREAMER = 'lib/gstreamer'
TILOLANG_SHIM = GSTREAMER + '/Frameworks/libtilo-local-intl.dylib'


def checked_json(path, expected):
    regular_file(path, 4 * 1024**2)
    with path.open('rb') as stream:
        data = stream.read(4 * 1024**2 + 1)
    if len(data) > 4 * 1024**2 or not re.fullmatch(r'[0-9a-f]{64}', expected) or hashlib.sha256(data).hexdigest() != expected:
        raise ValueError('profile SHA-256 mismatch')
    return json.loads(data)


def destination(path, roots):
    if path == roots[0]:
        return 'Contents/MacOS/tilo-runtime.real'
    if path == roots[1]:
        return 'Contents/Helpers/gst-plugin-scanner'
    folder = 'PlugIns' if path in roots[2:] else 'Frameworks'
    return f'Contents/{folder}/{path.name}'


def local_load(loader, target):
    return '@loader_path/' + os.path.relpath(target, Path(loader).parent)


def inspect_inputs(profile, deadline):
    roots = [Path(p).resolve(strict=True) for p in profile['roots']]
    images = profile['images']
    if (len(images) != 41 or len(roots) != 7 or roots[0].name != 'tilo-runtime' or
            roots[1].name != 'gst-plugin-scanner' or
            {p.name for p in roots[2:]} != PLUGINS or set(profile['plugins']) != PLUGINS or
            profile['candidate_local_artifact_minimum_macos'] != '27.0' or profile['issues']):
        raise ValueError('requires the reviewed 41-image, five-plugin private macOS 27 profile')
    if not re.fullmatch(r'[0-9a-f]{40}', profile['source']):
        raise ValueError('missing source commit')
    result, names = {}, set()
    for source, recorded in images.items():
        path = regular_file(Path(source), LIMIT)
        if str(path) != source:
            raise ValueError('profile image paths must be canonical')
        facts = image_info(path, 'arm64', remaining(deadline))
        if any(facts[key] != recorded[key] for key in ('sha256', 'bytes', 'slices')):
            raise ValueError(f'profile image changed: {path.name}')
        arch = facts['slices']['arm64']
        expected_type = {'EXECUTE'} if path in roots[:2] else {'DYLIB', 'BUNDLE'} if path in roots[2:] else {'DYLIB'}
        if arch['filetype'] not in expected_type or version(arch['minimum_macos']) > version('27.0'):
            raise ValueError(f'unsupported image type or deployment floor: {path.name}')
        target = destination(path, roots)
        if target.casefold() in names or target in {SHIM, LAUNCHER}:
            raise ValueError('case-insensitive runtime filename collision')
        names.add(target.casefold())
        result[source] = {'destination': target, **facts}
    if sum(item['bytes'] for item in result.values()) > 512 * 1024**2:
        raise ValueError('runtime image byte bound exceeded')
    if any(str(root) not in result for root in roots):
        raise ValueError('profile root missing from images')
    return result


def relocation_map(images, shim=SHIM):
    """Resolve each edge in helper and scanner contexts, refusing ambiguity."""
    intl = [key for key in images if Path(key).name == 'libintl.8.dylib']
    if len(intl) != 1:
        raise ValueError('requires exactly one real libintl')
    executables = [Path(key) for key, item in images.items()
                   if item['slices']['arm64']['filetype'] == 'EXECUTE']
    allowed = tuple({Path(key).parent for key in images})
    for source, item in images.items():
        changes = []
        for edge in item['slices']['arm64']['dependencies']:
            old = edge['load']
            if system_library(old):
                continue
            if old.startswith('/'):
                resolved = str(Path(old).resolve(strict=True))
            else:
                if not executables:
                    raise ValueError('contextual source load needs explicit executable contexts')
                targets = set()
                for executable in executables:
                    bases = [expand(p, Path(source), executable) for p in item['slices']['arm64']['rpaths']]
                    bases += [expand(p, executable, executable) for p in images[str(executable)]['slices']['arm64']['rpaths']]
                    targets.add(str(resolve_load(old, Path(source), executable, bases, allowed)))
                if len(targets) != 1:
                    raise ValueError('helper/scanner dependency resolution differs')
                resolved = targets.pop()
            if resolved not in images:
                raise ValueError(f'dependency missing from pinned closure: {old}')
            target = shim if resolved == intl[0] else images[resolved]['destination']
            changes.append([old, local_load(item['destination'], target)])
        item['changes'] = changes
    return intl[0]


def add_asset(assets, source, target, roots, deadline=None):
    deadline = deadline if deadline is not None else time.monotonic() + 10
    path = regular_file(source, 16 * 1024**2, roots)
    if target.casefold() in {key.casefold() for key in assets}:
        raise ValueError(f'resource collision: {target}')
    size = path.stat().st_size
    if len(assets) >= 4096 or size + sum(a['bytes'] for a in assets.values()) > 64 * 1024**2:
        raise ValueError('resource count or byte bound exceeded')
    assets[target] = {'source': str(path), 'sha256': bounded_hash(path, 16 * 1024**2, deadline), 'bytes': size}


def resources(profile, deadline, packages=18, into='Contents'):
    assets, prefixes = {}, {}
    if packages is not None and len(profile['packages']) != packages:
        raise ValueError(f'requires the reviewed {packages}-package profile')
    for name, package in profile['packages'].items():
        root = Path(name).resolve(strict=True)
        if str(root) != name or root.name != package['version']:
            raise ValueError('noncanonical package root')
        key = root.parent.name
        if not re.fullmatch(r'[a-zA-Z0-9@+_.-]+', key):
            raise ValueError('unsafe package name')
        for path in directory_entries(root, 256, deadline):
            if path.name.lower().startswith(('license', 'licence', 'copying', 'copyright', 'authors', 'notice')) and path.is_file():
                add_asset(assets, path, f'{into}/Resources/Notices/{key}/{path.name}', (root,), deadline)
        # Component-level evidence where formula-wide COPYING describes tools.
        extras = {'glib': 'include/glib-2.0/glib/gtypes.h', 'gettext': 'include/libintl.h'}
        if key in extras:
            path = root / extras[key]
            add_asset(assets, path, f'{into}/Resources/Notices/{key}/{path.name}', (root,), deadline)
        if key not in {'glib', 'gstreamer'}:
            continue
        prefixes[key] = str(root / 'share/locale')
        wanted = {'glib20.mo'} if key == 'glib' else {'gstreamer-1.0.mo', 'gst-plugins-base-1.0.mo'}
        found = set()
        locale = root / 'share/locale'
        if locale.is_symlink() or not locale.resolve(strict=True).is_relative_to(root):
            raise ValueError('symlink or escaping locale root')
        pending, visited = [locale], 0
        while pending:
            directory = pending.pop()
            visited += 1
            if visited + len(pending) > 4096:
                raise ValueError('resource traversal bound exceeded')
            for path in directory_entries(directory, 512, deadline):
                if path.is_symlink():
                    raise ValueError('symlink locale entry')
                if path.is_dir():
                    pending.append(path)
                    continue
                if path.name not in wanted:
                    continue
                rel = path.relative_to(locale)
                if len(rel.parts) != 3 or rel.parts[1] != 'LC_MESSAGES':
                    raise ValueError('unexpected locale layout')
                add_asset(assets, path, f'{into}/Resources/locale/{rel}', (root,), deadline)
                found.add(path.name)
        if found != wanted:
            raise ValueError('missing required locale domain catalogs')
    if set(prefixes) != {'glib', 'gstreamer'} or sum(a['bytes'] for a in assets.values()) > 64 * 1024**2:
        raise ValueError('missing locale packages or resource byte bound exceeded')
    return assets, prefixes


def make_plan(profile_path, expected):
    profile = checked_json(profile_path, expected)
    deadline = time.monotonic() + 180
    images = inspect_inputs(profile, deadline)
    intl = relocation_map(images)
    assets, prefixes = resources(profile, deadline)
    remaining(deadline)
    return {'format': 1, 'scope': 'private-local-macos27-helper', 'source': profile['source'],
            'profile_sha256': expected, 'architecture': 'arm64', 'minimum_macos': '27.0',
            'general_product_minimum_unchanged': '13.0', 'images': images, 'assets': assets,
            'locale_prefixes': prefixes, 'intl': intl,
            'adapter_sources': {p: digest(HERE / p) for p in
                                ('local_runtime_intl.c', 'local_runtime_launcher.c')},
            'signed': False, 'notarized': False, 'runtime_qualified': False}


def floor(path):
    if shutil.disk_usage(path).free < 20 * 1024**3:
        raise ValueError('20 GiB free-space floor reached')


def copy_checked(source, target, expected):
    regular_file(source, LIMIT)
    target.parent.mkdir(parents=True, exist_ok=True)
    deadline, total = time.monotonic() + 60, 0
    with source.open('rb') as origin, target.open('xb') as output:
        while chunk := origin.read(1024**2):
            total += len(chunk)
            remaining(deadline)
            floor(target.parent)
            if total > LIMIT:
                raise ValueError('input grew beyond copy bound')
            output.write(chunk)
    if digest(target) != expected:
        raise ValueError(f'input changed during copy: {source.name}')


def compiler_environment():
    env = dict(os.environ)
    env.update(CLOUD_RUSTC_JOBS='2', CLOUD_RUSTC_INNER_WRAPPER='', CLOUD_RUSTC_SCCACHE='0',
               MACOSX_DEPLOYMENT_TARGET='27.0')
    env.pop('CLOUD_RUSTC_LOCK_DIR', None)
    return env


def compile_adapters(work, plan, guard, shim=SHIM, launcher=LAUNCHER):
    config = {'TILO_GST_LOCALE': plan['locale_prefixes']['gstreamer'],
              'TILO_GLIB_LOCALE': plan['locale_prefixes']['glib'],
              'TILO_INTL_NAME': Path(plan['intl']).name,
              'TILO_PROFILE_KEY': plan['profile_sha256']}
    if plan.get('extended_locale_catalogs'):
        config['TILO_GST_EXTENDED_CATALOGS'] = '1'
    if plan.get('media_data'):
        config['TILO_MEDIA_DATA'] = '1'
    scratch = work / 'adapter-build'
    scratch.mkdir()
    header = scratch / 'local_runtime_config.h'
    header.write_text(''.join(f'#define {key} {json.dumps(value)}\n' for key, value in config.items()))
    base = [*([guard] if guard else []), '/usr/bin/clang', '-arch', 'arm64',
            '-mmacosx-version-min=27.0', '-Wall', '-Wextra', '-Werror', '-O2', '-I', scratch,
            '-Wl,-no_adhoc_codesign']
    sources = [('local_runtime_intl.c', shim)]
    if launcher:
        sources.append(('local_runtime_launcher.c', launcher))
    if plan.get('app_launcher'):
        sources.append(('local_runtime_launcher.c', plan['app_launcher']))
    for source, target in sources:
        if digest(HERE / source) != plan['adapter_sources'][source]:
            raise ValueError('adapter source changed')
        command = base + [HERE / source, '-o', work / target]
        if target == plan.get('app_launcher'):
            name = Path(target).name
            command += ['-DTILO_LAUNCH_NAME=' + json.dumps(name),
                        '-DTILO_EXEC_NAME=' + json.dumps(name + '.real')]
        if target == shim:
            (work / target).parent.mkdir(parents=True, exist_ok=True)
            command += ['-dynamiclib', '-Wl,-install_name,@loader_path/libtilo-local-intl.dylib',
                        '-Xlinker', '-reexport_library', '-Xlinker', plan['intl']]
        bounded_build(command, compiler_environment(), cwd=work, seconds=120)
    shutil.rmtree(scratch)
    # The re-export deliberately bypasses the shim, preventing recursion.
    original_id = plan['images'][plan['intl']]['slices']['arm64']['install_id']
    run(['/usr/bin/install_name_tool', '-change', original_id,
         '@loader_path/' + Path(plan['intl']).name, work / shim])


def inspect_payload(work, plan, extra_code=(), shim=SHIM, launcher=LAUNCHER):
    adapters = {shim, launcher} - {None}
    expected = {item['destination'] for item in plan['images'].values()} | adapters
    if {str(p.relative_to(work)) for p in machos(work)} != expected | set(extra_code):
        raise ValueError('unexpected or missing payload code')
    records = {}
    for relative in sorted(expected):
        facts = image_info(work / relative, 'arm64', 15)
        arch = facts['slices']['arm64']
        if arch['rpaths'] or version(arch['minimum_macos']) > version('27.0'):
            raise ValueError('unexpected staged rpath or deployment floor')
        for edge in arch['dependencies']:
            value = edge['load']
            if system_library(value):
                continue
            if not value.startswith('@loader_path/'):
                raise ValueError(f'nonlocal staged dependency: {value}')
            target = ((work / relative).parent / value[len('@loader_path/'):]).resolve()
            if not target.is_relative_to(work.resolve()) or str(target.relative_to(work.resolve())) not in expected:
                raise ValueError(f'escaping or missing staged dependency: {value}')
        records[relative] = facts
    for item in plan['images'].values():
        before = item['slices']['arm64']
        after = records[item['destination']]['slices']['arm64']
        changes = dict(item['changes'])
        expected_edges = [{**edge, 'load': changes.get(edge['load'], edge['load'])} for edge in before['dependencies']]
        install_id = '@loader_path/' + Path(item['destination']).name if before['install_id'] else None
        if (after['dependencies'] != expected_edges or after['install_id'] != install_id or
                after['filetype'] != before['filetype'] or
                after['minimum_macos'] != before['minimum_macos']):
            raise ValueError('relocation changed more than declared load commands')
    if records[shim]['slices']['arm64']['dependencies'].count(
            {'kind': 'LC_REEXPORT_DYLIB', 'load': '@loader_path/' + Path(plan['intl']).name}) != 1:
        raise ValueError('missing explicit real-libintl re-export')
    for relative, allowed in [(shim, {'@loader_path/' + Path(plan['intl']).name}), (launcher, set())]:
        if relative is None:
            continue
        arch = records[relative]['slices']['arm64']
        if {e['load'] for e in arch['dependencies'] if not system_library(e['load'])} != allowed:
            raise ValueError('unexpected adapter dependencies')
        if arch['filetype'] != ('DYLIB' if relative == shim else 'EXECUTE'):
            raise ValueError('unexpected adapter image type')
    return records


def stage(out, plan, guard, resume=False):
    floor(out.parent)
    with transaction(out, 'local-runtime', resume, plan) as work:
        if work is None:
            return
        for source, item in plan['images'].items():
            floor(work)
            target = work / item['destination']
            copy_checked(Path(source), target, item['sha256'])
            target.chmod(0o755)
            command = ['/usr/bin/install_name_tool']
            for old, new in item['changes']:
                command += ['-change', old, new]
            arch = item['slices']['arm64']
            for rpath in arch['rpaths']:
                command += ['-delete_rpath', rpath]
            if arch['install_id']:
                command += ['-id', '@loader_path/' + target.name]
            if len(command) > 1:
                run(command + [target])
        for relative, item in plan['assets'].items():
            floor(work)
            copy_checked(Path(item['source']), work / relative, item['sha256'])
            (work / relative).chmod(0o644)
        compile_adapters(work, plan, guard)
        records = inspect_payload(work, plan)
        write_receipt(work, plan, 'local-runtime', {'images': records,
                      'original_images': 41, 'adapter_images': 2,
                      'signing_and_runtime_validation': 'pending explicit interactive check',
                      'not_an_app_or_installer': True})


def record_closure(binary, plugins, deadline):
    """The images `binary` and GStreamer's plugin scanner load with `plugins`, by canonical path,
    resolved in the context of both executables as the relocation is."""
    gst = (HOMEBREW / 'opt/gstreamer').resolve(strict=True)
    folder = gst / 'lib/gstreamer-1.0'
    missing = sorted(name for name in plugins if not (folder / name).is_file())
    if missing:
        raise ValueError('reviewed plugins are not installed: ' + ', '.join(missing))
    executables = [binary.resolve(strict=True),
                   (gst / 'libexec/gstreamer-1.0/gst-plugin-scanner').resolve(strict=True)]
    roots = executables + [(folder / name).resolve(strict=True) for name in sorted(plugins)]
    images = {}

    def facts(path):
        if str(path) not in images:
            if len(images) >= 512:
                raise ValueError('closure image bound reached')
            images[str(path)] = image_info(path, 'arm64', min(15, remaining(deadline)))
        return images[str(path)]['slices']['arm64']

    queue = [(root, executable) for root in roots
             for executable in (executables if root not in executables else [root])]
    seen = set()
    while queue:
        path, executable = queue.pop()
        if (path, executable) in seen:
            continue
        seen.add((path, executable))
        arch = facts(path)
        bases = [expand(p, path, executable) for p in arch['rpaths']]
        bases += [expand(p, executable, executable) for p in facts(executable)['rpaths']]
        for edge in arch['dependencies']:
            target = resolve_load(edge['load'], path, executable, bases, (HOMEBREW,))
            if target:
                queue.append((target, executable))
    return roots, images


def tilolang_closure(binary, out, plugins, libraries, packages):
    """Stage `binary` (an arm64 tilolang-app) at `out/bin/` and the reviewed GStreamer closure it
    and the scanner load at `out/lib/gstreamer/`, every load relative to its loader. Fails on an
    image or package the reviewed lists do not name, or one they name that is missing."""
    deadline = time.monotonic() + 300
    roots, images = record_closure(Path(binary), plugins, deadline)
    found = {Path(path).name for path in images} - {roots[0].name, roots[1].name} - set(plugins)
    differences = []
    if found != set(libraries):
        differences.append('libraries: unreviewed '
                           f'{sorted(found - set(libraries))}, missing {sorted(set(libraries) - found)}')
    evidence = {}
    for path in images:
        if Path(path) == roots[0]:
            continue
        record = formula_evidence(Path(path), (HOMEBREW,), deadline)
        if not record:
            raise ValueError(f'no Homebrew package evidence for {Path(path).name}')
        evidence[record['package_root']] = record
    names = {Path(root).parent.name for root in evidence}
    if names != set(packages) or len(names) != len(evidence):
        differences.append('packages: unreviewed '
                           f'{sorted(names - set(packages))}, missing {sorted(set(packages) - names)}')
    if differences:
        raise ValueError('the closure differs from the reviewed ' + '; '.join(differences))
    for source, item in images.items():
        path = Path(source)
        item['destination'] = ('bin/' + path.name if path == roots[0] else
                               GSTREAMER + '/Helpers/' + path.name if path == roots[1] else
                               GSTREAMER + '/PlugIns/' + path.name if path in roots[2:] else
                               GSTREAMER + '/Frameworks/' + path.name)
    if len({item['destination'].casefold() for item in images.values()} | {TILOLANG_SHIM.casefold()}) != len(images) + 1:
        raise ValueError('case-insensitive runtime filename collision')
    intl = relocation_map(images, TILOLANG_SHIM)
    assets, prefixes = resources({'packages': evidence}, deadline, packages=None, into=GSTREAMER)
    media_assets(assets, evidence, prefixes['gstreamer'], deadline)
    plan = {'images': images, 'assets': assets, 'locale_prefixes': prefixes, 'intl': intl,
            'profile_sha256': hashlib.sha256(json.dumps(
                {Path(k).name: v['sha256'] for k, v in images.items()}, sort_keys=True).encode()).hexdigest(),
            'adapter_sources': {'local_runtime_intl.c': digest(HERE / 'local_runtime_intl.c')},
            'extended_locale_catalogs': True, 'media_data': True}
    out.mkdir(parents=True)
    for source, item in plan['images'].items():
        target = out / item['destination']
        copy_checked(Path(source), target, item['sha256'])
        target.chmod(0o755)
        command = ['/usr/bin/install_name_tool']
        for old, new in item['changes']:
            command += ['-change', old, new]
        arch = item['slices']['arm64']
        for rpath in arch['rpaths']:
            command += ['-delete_rpath', rpath]
        if arch['install_id']:
            command += ['-id', '@loader_path/' + target.name]
        if len(command) > 1:
            run(command + [target])
    for relative, item in assets.items():
        copy_checked(Path(item['source']), out / relative, item['sha256'])
        (out / relative).chmod(0o644)
    compile_adapters(out, plan, None, shim=TILOLANG_SHIM, launcher=None)
    records = inspect_payload(out, plan, shim=TILOLANG_SHIM, launcher=None)
    # What was reviewed and built, by name and hash, sealed with the runtime it describes.
    receipt = {'images': {item['destination']: item['sha256'] for item in images.values()},
               'packages': {Path(root).parent.name: {'version': record['version'],
                                                     'receipt_sha256': record['receipt_sha256']}
                            for root, record in evidence.items()},
               'staged': {relative: facts['sha256'] for relative, facts in records.items()}}
    (out / GSTREAMER / 'Resources/closure.json').write_text(json.dumps(receipt, indent=1, sort_keys=True))


def media_assets(assets, evidence, gst_locale, deadline):
    """Subtitles' dictionary and notice, the bundle's font configuration, and the plugin sets'
    message catalogs."""
    import collection_app_runtime as app
    import collection_media_runtime as media
    packages = {Path(root).parent.name: Path(root) for root in evidence}
    for name, relative in media.DATA.items():
        target = ('Resources/libthai/thbrk.tri' if name == 'libthai' else
                  'Resources/Notices/libunibreak/LICENCE')
        add_asset(assets, packages[name] / relative, f'{GSTREAMER}/{target}', (packages[name],), deadline)
    config = HERE / 'local_media_fonts.conf'
    add_asset(assets, config, GSTREAMER + '/Resources/fontconfig/fonts.conf', (HERE,), deadline)
    locale = Path(gst_locale)
    for messages in sorted(locale.glob('*/LC_MESSAGES')):
        for domain in sorted(app.DOMAINS):
            source = messages / (domain + '.mo')
            if source.is_file() and not source.is_symlink():
                add_asset(assets, source, f'{GSTREAMER}/Resources/locale/{source.relative_to(locale)}',
                          (locale,), deadline)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('stage', choices=['plan', 'stage', 'verify', 'tilolang'])
    parser.add_argument('--profile', type=Path)
    parser.add_argument('--profile-sha256')
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--compiler-guard', type=Path)
    parser.add_argument('--resume', action='store_true')
    args = parser.parse_args(argv)
    try:
        if args.stage == 'tilolang':
            import collection_media_runtime as media
            if not args.binary or not args.out:
                raise ValueError('tilolang takes --binary and --out')
            tilolang_closure(args.binary, args.out, media.TILOLANG_PLUGINS,
                             media.TILOLANG_LIBRARIES, media.TILOLANG_PACKAGES)
            print(json.dumps({'status': 'staged', 'out': str(args.out)}))
            return 0
        if not args.profile or not args.profile_sha256:
            raise ValueError('--profile and --profile-sha256 are required')
        plan = make_plan(args.profile, args.profile_sha256)
        if args.stage == 'plan':
            print(json.dumps(plan, indent=2, sort_keys=True))
            return 0
        if not args.out:
            raise ValueError('--out is required')
        if args.stage == 'stage':
            if not args.compiler_guard or not args.compiler_guard.is_file() or args.compiler_guard.name != 'rustc-budget.py':
                raise ValueError('stage requires the existing shared --compiler-guard')
            stage(args.out, plan, args.compiler_guard.resolve(), args.resume)
        record = check_receipt(args.out / 'local-runtime', plan, 'local-runtime')
        actual = inspect_payload(args.out / 'local-runtime', plan)
        if record['facts']['images'] != actual:
            raise ValueError('staged Mach-O metadata changed')
        print(json.dumps({'status': 'relocated-static-payload-verified', 'original_images': 41,
                          'adapter_images': 2, 'signed': False, 'runtime_qualified': False}))
        return 0
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f'local runtime: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(main())

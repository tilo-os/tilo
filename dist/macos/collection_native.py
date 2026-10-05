"""Local Apple tools and conservative validation for SDK-generated bundles."""
from __future__ import annotations

import os
from pathlib import Path
import plistlib
import posixpath
import re
import subprocess

from collection import digest, safe_name

MACHO = {bytes.fromhex(h) for h in ['feedface', 'cefaedfe', 'feedfacf', 'cffaedfe', 'cafebabe', 'bebafeca', 'cafebabf', 'bfbafeca']}


def run(command, timeout=120, cwd=None, env=None, combine=True):
    result = subprocess.run([str(x) for x in command], cwd=cwd, env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    if result.returncode:
        # Only tool diagnostics, never environment variables or credential arguments.
        raise ValueError(f'{Path(command[0]).name}: exit {result.returncode}: {result.stderr.decode(errors="replace").strip()}')
    return (result.stdout + (result.stderr if combine else b'')).decode(errors='replace')


def select_identity(output, requested, mode):
    if requested == 'ad-hoc' and mode == 'development':
        return {'sha1': '-', 'name': 'ad-hoc', 'team': None}
    prefix = 'Developer ID Application:' if mode == 'distribution' else 'Apple Development:'
    identities = []
    for sha1, name in re.findall(r'\b([A-Fa-f0-9]{40})\s+"([^"]+)"', output):
        team = re.search(r'\(([A-Z0-9]{10})\)$', name)
        if name.startswith(prefix) and team and (not requested or requested in {sha1, sha1.lower(), name}):
            identities.append({'sha1': sha1.upper(), 'name': name, 'team': team[1]})
    identities = list({i['sha1']: i for i in identities}.values())
    if not identities:
        raise ValueError(f'no matching valid {prefix} identity; use security find-identity -v -p codesigning; no Keychain setup is performed')
    if len(identities) != 1:
        raise ValueError('ambiguous signing identity; choose an exact Keychain SHA-1 with --identity')
    return identities[0]


def machos(app):
    result = []
    for path in sorted(app.rglob('*')):
        if path.is_symlink():
            if not path.resolve().is_relative_to(app.resolve()) or not path.exists():
                raise ValueError(f'escaping or broken symlink: {path}')
        elif path.is_file():
            with path.open('rb') as stream:
                if stream.read(4) in MACHO:
                    result.append(path)
        elif not path.is_dir():
            raise ValueError(f'unsupported filesystem object: {path}')
    return result


def expand_loader(value, binary, main):
    for prefix, base in [('@loader_path', binary.parent), ('@executable_path', main.parent)]:
        if value == prefix or value.startswith(prefix + '/'):
            return (base / value[len(prefix):].lstrip('/')).resolve()
    if value.startswith('/'):
        return Path(value).resolve()
    raise ValueError(f'unresolved Mach-O load path: {value}')


def system_library(path):
    normalized = posixpath.normpath(path)
    return normalized.startswith('/System/Library/') or normalized.startswith('/usr/lib/')


def load_paths(binary, main, app, arch):
    loads = run(['/usr/bin/otool', '-arch', arch, '-l', binary])
    rpaths = re.findall(r'cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (.+?) \(offset', loads)
    if binary != main:
        main_loads = run(['/usr/bin/otool', '-arch', arch, '-l', main])
        rpaths += re.findall(r'cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (.+?) \(offset', main_loads)
    bases = [expand_loader(p, binary, main) for p in rpaths]
    if any(not base.is_relative_to(app.resolve()) for base in bases):
        raise ValueError(f'external rpath in {binary}')
    dependencies = []
    for line in run(['/usr/bin/otool', '-arch', arch, '-L', binary]).splitlines():
        if not line.startswith('\t'):
            continue
        value = line.strip().split(' (compatibility version', 1)[0]
        dependencies.append(value)
        if system_library(value):
            continue
        # This adapter supports the SDK's two statically linked executables.
        # Frameworks and dylibs require a separate, reviewed signing adapter.
        raise ValueError(f'external or unresolved/non-system library {value} in {binary}; '
                         'use collection.py audit-runtime to inspect dependency/profile blockers before rebuilding; '
                         'a reviewed relocation adapter is required')
    return dependencies


def inspect_bundle_metadata(app, entry):
    if app.is_symlink() or not app.is_dir():
        raise ValueError(f'missing app or symlink bundle: {app}')
    binaries = machos(app)
    plist_path = app / 'Contents/Info.plist'
    info = plistlib.loads(plist_path.read_bytes())
    required = {'CFBundleIdentifier': entry['id'], 'CFBundleVersion': entry['version'],
                'CFBundleShortVersionString': entry['version'], 'CFBundlePackageType': 'APPL',
                'CFBundleExecutable': entry['binary']}
    if any(info.get(k) != v for k, v in required.items()):
        raise ValueError(f'bundle identity/version/type mismatch: {app}')
    main = app / 'Contents/MacOS' / safe_name(info['CFBundleExecutable'])
    helper = app / 'Contents/MacOS/tilo-runtime-mcp'
    if any(p.is_dir() and p.suffix in {'.app', '.framework', '.appex', '.xpc', '.bundle'} for p in app.rglob('*')):
        raise ValueError(f'unreviewed nested code bundle in {app}')
    manifest = app / 'Contents/Resources/tilo-app.toml'
    if digest(manifest) != entry['manifest_sha256']:
        raise ValueError(f'bundled manifest mismatch: {app}')
    if not info.get('LSMinimumSystemVersion'):
        raise ValueError(f'missing macOS deployment floor: {app}')
    icon = info.get('CFBundleIconFile')
    if icon and not (app / 'Contents/Resources' / (safe_name(icon) if icon.endswith('.icns') else safe_name(icon) + '.icns')).is_file():
        raise ValueError(f'missing declared icon: {app}')
    if entry.get('icon_sha256') and (not icon or digest(app / 'Contents/Resources/AppIcon.icns') != entry['icon_sha256']):
        raise ValueError(f'bundled icon mismatch: {app}')
    return main, helper, info, binaries


def inspect_bundle(app, entry, architecture):
    main, helper, info, binaries = inspect_bundle_metadata(app, entry)
    # SDK assembler has precisely two code objects. Unknown helpers/frameworks
    # need a reviewed per-process policy, never a blanket --deep signature.
    unexpected = set((app / 'Contents/MacOS').iterdir()) - {main, helper}
    unexpected.update(p for p in app.rglob('*') if p.is_file() and os.access(p, os.X_OK) and p not in binaries)
    if set(binaries) != {main, helper} or main == helper or unexpected:
        raise ValueError(f'unexpected or missing nested code in {app}; expected SDK app + tilo-runtime-mcp; use its specialized packager')
    expected = {'arm64', 'x86_64'} if architecture == 'universal' else {architecture}
    facts = []
    for binary in binaries:
        if not os.access(binary, os.X_OK):
            raise ValueError(f'non-executable Mach-O: {binary}')
        actual = set(run(['/usr/bin/lipo', '-archs', binary]).strip().split())
        if actual != expected:
            raise ValueError(f'architecture mismatch {binary}: {sorted(actual)} expected {sorted(expected)}')
        for arch in actual:
            loads = run(['/usr/bin/otool', '-arch', arch, '-l', binary])
            minimum = re.search(r'\bminos ([0-9.]+)', loads)
            if not minimum:
                minimum = re.search(r'cmd LC_VERSION_MIN_MACOSX\n\s+cmdsize \d+\n\s+version ([0-9.]+)', loads)
            version = lambda value: tuple((list(map(int, value.split('.'))) + [0, 0])[:3])
            if not minimum or version(minimum[1]) > version(info['LSMinimumSystemVersion']):
                raise ValueError(f'Mach-O deployment floor exceeds Info.plist or is missing: {binary}')
        facts.append({'path': str(binary.relative_to(app)), 'architectures': sorted(actual),
                      'libraries': {arch: load_paths(binary, main, app, arch) for arch in sorted(actual)}})
    return facts


def read_entitlements(path):
    if path.is_symlink():
        raise ValueError('entitlements must be a regular file')
    value = plistlib.loads(path.read_bytes())
    # SDK capability policy: no debugging/JIT/library-validation or app-group
    # exceptions smuggled in through a sidecar. Broader policy needs review.
    allowed = {'com.apple.security.device.camera', 'com.apple.security.device.audio-input',
               'com.apple.security.personal-information.location', 'com.apple.security.network.client',
               'com.apple.security.files.user-selected.read-write', 'com.apple.security.print'}
    if not isinstance(value, dict) or any(k not in allowed or v is not True for k, v in value.items()):
        raise ValueError(f'unreviewed entitlements in {path}')
    return value


def sign_bundle(app, policy, identity, offline):
    read_entitlements(policy)
    timestamp = '--timestamp=none' if offline or identity['sha1'] == '-' else '--timestamp'
    common = ['/usr/bin/codesign', '--force', '--sign', identity['sha1'], '--options', 'runtime', timestamp]
    # Preserve the SDK helper policy: no per-app device entitlements on the helper.
    # Runtime capture qualification is separate from packaging these SDK bundles.
    from collection import ROOT
    run(common + ['--entitlements', ROOT / 'dist/macos/Tilo.entitlements', app / 'Contents/MacOS/tilo-runtime-mcp'], timeout=60)
    run(common + ['--entitlements', policy, app], timeout=60)


def verify_signatures(app, identity, mode, architecture):
    code = machos(app)
    for item in code + [app]:
        command = ['/usr/bin/codesign', '--verify', '--strict', '--all-architectures']
        if identity['sha1'] != '-':
            command += ['-R', f'=anchor apple generic and certificate leaf = H"{identity["sha1"]}"']
        run(command + [item])
        arches = ['arm64', 'x86_64'] if architecture == 'universal' else [architecture]
        for arch in arches:
            details = run(['/usr/bin/codesign', '-d', '--verbose=4', '--arch', arch, item])
            flags = re.search(r'flags=0x([0-9a-fA-F]+)', details)
            if not flags or not int(flags[1], 16) & 0x10000:
                raise ValueError(f'hardened runtime missing: {item}')
            if identity['sha1'] == '-':
                if 'Signature=adhoc' not in details:
                    raise ValueError(f'expected ad-hoc signature: {item}')
            elif f'TeamIdentifier={identity["team"]}' not in details or f'Authority={identity["name"]}' not in details:
                raise ValueError(f'wrong signing team/authority: {item}')
            if mode == 'distribution' and 'Timestamp=' not in details:
                raise ValueError(f'secure timestamp missing: {item}')
    run(['/usr/bin/codesign', '--verify', '--deep', '--strict', '--all-architectures', app])

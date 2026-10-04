#!/usr/bin/env python3
"""Bounded, explicit macOS fixture test: never launch, mount or use a Keychain key."""
import argparse
from pathlib import Path
import plistlib
import shutil
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import collection as c
import collection_native as n
import collection_stages as s

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--compiler-guard', type=Path, required=True)
args = p.parse_args()
if not args.compiler_guard.is_file() or args.compiler_guard.name != 'rustc-budget.py':
    raise SystemExit('Expected the existing shared rustc-budget.py')
s.disk_floor(c.ROOT)
with tempfile.TemporaryDirectory(prefix='tilo dmg fixture ') as temporary:
    root = Path(temporary)
    source = root / 'main.c'
    source.write_text('int main(void) { return 0; }\n')
    import os
    env = dict(os.environ, CLOUD_RUSTC_JOBS='2')
    env.pop('CLOUD_RUSTC_LOCK_DIR', None)
    # A single small compiler invocation takes the existing shared budget slot.
    n.run([args.compiler_guard, '/usr/bin/clang', '-arch', 'arm64', '-mmacosx-version-min=13.0', source, '-o', root / 'fixture'], env=env, timeout=60)
    plan = c.make_plan(c.ROOT, 'calculator', 'arm64', 'development')
    entry = plan['apps'][0]
    app = root / entry['bundle']
    (app / 'Contents/MacOS').mkdir(parents=True)
    (app / 'Contents/Resources').mkdir()
    for name in ['calculator', 'tilo-runtime-mcp']:
        shutil.copy2(root / 'fixture', app / 'Contents/MacOS' / name)
    shutil.copy2(c.ROOT / entry['manifest'], app / 'Contents/Resources/tilo-app.toml')
    info = {'CFBundleExecutable': 'calculator', 'CFBundleIdentifier': entry['id'],
            'CFBundleVersion': entry['version'], 'CFBundleShortVersionString': entry['version'],
            'CFBundlePackageType': 'APPL', 'LSMinimumSystemVersion': '13.0'}
    (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
    policy = root / 'Calculator.entitlements'
    policy.write_bytes(plistlib.dumps({}))
    n.inspect_bundle(app, entry, 'arm64')
    identity = n.select_identity('', 'ad-hoc', 'development')
    out = root / 'output with spaces'
    with c.transaction(out, 'built', False, plan) as work:
        (work / 'apps').mkdir()
        (work / 'entitlements').mkdir()
        shutil.copytree(app, work / 'apps' / entry['bundle'])
        shutil.copy2(policy, work / 'entitlements' / policy.name)
        c.write_receipt(work, plan, 'built', {'origin': 'inert C fixture'})
    options = c.parser().parse_args(['sign', '--identity', 'ad-hoc', '--apps', 'calculator', '--arch', 'arm64', '--offline', '--per-app', '--out', str(out)])
    s.execute(options, plan)
    options.stage = 'package'
    s.execute(options, plan)
    options.resume = True
    s.execute(options, plan)
    options.stage = 'verify'
    s.execute(options, plan)
    # Production verifier must reject a changed sealed resource.
    (out / 'signed/apps' / entry['bundle'] / 'Contents/Resources/extra').write_text('tampered')
    try:
        n.verify_signatures(out / 'signed/apps' / entry['bundle'], identity, 'development', 'arm64')
    except ValueError:
        pass
    else:
        raise AssertionError('tampered app accepted')
    print('PASS: Mach-O fixture, inside-out ad-hoc signing, DMG creation/verification and tamper rejection. No Tilo executable built or launched.')

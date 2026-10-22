#!/usr/bin/env python3
"""Seal the current Browser and native CEF for a disposable receiving image.

Signing identities are explicitly provided by the operator. Generated developer
keys are suitable only for isolated qualification, never publisher authorization.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[3]


def run(command):
    subprocess.run(list(map(str, command)), cwd=ROOT, check=True)


def prepare(output, sidecar, pins, runtime_identity, app_identity):
    output.mkdir(mode=0o700)
    tools = ROOT / 'target/release'
    spec = importlib.util.spec_from_file_location('engine_release', ROOT / 'tools/apps/browser_runtime_release.py')
    release = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = release
    spec.loader.exec_module(release)
    version = release.runtime_version()
    recorder_spec = importlib.util.spec_from_file_location('browser_sources', ROOT / 'apps/imager/packaging/record-capture-cargo.py')
    recorder = importlib.util.module_from_spec(recorder_spec)
    recorder_spec.loader.exec_module(recorder)
    before = recorder.sources()
    lock = json.loads((ROOT / 'scripts/browser/cef-lock.json').read_text())
    platform = {'x86_64': 'linux64', 'aarch64': 'linuxarm64'}[os.uname().machine]
    # Never trust the extracted SDK's cache marker as a byte receipt. The
    # canonical downloader verifies the archive again before fresh extraction.
    sdk_directory = ROOT / 'target/cef-sdk' / lock['platforms'][platform]['name'].removesuffix('.tar.bz2')
    if sdk_directory.is_symlink():
        raise ValueError('redirected extracted CEF SDK')
    if sdk_directory.exists():
        shutil.rmtree(sdk_directory)
    run(['python3', ROOT / 'scripts/browser/build-host.py', '--runtime', output / 'engine'])
    if recorder.sources() != before:
        raise ValueError('Browser native source changed during compilation')
    (output / 'native-source-inputs.json').write_text(json.dumps(before, sort_keys=True) + '\n')
    cef = output / 'cef'
    run([tools / 'tilo-runtime-bundle', 'build', '--version', version,
         '--arch', os.uname().machine, '--from', '/', '--include', output / 'engine/chromium',
         '--identity', runtime_identity, '--out', cef])
    publisher = subprocess.check_output([tools / 'tilo-runtime-bundle', 'key', 'pubkey',
                                        '--identity', runtime_identity], text=True).strip()
    run([tools / 'tilo-runtime-bundle', 'verify', cef, '--require-signature',
         '--require-publisher', publisher, '--require-arch', os.uname().machine])
    packages = output / 'packages'
    run(['bash', ROOT / 'tools/apps/pack-runtime-package.sh', cef, 'so.tilo.runtime.chromium',
         'Browser engine', os.environ['TILO_VERSION'].removeprefix('imager-v').removeprefix('v'),
         app_identity, tools, packages])
    # Keep the canonical runtime-package metadata but seal the full offline form.
    import tarfile
    thin = next(packages.glob('*.tapp'))
    with tarfile.open(thin) as archive:
        manifest = archive.extractfile('manifest.toml').read()
    runtime_manifest = output / 'cef-package.toml'
    runtime_manifest.write_bytes(manifest)
    run([tools / 'tilo-app-package', 'pack', '--manifest', runtime_manifest, '--payload', cef,
         '--identity', app_identity, '--output', output / 'cef.tapp'])
    sdk = json.loads(pins.read_text())
    browser_manifest = output / 'browser.toml'
    shutil.copyfile(ROOT / 'apps/browser/tilo-app.toml', browser_manifest)
    run(['bash', ROOT / 'os/scripts/stamp-runtime.sh', browser_manifest,
         'tilo-runtime/' + sdk['runtime_version']])
    authored = tomllib.loads(browser_manifest.read_text())
    if authored['package']['optional-runtimes'] != ['tilo-runtime/' + version]:
        raise ValueError('Browser and current native CEF disagree')
    payload = output / 'browser'
    (payload / 'bin').mkdir(parents=True)
    shutil.copyfile(ROOT / 'target/thin-app/browser', payload / 'bin/browser')
    (payload / 'bin/browser').chmod(0o755)
    shutil.copyfile(ROOT / 'apps/browser/surface.toml', payload / 'surface.toml')
    run([tools / 'tilo-app-package', 'pack', '--manifest', browser_manifest, '--payload', payload,
         '--identity', app_identity, '--output', output / 'browser.tapp'])
    key = subprocess.check_output([tools / 'tilo-runtime-bundle', 'key', 'pubkey',
                                  '--identity', app_identity], text=True).strip()
    for package in ('browser.tapp', 'cef.tapp'):
        run([tools / 'tilo-app-package', 'verify', '--trusted-key', key, output / package])
    ledger = {'schema': 1, 'cef_version': version, 'cef_publisher': publisher,
              'app_publisher': key,
              'browser_sha256': recorder.facts(payload / 'bin/browser')['sha256'],
              'packages': {name: recorder.facts(output / name)['sha256']
                           for name in ('browser.tapp', 'cef.tapp')}}
    (output / 'receiving.json').write_text(json.dumps(ledger, sort_keys=True) + '\n')
    return sdk, version, publisher, key


def stage(root, output, pins, cohort, qualification=False):
    """Stage verified normal packages; only disposable images enroll test keys."""
    spec = importlib.util.spec_from_file_location('receiving_qualifier', ROOT / 'apps/imager/packaging/qualify-capture-release.py')
    qualifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(qualifier)
    ledger = qualifier.document(output / 'receiving.json')
    sdk = qualifier.document(pins)
    expected = cohort['roles']['browser']['sha256']
    if ledger['schema'] != 1 or ledger['browser_sha256'] != expected:
        raise ValueError('signed Browser differs from current compiler role')
    sources = qualifier.document(output / 'native-source-inputs.json')
    for name, digest in cohort['source_inputs'].items():
        if sources.get(name, {}).get('sha256') != digest:
            raise ValueError('native CEF source differs from admitted compiler cohort')
    tools = ROOT / 'target/release'
    import tarfile
    for name in ('browser.tapp', 'cef.tapp'):
        qualifier.bound_file(output / name, ledger['packages'][name])
        run([tools / 'tilo-app-package', 'verify', '--trusted-key', ledger['app_publisher'], output / name])
    with tarfile.open(output / 'browser.tapp') as archive:
        authored = tomllib.loads(archive.extractfile('manifest.toml').read().decode())
    if (authored['package']['runtime'] != 'tilo-runtime/' + sdk['runtime_version']
            or authored['package']['optional-runtimes'] != ['tilo-runtime/' + ledger['cef_version']]):
        raise ValueError('signed Browser receives another SDK/CEF graph')
    with tempfile.TemporaryDirectory(prefix='tilo-browser-admission-') as scratch:
        payload = Path(scratch).resolve() / 'payload'
        run([tools / 'tilo-app-package', 'unpack', '--dest', payload, output / 'browser.tapp'])
        qualifier.bound_file(payload / 'bin/browser', expected)
        qualifier.bound_file(payload / 'surface.toml', cohort['source_inputs']['apps/browser/surface.toml'])
    sidecar = pins.parent / 'sidecar'
    bundles = ((sidecar / 'runtime' / sdk['runtime_version'], sdk['runtime_version'], sdk['runtime_publisher']),
               (output / 'cef', ledger['cef_version'], ledger['cef_publisher']))
    for bundle, version, publisher in bundles:
        run([tools / 'tilo-runtime-bundle', 'verify', bundle, '--require-signature',
             '--require-arch', os.uname().machine, '--require-publisher', publisher])
        manifest = tomllib.loads((bundle / 'bundle.toml').read_text())
        if manifest['version'] != version:
            raise ValueError('signed receiving runtime version differs')
        if bundle == bundles[0][0]:
            destination = root / 'usr/lib/tilo-runtime' / version
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(bundle, destination)
        # CEF is installed below by the actual full runtime package. Copying it
        # first makes normal replacement try to rename a lower-layer directory,
        # which overlayfs refuses with EXDEV.
    sdk_manifest = tomllib.loads((bundles[0][0] / 'bundle.toml').read_text())
    if sdk_manifest['root_hash'] != sdk['runtime_root_hash']:
        raise ValueError('receiving SDK root differs from selected capture pins')
    qualifier.bound_file(bundles[0][0] / 'lib/libruntime_dylib.so', cohort['roles']['runtime']['sha256'])
    qualifier.bound_file(bundles[0][0] / 'bin/tilo-launcher', cohort['roles']['launcher']['sha256'])
    share = root / 'usr/share/tilo'
    share.mkdir(parents=True, exist_ok=True)
    for name, value in (('account-consumer-runtime-version', sdk['runtime_version']),
                        ('account-consumer-runtime-publisher.hex', sdk['runtime_publisher']),
                        ('browser-runtime-publisher.hex', ledger['cef_publisher'])):
        path = share / name
        path.write_text(value + '\n')
        path.chmod(0o644)
    shutil.copytree(sidecar, root / 'usr/share/tilo-imager/capture', dirs_exist_ok=True)
    host = cohort['roles']['host']
    qualifier.bound_file(host['path'], host['sha256'])
    imager = root / 'usr/bin/tilo-imager'
    imager.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(host['path'], imager)
    imager.chmod(0o755)
    packages = root / 'qualification-packages'
    packages.mkdir()
    for name in ('browser.tapp', 'cef.tapp'):
        shutil.copyfile(output / name, packages / name)
        qualifier.bound_file(packages / name, ledger['packages'][name])
    publishers = (sdk['runtime_publisher'], ledger['cef_publisher'])
    recipe = ''
    if qualification:
        # This exact enrollment exists only in the disposable image. It does
        # not assert production publisher authorization.
        trust = root / 'etc/tilo/runtime-bundle-trusted-publishers.txt'
        with trust.open('a') as target:
            target.write(''.join(publisher + '\n' for publisher in publishers))
        script = root / 'usr/libexec/tilo-service-session-qualification.py'
        script.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / 'apps/browser/chromium/tests/service_sessions.py', script)
        script.chmod(0o755)
    else:
        for publisher in publishers:
            recipe += 'RUN grep -Fx ' + publisher + ' /etc/tilo/runtime-bundle-trusted-publishers.txt\n'
    for name in ('cef.tapp', 'browser.tapp'):
        # The normal installer refuses mutable installed component pins. Buildah
        # supplies this exact staged pin as a read-only rootless build mount;
        # installed application data remains writable at its ordinary path.
        recipe += ('RUN --mount=type=bind,source=rootfs/usr/share/tilo/browser-runtime-publisher.hex,'
                   'target=/usr/share/tilo/browser-runtime-publisher.hex,ro,relabel=shared '
                   'TILO_APPS_DATA_DIR=/usr/share/tilo-apps TILO_RUNTIME_BUNDLE_DIR=/usr/lib/tilo-runtime '
                   '/usr/libexec/tilo/tilo-install install /qualification-packages/' + name
                   + ' --force --pinned --require-publisher ' + ledger['app_publisher']
                   + (' --allow-self-signed' if qualification else '') + '\n')
    # This tree contains public installed payloads, never signing identities or
    # session state. A private operator umask must not make /usr untraversable
    # to the genuine non-root receiving user after COPY into the image.
    for directory in (root, *root.rglob('*')):
        if directory.is_dir():
            directory.chmod(0o755)
    return recipe + 'RUN rm -rf /qualification-packages\n'


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--sidecar', type=Path, required=True)
    parser.add_argument('--pins', type=Path, required=True)
    args = parser.parse_args()
    prepare(args.output, args.sidecar, args.pins, os.environ['TILO_CAPTURE_RUNTIME_IDENTITY'],
            os.environ['TILO_CAPTURE_SERVICE_IDENTITY'])

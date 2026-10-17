#!/usr/bin/env python3
"""Build the signed worker first, then an exact host/receiver qualification cohort."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def run(command, env):
    subprocess.run([str(value) for value in command], cwd=ROOT, env=env, check=True)


def load_qualifier():
    from importlib.util import spec_from_file_location, module_from_spec
    spec = spec_from_file_location('capture_qualifier', HERE / 'qualify-capture-release.py')
    qualifier = module_from_spec(spec)
    spec.loader.exec_module(qualifier)
    return qualifier


def qualification_image(output, env, cohort):
    qualifier = load_qualifier()
    # Install actual observed consumers and current text prerequisites in a new
    # rootless-built layer. The immutable prerequisite base supplies native deps.
    value = qualifier.admit_cohort(cohort, ROOT / 'target/release/imager', True)
    with tempfile.TemporaryDirectory(prefix='tilo-capture-image-') as scratch:
        context = Path(scratch) / 'context'
        prerequisites = qualifier.fixed_module(ROOT / 'os/scripts/installer-receive-fixture/prerequisite-image.py',
                                               'capture_prerequisites')
        prerequisites.assemble(context, env['TILO_CAPTURE_QUALIFIER_BASE'])
        for role, destination in qualifier.BINDINGS.items():
            target = context / 'rootfs' / destination.lstrip('/')
            target.parent.mkdir(parents=True, exist_ok=True)
            qualifier.bound_file(value['roles'][role]['path'], value['roles'][role]['sha256'])
            shutil.copyfile(value['roles'][role]['path'], target)
            target.chmod(0o755)
        for source, destination in qualifier.PLATFORM_SCRIPTS.items():
            qualifier.bound_file(ROOT / source, value['source_inputs'][source])
            target = context / 'rootfs' / destination.lstrip('/')
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / source, target)
            target.chmod(0o755)
        browser = qualifier.fixed_module(HERE / 'prepare-capture-browser.py', 'capture_browser')
        recipe = 'FROM ' + env['TILO_CAPTURE_QUALIFIER_BASE'] + '\nCOPY rootfs/ /\n'
        recipe += browser.stage(context / 'rootfs', output / 'browser-release', output / 'pins.json',
                                value, qualification=True)
        (context / 'Containerfile').write_text(recipe)
        iid = Path(scratch) / 'iid'
        run(['podman', 'build', '--pull=never', '--network=none', '--iidfile', iid, context], env)
        image = iid.read_text().strip()
        if not re.fullmatch('sha256:[0-9a-f]{64}', image):
            raise ValueError('qualification layer has no immutable identity')
        # Native execution exports this rootless filesystem into an owned
        # privileged namespace; it never imports into rootful Podman storage.
    return image


def prepare(output, feature, env, build_only=False):
    # Refuse missing operator authority before allocating a build or touching Cargo.
    for name in ('TILO_VERSION', 'TILO_CAPTURE_RUNTIME_IDENTITY',
                 'TILO_CAPTURE_SERVICE_IDENTITY', 'TILO_CAPTURE_QUALIFIER_BASE',
                 'TILO_CAPTURE_SOURCE_ASSETS'):
        if not env.get(name):
            raise ValueError('required capture release input: ' + name)
    if not re.fullmatch('sha256:[0-9a-f]{64}', env['TILO_CAPTURE_QUALIFIER_BASE']):
        raise ValueError('capture qualifier base must be an immutable local image ID')
    qualifier = load_qualifier()
    qualifier.source_assets(Path(env['TILO_CAPTURE_SOURCE_ASSETS']))
    output = output.absolute()
    output.mkdir(mode=0o700)
    env = dict(env, TILO_CAPTURE_BUILD_EVIDENCE=str(output / 'evidence'))
    # The canonical recorder observes actual fresh Cargo streams, not saved receipts.
    recorder = HERE / 'record-capture-cargo.py'
    env['CARGO'] = str(recorder)
    clean = dict(env)
    for name in ('TILO_CAPTURE_PINS', 'TILO_CAPTURE_SIDECAR', 'TILO_REQUIRE_CAPTURE'):
        clean.pop(name, None)
    run(['bash', HERE / 'capture-native-preflight.sh'], clean)
    run([recorder, 'build', '--locked', '--release', '-p', 'shell', '--bin', 'shell',
         '-p', 'tilo-fs', '--bin', 'fs', '-p', 'tilo-control', '--bin', 'tilo-control',
         '-p', 'account-custody', '--bin', 'tilo-account-credential',
         '-p', 'tilo-install-helpers', '--bin', 'tilo-staging-handoff', '--bin', 'tilo-install-handoff-copy',
         '-p', 'tilo-firstboot-files', '--bin', 'tilo-firstboot-files'], clean)
    run(['bash', HERE / 'package-capture-service.sh', '--with-writer', '--build-only',
         output / 'sidecar', output / 'pins.json'], clean)
    run([recorder, 'assemble', '--sidecar', output / 'sidecar', '--pins', output / 'pins.json'], clean)
    env.update(TILO_CAPTURE_PINS=str(output / 'pins.json'),
               TILO_CAPTURE_SIDECAR=str(output / 'sidecar'), TILO_REQUIRE_CAPTURE='1')
    run([recorder, 'build', '--locked', '--release', '-p', 'imager', '--bin', 'imager',
         '--bin', 'tilo-prepared-setup', '--bin', 'tilo-installer-prepared-setup-extract',
         '-p', 'installer', '--bin', 'tilo-installer', '--bin', 'tilo-firstboot-home-seeds',
         '--features', 'imager/' + feature], env)
    run([recorder, 'test', '--locked', '--release', '-p', 'imager', '-p', 'imager-backend', '--lib', '--no-run',
         '--features', feature + ',native-capture-qualification'], env)
    browser = qualifier.fixed_module(HERE / 'prepare-capture-browser.py', 'capture_browser')
    browser.prepare(
        output / 'browser-release', output / 'sidecar', output / 'pins.json',
        env['TILO_CAPTURE_RUNTIME_IDENTITY'], env['TILO_CAPTURE_SERVICE_IDENTITY'])
    cohort = output / 'cohort.json'
    run([recorder, 'cohort', '--evidence', output / 'evidence', '--output', cohort], env)
    env['TILO_CAPTURE_QUALIFICATION_COHORT'] = str(cohort)
    env['TILO_CAPTURE_SIGNING_PURPOSE'] = env.get('TILO_CAPTURE_SIGNING_PURPOSE', 'operator-provided')
    env['TILO_CAPTURE_BROWSER_RELEASE'] = str(output / 'browser-release')
    if build_only:
        # A dependency container may compile as namespace UID0. Admission and
        # native execution must subsequently run as the actual rootless operator.
        # This file records inputs only, never a qualification or readiness claim.
        names = ('TILO_CAPTURE_PINS', 'TILO_CAPTURE_SIDECAR',
                 'TILO_CAPTURE_QUALIFICATION_COHORT', 'TILO_CAPTURE_SOURCE_ASSETS',
                 'TILO_REQUIRE_CAPTURE', 'TILO_CAPTURE_BUILD_EVIDENCE',
                 'TILO_CAPTURE_SIGNING_PURPOSE', 'TILO_CAPTURE_BROWSER_RELEASE',
                 'TILO_CAPTURE_QUALIFIER_BASE')
        settings = {name: env[name] for name in names}
        (output / 'build-inputs.json').write_text(json.dumps(settings, sort_keys=True) + '\n')
        return settings
    image = qualification_image(output, env, cohort)
    env['TILO_CAPTURE_QUALIFIER_IMAGE'] = image
    run(['python3', HERE / 'qualify-capture-release.py', '--host', ROOT / 'target/release/imager',
         '--pins', env['TILO_CAPTURE_PINS'], '--sidecar', env['TILO_CAPTURE_SIDECAR'],
         '--arch', os.uname().machine, '--cohort', cohort, '--image', image,
         '--source-assets', env['TILO_CAPTURE_SOURCE_ASSETS'], '--output', output / 'qualification'], env)
    # These are inputs for another actual qualification, never a readiness flag.
    settings = {name: env[name] for name in ('TILO_CAPTURE_PINS', 'TILO_CAPTURE_SIDECAR',
                'TILO_CAPTURE_QUALIFICATION_COHORT', 'TILO_CAPTURE_QUALIFIER_IMAGE',
                'TILO_CAPTURE_SOURCE_ASSETS', 'TILO_REQUIRE_CAPTURE', 'TILO_CAPTURE_BUILD_EVIDENCE',
                'TILO_CAPTURE_SIGNING_PURPOSE', 'TILO_CAPTURE_BROWSER_RELEASE')}
    (output / 'environment.json').write_text(json.dumps(settings, sort_keys=True) + '\n')
    return settings


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--feature', choices=('host-media-writer', 'flatpak-media-writer'), required=True)
    parser.add_argument('--github-env', type=Path)
    parser.add_argument('--build-only', action='store_true',
                        help='compile and sign inputs only; native qualification remains required')
    args = parser.parse_args()
    settings = prepare(args.output, args.feature, dict(os.environ), args.build_only)
    if args.github_env:
        with args.github_env.open('a') as destination:
            for name, value in settings.items():
                if '\n' in value or '\r' in value:
                    raise ValueError('multiline capture environment input')
                destination.write(name + '=' + value + '\n')


if __name__ == '__main__':
    main()

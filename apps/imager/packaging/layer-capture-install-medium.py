#!/usr/bin/env python3
"""Layer the qualified Imager service and native installer onto a selected OS base."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import tempfile

from importlib.util import spec_from_file_location, module_from_spec

HERE = Path(__file__).resolve().parent
spec = spec_from_file_location('capture_release', HERE / 'prepare-capture-release.py')
release = module_from_spec(spec)
spec.loader.exec_module(release)


def layer(settings, base, output, env):
    if not re.fullmatch('sha256:[0-9a-f]{64}', base):
        raise ValueError('install medium requires an immutable selected local OS image')
    env = dict(env, **settings)
    for name in ('TILO_CAPTURE_PINS', 'TILO_CAPTURE_SIDECAR', 'TILO_CAPTURE_QUALIFICATION_COHORT',
                 'TILO_CAPTURE_QUALIFIER_IMAGE', 'TILO_CAPTURE_SOURCE_ASSETS',
                 'TILO_CAPTURE_BROWSER_RELEASE'):
        if not env.get(name):
            raise ValueError('missing medium capture input: ' + name)
    developer = env.get('TILO_CAPTURE_SIGNING_PURPOSE') in ('developer-qualification', 'developer-isolated')
    disposable = env.get('TILO_CAPTURE_DISPOSABLE_MEDIUM') == '1'
    if developer and not disposable:
        raise ValueError('developer keys are confined to an explicitly disposable qualification medium')
    env['TILO_REQUIRE_CAPTURE'] = '1'
    with tempfile.TemporaryDirectory(prefix='tilo-capture-medium-') as scratch:
        context = Path(scratch)
        root = context / 'rootfs'
        host = root / 'usr/bin/imager'
        host.parent.mkdir(parents=True)
        shutil.copyfile(release.ROOT / 'target/release/imager', host)
        host.chmod(0o755)
        # Execute the gate on the bytes destined for the medium, not an old
        # detached receipt or the base image's historical host.
        release.run(['bash', HERE / 'stage-capture-sidecar.sh',
                     release.ROOT / 'target/release/imager', root / 'usr', host], env)
        qualifier = release.load_qualifier()
        cohort = qualifier.admit_cohort(Path(env['TILO_CAPTURE_QUALIFICATION_COHORT']),
                                       host, True)
        # Publish the same actual platform consumers exercised by the full
        # receiver gate at their ordinary Linux installation locations.
        for role, destination in qualifier.BINDINGS.items():
            subject = cohort['roles'][role]
            qualifier.bound_file(subject['path'], subject['sha256'])
            target = root / destination.lstrip('/')
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(subject['path'], target)
            target.chmod(0o755)
            qualifier.bound_file(target, subject['sha256'])
        browser = qualifier.fixed_module(HERE / 'prepare-capture-browser.py', 'capture_browser')
        if disposable:
            trust = root / 'etc/tilo/runtime-bundle-trusted-publishers.txt'
            trust.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(release.ROOT / 'os/registry-runtime-publisher.hex', trust)
        recipe = 'FROM ' + base + '\nCOPY rootfs/ /\n'
        recipe += browser.stage(root, Path(env['TILO_CAPTURE_BROWSER_RELEASE']),
                                Path(env['TILO_CAPTURE_PINS']), cohort, qualification=disposable)
        (context / 'Containerfile').write_text(recipe)
        release.run(['podman', 'build', '--pull=never', '--network=none', '--iidfile', output,
                     context], env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--environment', type=Path, required=True)
    parser.add_argument('--base', required=True)
    parser.add_argument('--iidfile', type=Path, required=True)
    args = parser.parse_args()
    layer(json.loads(args.environment.read_text()), args.base, args.iidfile.absolute(), dict(os.environ))


if __name__ == '__main__':
    main()

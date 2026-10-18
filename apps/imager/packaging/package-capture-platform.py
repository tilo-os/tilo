#!/usr/bin/env python3
"""Package the qualified Linux host and its canonical root-owned helper paths."""
import argparse
import os
from pathlib import Path
import shutil
import tarfile
import tempfile

from importlib.util import spec_from_file_location, module_from_spec

HERE = Path(__file__).resolve().parent
spec = spec_from_file_location('capture_release', HERE / 'prepare-capture-release.py')
release = module_from_spec(spec)
spec.loader.exec_module(release)


def package(output, host, env):
    qualifier = release.load_qualifier()
    cohort = qualifier.admit_cohort(Path(env['TILO_CAPTURE_QUALIFICATION_COHORT']), host, True)
    with tempfile.TemporaryDirectory(prefix='tilo-capture-platform-') as scratch:
        root = Path(scratch)
        target = root / 'usr/bin/tilo-imager'
        target.parent.mkdir(parents=True)
        shutil.copyfile(host, target)
        target.chmod(0o755)
        enrollment = target.with_name('tilo-enroll')
        shutil.copyfile(host.with_name('tilo-enroll'), enrollment)
        enrollment.chmod(0o755)
        release.run(['bash', HERE / 'stage-capture-sidecar.sh', host, root / 'usr', target], env)
        # Native tar/AppImage/Flatpak launchers still need the helper installed
        # on the host. Never elevate an executable from their mutable bundle.
        for role in ('prepared_helper', 'credential_helper', 'home_seeds', 'prepared_extract'):
            row = cohort['roles'][role]
            destination = root / qualifier.BINDINGS[role].lstrip('/')
            qualifier.bound_file(row['path'], row['sha256'])
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(row['path'], destination)
            destination.chmod(0o755)
            qualifier.bound_file(destination, row['sha256'])
        def ownership(member):
            member.uid = member.gid = 0
            member.uname = member.gname = 'root'
            return member
        with tarfile.open(output, 'x:gz') as archive:
            archive.add(root / 'usr', arcname='usr', filter=ownership)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--host', type=Path, required=True)
    args = parser.parse_args()
    package(args.output, args.host, dict(os.environ))


if __name__ == '__main__':
    main()

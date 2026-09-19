#!/usr/bin/env python3
"""Compare the production trust-input copy with its previous filesystem contract.

Requires rootful local Podman and an already-present image with /bin/sh, mkdir,
cp and rm. It never pulls, exports a registry cache, or builds OS components.
"""
import argparse
import hashlib
from pathlib import Path
import shlex
import shutil
import stat
import subprocess
import tempfile


LEGACY = """COPY keys/cosign.pub keys/tpm2-pcr-signing.public.pem /run/tilo-official-trust/
COPY os/dev/development-trust.py os/dev/install-public-trust.py os/dev/hardware_profile.py /run/tilo-trust-tools/
COPY os/etc/containers/policy.json /etc/containers/policy.json
COPY os/etc/containers/registries.d/code.podesta.ai.yaml /etc/containers/registries.d/code.podesta.ai.yaml
"""
DESTINATIONS = (
    'run/tilo-official-trust', 'run/tilo-trust-tools', 'etc/containers',
)


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def manifest(root):
    result = {}
    for name in ('run', 'etc', *DESTINATIONS):
        path = root / name
        paths = [path, *path.rglob('*')] if name in DESTINATIONS else [path]
        for entry in paths:
            info = entry.lstat()
            result[str(entry.relative_to(root))] = (
                stat.S_IFMT(info.st_mode), stat.S_IMODE(info.st_mode),
                info.st_uid, info.st_gid,
                entry.readlink().as_posix() if entry.is_symlink() else
                hashlib.sha256(entry.read_bytes()).hexdigest() if entry.is_file() else None,
            )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tools-image', required=True, help='an already-present image ID')
    args = parser.parse_args()
    # Resolve before writing recipes: no mutable registry lookup or shell expansion.
    image = command('podman', 'image', 'inspect', '--format', '{{.Id}}', args.tools_image)
    repo = Path(__file__).resolve().parents[2]
    source = (repo / 'os/Containerfile').read_text(encoding='utf-8')
    begin = source.index('COPY keys/cosign.pub keys/tpm2-pcr-signing.public.pem \\\n')
    end = source.index('\nRUN ', begin)
    grouped_copy = source[begin:end]
    run_start = source.index('    mkdir -p /run/tilo-official-trust ', end)
    run_end = source.index(' && PATH=', run_start)
    preparation = source[run_start:run_end].rstrip().removesuffix('\\').rstrip()
    recipes = {'legacy': LEGACY, 'grouped': grouped_copy + '\nRUN ' + preparation +
               ' && rm -rf /run/tilo-trust-inputs\n'}
    with tempfile.TemporaryDirectory(prefix='tilo-trust-copy-') as temporary:
        context = Path(temporary)
        for line in LEGACY.splitlines():
            for name in shlex.split(line)[1:-1]:
                target = context / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(repo / name, target)
                # Exercise executable as well as data-file modes independently of
                # the checkout filesystem (Windows mounts need not preserve them).
                target.chmod(0o755 if name.endswith('.py') else 0o644)
        base = context / 'base'
        for name in ('run', 'etc', *DESTINATIONS, 'etc/containers/registries.d'):
            (base / name).mkdir(parents=True, exist_ok=True)
            (base / name).chmod(0o711)
        for name in DESTINATIONS:
            (base / name / 'unrelated-sentinel').write_bytes(b'preserve existing content\n')
        # Both replacing a file and creating absent files must retain the same result.
        (base / 'etc/containers/policy.json').write_text('old policy\n')
        (base / 'etc/containers/policy.json').chmod(0o600)
        results = {}
        for name, recipe in recipes.items():
            (context / 'Containerfile').write_text(
                f'FROM {image}\nCOPY base/ /\n' + recipe, encoding='utf-8')
            subprocess.run(['podman', 'build', '--pull=never', '--network=none',
                            '--no-cache', '--layers=false', '--memory=512m', '--timestamp=1789781336',
                            '--iidfile', str(context / 'image-id'), '-f',
                            str(context / 'Containerfile'), str(context)], check=True)
            built = (context / 'image-id').read_text().strip()
            try:
                mounted = Path(command('podman', 'image', 'mount', built))
                try:
                    results[name] = manifest(mounted)
                    assert not (mounted / 'run/tilo-trust-inputs').exists()
                finally:
                    command('podman', 'image', 'unmount', built)
            finally:
                command('podman', 'image', 'rm', built)
        differences = {key: (results['legacy'].get(key), results['grouped'].get(key))
                       for key in results['legacy'].keys() | results['grouped'].keys()
                       if results['legacy'].get(key) != results['grouped'].get(key)}
        assert not differences, differences
        print('PASS: production grouped copy preserves file bytes, modes, owners, '
              'existing directories and unrelated content; temporary inputs removed')


if __name__ == '__main__':
    main()

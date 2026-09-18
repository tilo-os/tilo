#!/usr/bin/env python3
"""Native real cpio/gzip/squashfs proof; no mounts or extracted image filesystem."""
import base64
import gzip
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

SPEC = importlib.util.spec_from_file_location('initramfs', Path(__file__).resolve().parents[1] / 'dev/initramfs-trust.py')
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)


def main():
    if os.environ.get('TILO_DISPOSABLE_NATIVE_FIXTURE') != '1':
        raise ValueError('explicit disposable native fixture required')
    for tool in ('cpio', 'gzip', 'zstd', 'mksquashfs', 'unsquashfs'):
        if not shutil.which(tool):
            raise ValueError('required native archive tooling unavailable')
    key = (b'-----BEGIN PUBLIC KEY-----\n' +
           base64.b64encode(bytes.fromhex('302a300506032b6570032100') + bytes(range(32))) +
           b'\n-----END PUBLIC KEY-----\n')
    with tempfile.TemporaryDirectory(prefix='tilo-initramfs-native-', dir=os.environ.get('TMPDIR')) as temporary:
        root = Path(temporary)
        def archive(name, files):
            context = root / name
            context.mkdir()
            for relative, data in files.items():
                path = context / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            names = sorted(str(path.relative_to(context)) for path in context.rglob('*'))
            return subprocess.run(['/usr/bin/cpio', '--create', '--format=newc', '--quiet', '--null'],
                cwd=context, input=('\0'.join(names) + '\0').encode(), stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, check=True, timeout=20).stdout
        early = archive('early', {'early_cpio': b'1', 'kernel/x86/microcode/fixture.bin': b'fixture'})
        plain = archive('plain', {verifier.KEY: key})
        def squash_archive(name, relative, contents, redirect=None):
            squash_root = root / (name + '-source')
            path = squash_root / relative
            path.parent.mkdir(parents=True)
            path.write_bytes(contents)
            if redirect == 'key':
                path.rename(path.with_name('actual-public.pem'))
                path.symlink_to('actual-public.pem')
            elif redirect == 'parent':
                parent = path.parent
                parent.rename(parent.with_name('actual-tilo'))
                parent.symlink_to('actual-tilo', target_is_directory=True)
            squash = root / (name + '.squashfs')
            subprocess.run([shutil.which('mksquashfs'), str(squash_root), str(squash),
                            '-noappend', '-processors', '1', '-comp', 'gzip', '-no-progress'],
                           check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
            return archive(name, {'squashfs-root.img': squash.read_bytes()})
        wrapped = squash_archive('squash', verifier.KEY, key)
        stale_squash = squash_archive('stale-squash', verifier.KEY, b'old public key')
        missing_squash = squash_archive('missing-squash', 'unrelated', b'public')
        linked_key = squash_archive('linked-key', verifier.KEY, key, 'key')
        linked_parent = squash_archive('linked-parent', verifier.KEY, key, 'parent')
        zstd_main = subprocess.run(['/usr/bin/zstd', '-q', '-c'], input=plain, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, check=True, timeout=20).stdout
        stale = archive('stale', {verifier.KEY: b'old public key'})
        missing = archive('missing', {'unrelated': b'public'})
        corrupted = bytearray(gzip.compress(plain))
        corrupted[-5] ^= 1
        cases = [('plain', plain, True), ('early-gzip', early + gzip.compress(plain), True),
                 ('early-gzip-squashfs', early + gzip.compress(wrapped), True),
                 ('early-zstd', early + zstd_main, True),
                 ('squash-key-symlink', gzip.compress(linked_key), False),
                 ('squash-parent-symlink', gzip.compress(linked_parent), False),
                 ('truncated-zstd', zstd_main[:-3], False),
                 ('missing', gzip.compress(missing), False), ('stale', gzip.compress(stale), False),
                 ('duplicate', plain + gzip.compress(plain), False),
                 ('duplicate-squash', plain + gzip.compress(wrapped), False),
                 ('stale-squash', gzip.compress(stale_squash), False),
                 ('missing-squash', gzip.compress(missing_squash), False),
                 ('corrupt', bytes(corrupted), False), ('truncated', gzip.compress(plain)[:-4], False)]
        results = []
        for name, data, accepted in cases:
            image = root / (name + '.img')
            image.write_bytes(data)
            try:
                proof = verifier.verify(image, key, scratch_parent=root)
            except verifier.VerificationError:
                if accepted:
                    raise
                results.append({'case': name, 'rejected': True})
            else:
                if not accepted:
                    raise ValueError('invalid fixture unexpectedly accepted')
                results.append({'case': name, 'proof': proof})
        packages = subprocess.run(['/usr/bin/rpm', '-q', 'cpio', 'squashfs-tools', 'gzip', 'xz', 'zstd'],
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10).stdout.decode().splitlines()
        print(json.dumps({'schema': 1, 'status': 'passed', 'tool_packages': packages, 'cases': results}, sort_keys=True))


if __name__ == '__main__':
    main()

#!/usr/bin/env python3
"""Build the matching upstream Zero VM without granting executable writable pages.

The existing checksum-pinned JRE supplies its matching Java libraries. Only the
VM is rebuilt from that JRE's recorded upstream commit, with Zero's C++ interpreter
instead of HotSpot's generated interpreter/JIT. No signing or policy is involved.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('zero_staging', ROOT / 'os/scripts/stage-signal-runtime.py')
staging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(staging)
require = staging.require
BOOT_JDK = Path('/usr/lib/jvm/java-25-openjdk')
RESERVER = 'src/hotspot/share/memory/memoryReserver.cpp'
ORIGINAL_RESERVATION = '                                 ExecMem,\n                                 mtCode);'
ZERO_RESERVATION = '''#ifdef ZERO
                                 // Zero stores interpreter entry pointers, not generated code.
                                 !ExecMem,
#else
                                 ExecMem,
#endif
                                 mtCode);'''


def patch_zero_cache(tree, payload):
    source = tree / RESERVER
    original = source.read_text()
    require(original.count(ORIGINAL_RESERVATION) == 1, 'Zero cache source differs')
    modified = original.replace(ORIGINAL_RESERVATION, ZERO_RESERVATION)
    source.write_text(modified)
    # Ship the exact corresponding modified GPL source with the VM, rather than
    # relying on a mutable branch URL to explain this data-only reservation.
    output = payload / 'java-zero-source' / RESERVER
    output.parent.mkdir(parents=True)
    output.write_text(modified)
    return hashlib.sha256(modified.encode()).hexdigest()


def build(work, payload, assets, pin):
    source = pin['java_zero_source']
    require(len(source['commit']) == 40 and all(c in '0123456789abcdef' for c in source['commit']),
            'exact Zero source commit required')
    upstream = (payload / 'java/release').read_text()
    require('SOURCE=".:git:' + source['commit'][:12] + '"' in upstream,
            'Zero source differs from the pinned JRE source')
    require('JAVA_VERSION="' + pin['java_version'] + '"' in upstream,
            'Zero JRE version differs')
    archive = staging.verified_asset(assets, source)
    require(not work.exists(), 'Zero build output already exists')
    boot = BOOT_JDK
    require((boot / 'bin/javac').is_file(), 'existing Java 25 build compiler required')
    work.mkdir(parents=True)
    # Larger bounds apply only to the immutable, verified compiler source.
    # Ordinary runtime archives keep their existing smaller defaults.
    tree = staging.unpack(archive, source['root'], work / 'source',
                          max_files=100000, max_bytes=2 * 1024 * 1024 * 1024)
    modification = patch_zero_cache(tree, payload)
    configure = ['bash', 'configure', '--with-jvm-variants=zero',
                 '--enable-headless-only',
                 '--with-boot-jdk=' + str(boot), '--with-debug-level=release',
                 '--with-native-debug-symbols=none', '--disable-warnings-as-errors',
                 '--with-jobs=2', '--with-version-build=1', '--with-version-opt=TiloZero',
                 '--with-version-pre=', '--with-source-date=' + str(source['source_date_epoch']),
                 '--with-vendor-name=Tilo', '--with-vendor-version-string=TiloZero-' + pin['java_version']]
    subprocess.run(configure, cwd=tree, check=True)
    subprocess.run(['make', 'hotspot', 'JOBS=2'], cwd=tree, check=True)
    libraries = list(tree.glob('build/*/jdk/lib/zero/libjvm.so'))
    require(len(libraries) == 1, 'Zero build must produce exactly one VM')
    library = libraries[0]
    staging.require_x86_elf(library)
    require(b'Zero VM' in library.read_bytes(), 'compiled VM is not Zero')
    with library.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    shutil.copy2(library, payload / 'java/lib/server/libjvm.so')
    # HotSpot's prebuilt shared class archives cannot be used by this Zero VM.
    # Let Zero load the pinned classes normally, without a vendor archive warning.
    for archive_name in ('classes.jsa', 'classes_nocoops.jsa'):
        (payload / 'java/lib/server' / archive_name).unlink(missing_ok=True)
    (payload / 'java/upstream-release').write_text(upstream)
    # Retain the untouched vendor metadata separately; do not claim the new
    # composite runtime is the vendor's original HotSpot distribution.
    fields = dict(line.split('=', 1) for line in upstream.splitlines() if '=' in line)
    version = pin['java_version'] + '+1-TiloZero'
    fields.update(IMPLEMENTOR='"Tilo"', IMPLEMENTOR_VERSION='"TiloZero-' + pin['java_version'] + '"',
                  JVM_VARIANT='"Zero"', JVM_VERSION='"' + version + '"',
                  JAVA_RUNTIME_VERSION='"' + version + '"', FULL_VERSION='"' + version + '"',
                  SEMANTIC_VERSION='"' + version + '"',
                  BUILD_SOURCE='"tools/apps/signal_zero_runtime.py"',
                  BUILD_SOURCE_REPO='"https://code.tilo.so/tilo/tilo"')
    fields.pop('BUILD_INFO', None)
    (payload / 'java/release').write_text(''.join(key + '=' + value + '\n' for key, value in fields.items()))
    proof = {'source': source, 'java_version': pin['java_version'],
             'variant': 'zero', 'vm_sha256': digest, 'configure': configure[2:],
             'data_only_cache_source_sha256': modification}
    (payload / 'java-zero.json').write_text(json.dumps(proof, sort_keys=True) + '\n')
    verify(payload, pin)


def verify(payload, pin):
    require(not any((payload / 'java/lib/server' / name).exists()
                    for name in ('classes.jsa', 'classes_nocoops.jsa')),
            'incompatible HotSpot class archive retained')
    proof = json.loads((payload / 'java-zero.json').read_text())
    require(proof['source'] == pin['java_zero_source'] and proof['variant'] == 'zero'
            and proof['java_version'] == pin['java_version'], 'Zero runtime provenance differs')
    with (payload / 'java/lib/server/libjvm.so').open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    require(proof['vm_sha256'] == digest, 'compiled Zero VM bytes changed')
    modified = (payload / 'java-zero-source' / RESERVER).read_text()
    require(proof['data_only_cache_source_sha256'] == hashlib.sha256(modified.encode()).hexdigest()
            and modified.count(ZERO_RESERVATION) == 1, 'Zero data-only cache source differs')
    require('JVM_VARIANT="Zero"' in (payload / 'java/release').read_text(),
            'Zero runtime declaration differs')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--payload', type=Path, required=True)
    parser.add_argument('--assets', type=Path, required=True)
    args = parser.parse_args()
    build(args.work, args.payload, args.assets, json.loads(staging.PIN.read_text()))


if __name__ == '__main__':
    main()

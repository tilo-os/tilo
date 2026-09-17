#!/usr/bin/env python3
"""Two-phase native Wine producer using Mock's supported hermetic lifecycle.

Run resolve in a disposable networked container and build in a NEW container
from the same digest, with --network=none, separate empty /var/lib/mock and
the resolved bundle mounted read-only. This exports RPMs, never a prepared OCI
buildroot. The runtime image digest is provenance, not WINE_BUILDROOT_REF.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

import wine_rpm_contract as contract

MOCK_COMMIT = '3b308660a12fb59886f438a59561f023d01586d8'
PROVENANCE_PACKAGES = ('gcc', 'gcc-c++', 'rpm-build')


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def inventory(directory):
    result = {}
    for path in sorted(Path(directory).rglob('*')):
        if path.is_symlink():
            raise ValueError(f'symlink in retained inputs: {path}')
        if path.is_file():
            result[path.relative_to(directory).as_posix()] = {
                'sha256': digest(path), 'size': path.stat().st_size}
        elif not path.is_dir():
            raise ValueError(f'nonregular retained input: {path}')
    return result


def write_json(path, data):
    Path(path).write_text(json.dumps(data, sort_keys=True, indent=2) + '\n')


def run(argv, log):
    # Retain exact diagnostic bytes and expose progress during long CI builds.
    with Path(log).open('wb') as output:
        output.write((json.dumps(argv) + '\n').encode())
        output.flush()
        with subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT) as process:
            while chunk := process.stdout.read1(65536):
                output.write(chunk)
                output.flush()
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
            returncode = process.wait()
        if returncode:
            raise subprocess.CalledProcessError(returncode, argv)


def verify_i686_execution(directory):
    """Prove IA32 execution and glibc's legacy futex ABI without target libc."""
    source = directory / 'i686-execution.S'
    binary = directory / 'i686-execution'
    # i386 syscall 240 uses futex_time32 even for an untimed wake. A kernel
    # with IA32_EMULATION but no COMPAT_32BIT_TIME can run an exit-only probe
    # while glibc aborts during the first RPM scriptlet. Neither call blocks:
    # wake has no waiters; wait compares 1 with a word containing 0.
    source.write_text(''' .section .text
.global _start
_start:
    push $0
    mov %esp, %ebx
    mov $129, %ecx
    mov $1, %edx
    xor %esi, %esi
    xor %edi, %edi
    xor %ebp, %ebp
    mov $240, %eax
    int $0x80
    test %eax, %eax
    jne failed
    mov $128, %ecx
    mov $240, %eax
    int $0x80
    cmp $-11, %eax
    jne failed
    mov $1, %eax
    xor %ebx, %ebx
    int $0x80
failed:
    mov $1, %eax
    mov $1, %ebx
    int $0x80
.section .note.GNU-stack,"",@progbits
''')
    run(['gcc', '-m32', '-nostdlib', '-static', '-Wl,--build-id=none',
         str(source), '-o', str(binary)], directory / 'i686-compile.log')
    contract.elf_identity(binary.read_bytes()[:20], 'i686')
    try:
        run([str(binary)], directory / 'i686-execution.log')
    except (OSError, subprocess.CalledProcessError) as error:
        with (directory / 'i686-execution.log').open('a') as output:
            output.write(str(error) + '\n')
        raise ValueError('runner cannot execute the static i686 probe; native '
                         'IA32 execution is required, including the legacy futex ABI '
                         '(CONFIG_COMPAT_32BIT_TIME), before resolving Wine dependencies') from error


def flags(target, pin, jobs):
    contract.target(target)
    if jobs <= 0:
        raise ValueError('jobs must be positive')
    return ['--target', target, '--without', 'new_wow64', '--define',
            'dist ' + contract.build_identity(pin)['dist'], '--define',
            f'_smp_mflags -j{jobs}']


def validate_lock(lock, target):
    if lock.get('version') != '1.1.0':
        raise ValueError('unexpected pinned Mock lock schema')
    config = lock.get('config', {})
    if config.get('target_arch') != target or config.get('bootstrap_image_ready') is not True:
        raise ValueError('lock target or ready bootstrap does not match')
    if not lock.get('bootstrap', {}).get('image_digest'):
        raise ValueError('lock lacks bootstrap image digest')
    if not lock.get('buildroot', {}).get('rpms'):
        raise ValueError('lock lacks resolved RPM inventory')
    native_packages = {package.get('name') for package in lock['buildroot']['rpms']
                       if package.get('arch') == target}
    missing = set(PROVENANCE_PACKAGES) - native_packages
    if missing:
        raise ValueError('lock lacks native provenance tools: ' + ', '.join(sorted(missing)))


def prepare_mock_config(source, destination, name):
    """Resolve provenance tools together with Wine's dependencies, before freezing."""
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', name):
        raise ValueError('invalid Mock configuration name')
    selected = source / (name + '.cfg')
    if not selected.is_file():
        raise ValueError('selected Mock configuration does not exist')
    shutil.copytree(source, destination, symlinks=False)
    # Mock's --additional-package only supports rebuild, not calculatedeps.
    # more_buildreqs is honored during dependency resolution; hermetic Mock
    # later installs every RPM from the resulting lock/repository offline.
    with (destination / selected.name).open('a') as config:
        config.write("\n# Tilo's post-build compiler provenance requirements.\n"
                     "config_opts['more_buildreqs']['wine'] = "
                     "list(config_opts['more_buildreqs'].get('wine', [])) + "
                     f"{list(PROVENANCE_PACKAGES)!r}\n")


def build_command(bundle, root, result, options, filename):
    return ['mock', '--hermetic-build', str(bundle / 'resolution/buildroot_lock.json'),
            str(bundle / 'repository'), '--rootdir', str(root), '--resultdir', str(result),
            '--no-cleanup-after', *options, str(bundle / 'build-source' / filename)]


def prepare_source(source, destination, pin, options):
    """Retain authenticated input and the exact spec used for dependency resolution."""
    destination.mkdir()
    with tempfile.TemporaryDirectory(prefix='tilo-wine-source-') as temporary:
        top = Path(temporary)
        run(['rpm', '-i', '--define', f'_topdir {top}', str(source / pin['filename'])],
            destination / 'extract.log')
        spec = top / 'SPECS/wine.spec'
        contract.verify_file(spec, pin['spec_sha256'])
        shutil.copyfile(spec, destination / 'upstream.spec')
        spec.unlink()
        contract.prepare_spec(destination / 'upstream.spec', spec, pin)
        shutil.copyfile(spec, destination / 'wine.spec')
        run(['rpmbuild', '-bs', str(spec), '--define', f'_topdir {top}', *options],
            destination / 'source-build.log')
        filename = contract.build_identity(pin)['source_rpm']
        shutil.copyfile(top / 'SRPMS' / filename, destination / filename)
    write_json(destination / 'derivation.json', {
        'schema': 1, 'correction': 'wine11-twain-32-i386-file-list',
        'upstream_srpm_sha256': pin['sha256'], 'upstream_spec_sha256': pin['spec_sha256'],
        'build_spec_sha256': pin['build_spec_sha256'], 'source_rpm': filename,
        'source_rpm_sha256': digest(destination / filename)})
    contract.verify_prepared_source(destination, pin)


def offline_network(interfaces=Path('/sys/class/net')):
    names = {path.name for path in interfaces.iterdir()}
    if names - {'lo'}:
        raise ValueError('offline phase requires a network namespace with only loopback')


def tool_record(runtime_image):
    contract.reference(runtime_image)
    tools = {}
    for name in ('mock', 'mock-hermetic-repo', 'rpm', 'podman'):
        path = shutil.which(name)
        if not path:
            raise ValueError(f'missing producer tool: {name}')
        tools[name] = {'path': path, 'sha256': digest(path)}
    return {'runtime_image': runtime_image,
            'mock_api_reference_commit': MOCK_COMMIT,
            'mock_version': contract.run(['mock', '--version']),
            'host_rpms': contract.run(['rpm', '-qa', '--qf',
                '%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\n']).splitlines(),
            'tools': tools}


def resolve(args, pin):
    destination = args.output.resolve()
    destination.mkdir(parents=True, exist_ok=False)
    if args.target == 'i686':
        verify_i686_execution(destination)
    contract.source(args.source, pin)
    shutil.copytree(args.source, destination / 'source')
    prepare_mock_config(args.config_dir, destination / 'mock-config', args.config)
    write_json(destination / 'wine-source.json', pin)
    options = flags(args.target, pin, args.jobs)
    prepare_source(destination / 'source', destination / 'build-source', pin, options)
    result = destination / 'resolution'
    result.mkdir()
    os.chown(result, int(os.environ['SUDO_UID']), int(os.environ['SUDO_GID']))
    command = ['mock', '--configdir', str(destination / 'mock-config'), '-r', args.config,
               '--resultdir', str(result), *options]
    run([*command, '--debug-config-expanded'], destination / 'resolved-config.log')
    run([*command, '--calculate-build-dependencies',
         str(destination / 'build-source' / contract.build_identity(pin)['source_rpm'])],
        destination / 'resolve.log')
    lockfile = result / 'buildroot_lock.json'
    validate_lock(json.loads(lockfile.read_text()), args.target)
    run(['mock-hermetic-repo', '--lockfile', str(lockfile), '--output-repo',
         str(destination / 'repository')], destination / 'repository.log')
    record = {'schema': 1, 'mode': 'mock-hermetic', 'target': args.target,
              'source': pin, 'options': options, 'config': args.config,
              'tools': tool_record(args.runtime_image),
              'files': inventory(destination)}
    write_json(destination / 'manifest.json', record)


def verify_bundle(bundle, runtime_image, target, pin, jobs):
    record = json.loads((bundle / 'manifest.json').read_text())
    actual = inventory(bundle)
    actual.pop('manifest.json')
    if actual != record['files']:
        raise ValueError('resolved bundle changed')
    if (record.get('schema'), record.get('mode'), record.get('target'), record.get('source'),
        record.get('options')) != (1, 'mock-hermetic', target, pin, flags(target, pin, jobs)):
        raise ValueError('source/target/build options differ from resolution')
    current = tool_record(runtime_image)
    # Package order is not identity; preserve original order in evidence.
    current_comparable = dict(current, host_rpms=sorted(current['host_rpms']))
    recorded_comparable = dict(record['tools'], host_rpms=sorted(record['tools']['host_rpms']))
    if current_comparable != recorded_comparable:
        raise ValueError('offline tool environment differs from resolution')
    validate_lock(json.loads((bundle / 'resolution/buildroot_lock.json').read_text()), target)
    return record


def build(args, pin):
    offline_network()
    bundle = args.bundle.resolve()
    record = verify_bundle(bundle, args.runtime_image, args.target, pin, args.jobs)
    contract.source(bundle / 'source', pin)
    contract.verify_prepared_source(bundle / 'build-source', pin)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    root = args.root.resolve()
    if root.exists():
        raise ValueError('offline root must not exist; use a fresh container/root')
    result = output / 'rpms-and-logs'
    result.mkdir()
    os.chown(result, int(os.environ['SUDO_UID']), int(os.environ['SUDO_GID']))
    lockfile = bundle / 'resolution/buildroot_lock.json'
    command = build_command(bundle, root, result, record['options'],
                            contract.build_identity(pin)['source_rpm'])
    run(command, output / 'hermetic-build.log')
    # Direct read-only RPM queries avoid a second Mock --chroot invocation,
    # which initializes a root and cannot be combined with --hermetic-build.
    run(['chroot', str(root), 'rpm', '-qa', '--qf',
         '%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\n'],
        output / 'installed-rpms.log')
    for tool in ('gcc', 'g++', 'rpmbuild'):
        run(['chroot', str(root), tool, '--version'], output / f'{tool}-version.log')
    optflags = shlex.split(contract.run(['chroot', str(root), 'rpmspec', '--target',
                                         args.target, '--eval', '%{optflags}']))
    compiler_evidence = {}
    with tempfile.TemporaryDirectory(prefix='tilo-compiler-', dir=root / 'tmp') as temporary:
        for compiler, language in (('gcc', 'c'), ('g++', 'c++')):
            binary = Path(temporary) / compiler
            command = ['chroot', str(root), compiler,
                       '-m32' if args.target == 'i686' else '-m64', *optflags,
                       '-x', language, '-', '-o', '/' + binary.relative_to(root).as_posix()]
            subprocess.run(command, input='int main(void) { return 0; }\n', text=True, check=True)
            elf_class, machine = contract.elf_identity(binary.read_bytes()[:20], args.target)
            compiler_evidence[compiler] = {'elf_class': elf_class, 'elf_machine': machine,
                                           'rpm_optflags': optflags}
    write_json(output / 'compiler-probes.json', compiler_evidence)
    specs = list(root.glob('builddir/build/SPECS/wine.spec'))
    if len(specs) != 1:
        raise ValueError('Mock did not retain the Wine spec for post-build verification')
    contract.verify_file(specs[0], pin['build_spec_sha256'])
    spec = '/' + specs[0].relative_to(root).as_posix()
    run(['chroot', str(root), 'rpmspec', '--target', args.target, '--without', 'new_wow64',
         '--define', 'dist ' + pin['build_dist'], '-q', '--buildrequires', spec],
        output / 'buildrequires.log')
    rebuilt_source = result / contract.build_identity(pin)['source_rpm']
    if rebuilt_source.is_symlink() or not rebuilt_source.is_file():
        raise ValueError('Mock did not retain the rebuilt Tilo source RPM')
    # The input transfer must remain identical throughout the offline phase.
    verify_bundle(bundle, args.runtime_image, args.target, pin, args.jobs)
    write_json(output / 'producer.json', {
        'schema': 1, 'mode': 'mock-hermetic', 'target': args.target,
        'source': pin, 'build_identity': contract.build_identity(pin),
        'resolution_manifest_sha256': digest(bundle / 'manifest.json'),
        'lockfile_sha256': digest(lockfile), 'tools': record['tools'],
        'options': record['options'], 'network_interfaces': ['lo'],
        'files': inventory(output), 'execution_validated': False})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=('resolve', 'build'))
    parser.add_argument('--target', required=True, choices=('x86_64', 'i686'))
    parser.add_argument('--runtime-image', required=True)
    parser.add_argument('--pin', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--jobs', type=int, default=2)
    parser.add_argument('--source', type=Path)
    parser.add_argument('--config')
    parser.add_argument('--config-dir', type=Path, default=Path('/etc/mock'))
    parser.add_argument('--bundle', type=Path)
    parser.add_argument('--root', type=Path, default=Path('/var/lib/mock/tilo-wine-offline/root'))
    args = parser.parse_args()
    if platform.machine() != 'x86_64' or platform.system() != 'Linux':
        parser.error('native x86-64 Linux producer required')
    if os.geteuid() != 0 or not os.environ.get('SUDO_UID') or int(os.environ['SUDO_UID']) == 0:
        parser.error('invoke using sudo from an unprivileged member of the mock group')
    if args.phase == 'resolve' and (not args.source or not args.config):
        parser.error('resolve requires --source and --config')
    if args.phase == 'build' and not args.bundle:
        parser.error('build requires --bundle')
    pin = json.loads(args.pin.read_text())
    os.environ['SOURCE_DATE_EPOCH'] = str(contract.build_identity(pin)['source_date_epoch'])
    (resolve if args.phase == 'resolve' else build)(args, pin)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error

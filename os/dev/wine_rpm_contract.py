#!/usr/bin/env python3
"""Fail-closed input/buildroot/RPM checks for Fedora's traditional Wine mode.

These checks establish packaging identity and loader layout, not execution
support. Actual same-prefix cross-width acceptance remains a separate gate.
"""
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile


def run(argv):
    return subprocess.check_output(argv, text=True).rstrip('\n')


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def reference(value):
    if not re.fullmatch(r'[^\s@]+@sha256:[0-9a-f]{64}', value):
        raise ValueError('buildroot must be an immutable OCI digest reference')
    return value


def target(value):
    if value not in ('x86_64', 'i686'):
        raise ValueError('unsupported Wine RPM target')
    return value


def build_identity(pin):
    if not re.fullmatch(r'\.fc44\.qos_traditional[1-9][0-9]*', pin['build_dist']):
        raise ValueError('invalid traditional Wine release namespace')
    if pin['build_release'] != pin['release'].removesuffix('.fc44') + pin['build_dist']:
        raise ValueError('build release does not match unchanged spec dist expansion')
    if type(pin['source_date_epoch']) is not int or pin['source_date_epoch'] <= 0:
        raise ValueError('source_date_epoch must be a positive integer')
    return {'dist': pin['build_dist'], 'release': pin['build_release'],
            'source_rpm': f"wine-{pin['version']}-{pin['build_release']}.src.rpm",
            'source_date_epoch': pin['source_date_epoch']}


def verify_file(path, expected, size=None):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise ValueError(f'input must be a regular non-symlink file: {path}')
    if (size is not None and path.stat().st_size != size) or sha(path) != expected:
        raise ValueError(f'input identity mismatch: {path}')


def source(directory, pin):
    directory = Path(directory)
    srpm = directory / pin['filename']
    key = directory / 'fedora-44.asc'
    verify_file(srpm, pin['sha256'], pin['size'])
    verify_file(key, pin['fedora_key_sha256'])
    # A private trust database prevents unrelated buildroot keys authorizing
    # this source. Hash verification precedes RPM parsing and key import.
    with tempfile.TemporaryDirectory(prefix='qos-wine-trust-') as database:
        run(['rpmkeys', '--dbpath', database, '--import', str(key)])
        signature = run(['rpmkeys', '--dbpath', database, '--checksig', '--verbose', str(srpm)])
    if 'Signature' not in signature or 'NOKEY' in signature or 'NOT OK' in signature:
        raise ValueError('source signature is not authenticated')
    return signature


def packages():
    return sorted(run(['rpm', '-qa', '--qf', '%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\n']).splitlines())


def compiler_probe(architecture):
    flags = shlex.split(run(['rpmspec', '--target', architecture, '--eval', '%{optflags}']))
    compiler = ['gcc', '-m32' if architecture == 'i686' else '-m64']
    with tempfile.TemporaryDirectory(prefix='qos-wine-compiler-') as temporary:
        root = Path(temporary)
        (root / 'probe.c').write_text('int main(void) { return 0; }\n')
        subprocess.run(compiler + flags + [str(root / 'probe.c'), '-o', str(root / 'probe')], check=True)
        header = (root / 'probe').read_bytes()[:20]
    expected = elf_identity(header, architecture)
    return {'command': compiler, 'rpm_optflags': flags, 'version': run(['gcc', '--version']),
            'cxx_command': ['g++', compiler[1]], 'cxx_version': run(['g++', '--version']),
            'elf_class': expected[0], 'elf_machine': expected[1]}


def elf_identity(header, architecture):
    expected = (1, 3) if target(architecture) == 'i686' else (2, 62)
    if len(header) < 20 or header[:4] != b'\x7fELF' or header[5] != 1 or (header[4], int.from_bytes(header[18:20], 'little')) != expected:
        raise ValueError('ELF payload does not match the requested architecture')
    return expected


def rpm_loader_identity(path, architecture):
    # --to-stdout writes no archive paths. Header layout validation precedes
    # reading this exact loader; its bytes are inspected, never executed.
    name = './usr/bin/wine64' if architecture == 'x86_64' else './usr/bin/wine32'
    with subprocess.Popen(['rpm2cpio', str(path)], stdout=subprocess.PIPE) as archive:
        try:
            selected = subprocess.run(['cpio', '--quiet', '-i', '--to-stdout', name],
                                      stdin=archive.stdout, stdout=subprocess.PIPE, check=True)
        finally:
            archive.stdout.close()
        if archive.wait() != 0:
            raise ValueError('cannot read RPM payload')
    elf_class, machine = elf_identity(selected.stdout[:20], architecture)
    return {'elf_class': elf_class, 'elf_machine': machine}


def buildroot(architecture, pin):
    target(architecture)
    release = dict(line.split('=', 1) for line in Path('/etc/os-release').read_text().splitlines() if '=' in line)
    if release.get('ID', '').strip('"') != 'fedora' or release.get('VERSION_ID', '').strip('"') != '44':
        raise ValueError('Fedora 44 buildroot required')
    return {'schema': 1, 'target': architecture, 'source_sha256': pin['sha256'],
            'build_identity': build_identity(pin),
            'without': ['new_wow64'], 'compiler': compiler_probe(architecture),
            'packages': packages(), 'buildrequires_sha256': sha('/usr/lib/qos/wine-buildrequires.txt')}


def validate_core_layout(architecture, files):
    """Inspect RPM header filename/link pairs, without extracting payloads."""
    architecture = target(architecture)
    own, other, base, otherbase = ('x86_64', 'i386', '/usr/lib64/wine', '/usr/lib/wine') if architecture == 'x86_64' else ('i386', 'x86_64', '/usr/lib/wine', '/usr/lib64/wine')
    if any('/wine-wow64/' in name for name in files):
        raise ValueError('new WoW64 layout is not a traditional paired build')
    for required in (f'/usr/bin/wine{64 if architecture == "x86_64" else 32}',
                     f'{base}/{own}-unix/ntdll.so', f'{base}/{own}-unix/wine'):
        if required not in files or files[required]:
            raise ValueError(f'missing regular native loader payload: {required}')
    for kind in ('unix', 'windows'):
        name, destination = f'{base}/{other}-{kind}', f'{otherbase}/{other}-{kind}'
        if files.get(name) != destination:
            raise ValueError(f'missing exact alternate-loader link: {name} -> {destination}')


def output(architecture, pin, buildroot_ref, directory=Path('/wine-output'), write=True):
    inventory, seen = {}, set()
    core_found = False
    for path in sorted((directory / 'rpms').glob('*.rpm')):
        if path.is_symlink() or not path.is_file():
            raise ValueError('nonregular output RPM')
        identity = run(['rpm', '-qp', '--qf', '%{NAME}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\t%{SOURCERPM}\t%{LICENSE}', str(path)]).split('\t')
        if len(identity) != 6:
            raise ValueError('malformed output RPM identity')
        name, version, release, arch, srpm, license_name = identity
        build = build_identity(pin)
        if arch not in (architecture, 'noarch') or (version, release, srpm) != (pin['version'], build['release'], build['source_rpm']):
            raise ValueError('unexpected output RPM architecture/source/version')
        if tuple(identity[:4]) in seen or not license_name:
            raise ValueError('duplicate output identity or absent license metadata')
        seen.add(tuple(identity[:4]))
        files = dict(line.split('\t', 1) for line in run(['rpm', '-qp', '--qf', '[%{FILENAMES}\t%{FILELINKTOS}\n]', str(path)]).splitlines())
        if name == 'wine-core':
            validate_core_layout(architecture, files)
            loader = rpm_loader_identity(path, architecture)
            core_found = True
        inventory[path.name] = {'identity': identity, 'sha256': sha(path), 'size': path.stat().st_size}
        if name == 'wine-core':
            inventory[path.name]['loader'] = loader
    if not core_found:
        raise ValueError('output has no validated wine-core RPM')
    rebuilt_source = directory / 'provenance' / build_identity(pin)['source_rpm']
    if rebuilt_source.is_symlink() or not rebuilt_source.is_file():
        raise ValueError('missing rebuilt source RPM for qOS release identity')
    contract = {'schema': 1, 'target': architecture, 'source': pin,
                'build_identity': build_identity(pin), 'rebuilt_source_sha256': sha(rebuilt_source),
                'buildroot_ref': reference(buildroot_ref), 'without': ['new_wow64'],
                'rpms': inventory, 'execution_validated': False}
    if write:
        (directory / 'provenance/contract.json').write_text(json.dumps(contract, indent=2) + '\n')
    return contract


def main(argv):
    operation, value, pin_file, *rest = argv
    pin = json.loads(Path(pin_file).read_text())
    if operation == 'source':
        print(source(value, pin))
    elif operation == 'spec':
        verify_file(value, pin['spec_sha256'])
    elif operation == 'setting':
        print(build_identity(pin)[value])
    elif operation == 'record-buildroot':
        Path('/usr/lib/qos/wine-buildroot.json').write_text(json.dumps(buildroot(value, pin), indent=2) + '\n')
    elif operation == 'verify-buildroot':
        reference(rest[0])
        if buildroot(value, pin) != json.loads(Path('/usr/lib/qos/wine-buildroot.json').read_text()):
            raise ValueError('buildroot compiler/package/source contract changed')
    elif operation == 'output':
        output(target(value), pin, rest[0])
    else:
        raise ValueError('unknown contract operation')


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error

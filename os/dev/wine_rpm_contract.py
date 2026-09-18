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
import shutil
import subprocess
import sys
import tempfile

TWAIN_FILES = b'%{_libdir}/wine/%{winepedirs}/twain_32.dll\n'
TWAIN_FILES_FIXED = (b'%ifarch %{ix86}\n' + TWAIN_FILES + b'%endif\n'
                     b'%ifarch x86_64\n%if %{with new_wow64}\n'
                     b'%{_libdir}/wine/i386-windows/twain_32.dll\n%endif\n%endif\n')


def prepare_spec(original, destination, pin):
    """Apply the one pinned packaging correction, preserving all source/features."""
    verify_file(original, pin['spec_sha256'])
    data = Path(original).read_bytes()
    if data.count(TWAIN_FILES) != 1:
        raise ValueError('unexpected Wine TWAIN file list')
    derived = data.replace(TWAIN_FILES, TWAIN_FILES_FIXED)
    if hashlib.sha256(derived).hexdigest() != pin['build_spec_sha256']:
        raise ValueError('derived Wine spec identity mismatch')
    with Path(destination).open('xb') as output:
        output.write(derived)


def verify_prepared_source(directory, pin):
    directory = Path(directory)
    verify_file(directory / 'upstream.spec', pin['spec_sha256'])
    verify_file(directory / 'wine.spec', pin['build_spec_sha256'])
    with tempfile.TemporaryDirectory(prefix='tilo-wine-spec-') as temporary:
        prepare_spec(directory / 'upstream.spec', Path(temporary) / 'wine.spec', pin)
    record = json.loads((directory / 'derivation.json').read_text())
    expected = {'schema': 1, 'correction': 'wine11-twain-32-i386-file-list',
                'upstream_srpm_sha256': pin['sha256'],
                'upstream_spec_sha256': pin['spec_sha256'],
                'build_spec_sha256': pin['build_spec_sha256'],
                'source_rpm': build_identity(pin)['source_rpm']}
    if any(record.get(key) != value for key, value in expected.items()):
        raise ValueError('Wine source derivation identity mismatch')
    verify_file(directory / expected['source_rpm'], record['source_rpm_sha256'])
    verify_srpm_spec(directory / expected['source_rpm'], pin['build_spec_sha256'])
    return record


def verify_srpm_spec(path, expected):
    # Read only the spec member; never extract arbitrary RPM paths or execute it.
    with subprocess.Popen(['rpm2cpio', str(path)], stdout=subprocess.PIPE) as archive:
        try:
            selected = subprocess.run(['cpio', '--quiet', '-i', '--to-stdout', './wine.spec'],
                                      stdin=archive.stdout, stdout=subprocess.PIPE, check=True)
        finally:
            archive.stdout.close()
        if archive.wait() != 0:
            raise ValueError('cannot read source RPM spec')
    if hashlib.sha256(selected.stdout).hexdigest() != expected:
        raise ValueError('source RPM spec identity mismatch')


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
    if not re.fullmatch(r'\.fc44\.tilo_traditional[1-9][0-9]*', pin['build_dist']):
        raise ValueError('invalid traditional Wine release namespace')
    if pin['build_release'] != pin['release'].removesuffix('.fc44') + pin['build_dist']:
        raise ValueError('build release does not match spec dist expansion')
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
    with tempfile.TemporaryDirectory(prefix='tilo-wine-trust-') as database:
        run(['rpmkeys', '--dbpath', database, '--import', str(key)])
        signature = run(['rpmkeys', '--dbpath', database, '--checksig', '--verbose', str(srpm)])
    # RPM 6 prints lowercase "signature". A zero exit also occurs for unsigned
    # digest-only packages, so require an indented positive signature result.
    if (not re.search(r'(?m)^[ \t]+[^\r\n]*[Ss]ignature[^\r\n]*: OK$', signature)
            or 'NOKEY' in signature or 'NOT OK' in signature):
        raise ValueError('source signature is not authenticated:\n' + signature)
    return signature


def packages():
    return sorted(run(['rpm', '-qa', '--qf', '%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\n']).splitlines())


def compiler_probe(architecture):
    flags = shlex.split(run(['rpmspec', '--target', architecture, '--eval', '%{optflags}']))
    compiler = ['gcc', '-m32' if architecture == 'i686' else '-m64']
    with tempfile.TemporaryDirectory(prefix='tilo-wine-compiler-') as temporary:
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
            'packages': packages(), 'buildrequires_sha256': sha('/usr/lib/tilo/wine-buildrequires.txt')}


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


def tree_inventory(root):
    result = {}
    for item in sorted(Path(root).rglob('*')):
        if item.is_symlink() or not (item.is_file() or item.is_dir()):
            raise ValueError('nonregular provenance input')
        if item.is_file():
            result[item.relative_to(root).as_posix()] = {'sha256': sha(item), 'size': item.stat().st_size}
    return result


def mock_destination(name, pin):
    if Path(name).is_absolute() or '..' in Path(name).parts or '\\' in name:
        raise ValueError('invalid producer inventory path')
    if name.endswith('.rpm'):
        if len(Path(name).parts) != 2 or Path(name).parts[0] != 'rpms-and-logs':
            raise ValueError('RPM outside Mock result directory')
        if name.endswith('.src.rpm'):
            if Path(name).name != build_identity(pin)['source_rpm']:
                raise ValueError('unexpected rebuilt source RPM')
            return 'provenance/' + Path(name).name
        return 'rpms/' + Path(name).name
    return 'provenance/mock/' + name


def retained_resolution_files(files, pin):
    excluded = {'resolution/buildroot_lock.json', 'source/' + pin['filename'], 'source/fedora-44.asc'}
    return {name: value for name, value in files.items()
            if name not in excluded and not name.startswith(('repository/', 'mock-config/'))}


def validate_mock(directory, architecture, pin):
    directory = Path(directory)
    provenance = directory / 'provenance'
    record = json.loads((provenance / 'mock-producer.json').read_text())
    resolution = json.loads((provenance / 'resolution-manifest.json').read_text())
    lock = json.loads((provenance / 'buildroot_lock.json').read_text())
    for value in (record, resolution):
        if (value.get('schema'), value.get('mode'), value.get('target'), value.get('source')) != (
                1, 'mock-hermetic', architecture, pin):
            raise ValueError('Mock source/target/mode mismatch')
    options = record.get('options', [])
    expected = ['--target', architecture, '--without', 'new_wow64', '--define',
                'dist ' + build_identity(pin)['dist'], '--define']
    if (len(options) != 8 or options[:7] != expected
            or not re.fullmatch(r'_smp_mflags -j[1-9][0-9]*', options[7])
            or options != resolution.get('options')):
        raise ValueError('Mock build options differ')
    if (record.get('build_identity') != build_identity(pin)
            or record.get('network_interfaces') != ['lo']
            or record.get('execution_validated') is not False):
        raise ValueError('Mock producer identity/network evidence mismatch')
    if record.get('tools') != resolution.get('tools'):
        raise ValueError('Mock runtime differs from resolution')
    tools = record['tools']
    reference(tools['runtime_image'])
    if (not tools.get('mock_version') or not tools.get('host_rpms')
            or not re.fullmatch(r'[0-9a-f]{40}', tools.get('mock_api_reference_commit', ''))
            or set(tools.get('tools', {})) != {'mock', 'mock-hermetic-repo', 'rpm', 'podman'}):
        raise ValueError('incomplete Mock runtime identity')
    for item in tools['tools'].values():
        if not item.get('path') or not re.fullmatch(r'[0-9a-f]{64}', item.get('sha256', '')):
            raise ValueError('invalid Mock tool identity')
    if (sha(provenance / 'resolution-manifest.json') != record['resolution_manifest_sha256']
            or sha(provenance / 'buildroot_lock.json') != record['lockfile_sha256']):
        raise ValueError('Mock resolution evidence changed')
    lock_identity = resolution['files'].get('resolution/buildroot_lock.json')
    if lock_identity != {'sha256': record['lockfile_sha256'], 'size': (provenance / 'buildroot_lock.json').stat().st_size}:
        raise ValueError('lock not bound to resolution inventory')
    configs = {name.removeprefix('mock-config/'): identity for name, identity in resolution['files'].items()
               if name.startswith('mock-config/')}
    if not configs or tree_inventory(provenance / 'mock-config') != configs:
        raise ValueError('Mock configuration evidence changed')
    retained = retained_resolution_files(resolution['files'], pin)
    if tree_inventory(provenance / 'resolution') != retained:
        raise ValueError('resolution/source metadata changed')
    verify_prepared_source(provenance / 'resolution/build-source', pin)
    if (lock.get('version') != '1.1.0' or lock.get('config', {}).get('target_arch') != architecture
            or lock.get('config', {}).get('bootstrap_image_ready') is not True
            or not lock.get('bootstrap', {}).get('image_digest') or not lock.get('buildroot', {}).get('rpms')):
        raise ValueError('invalid Mock lock')
    files = record['files']
    if not files or 'producer.json' in files:
        raise ValueError('invalid producer inventory')
    required = {'compiler-probes.json', 'installed-rpms.log', 'buildrequires.log', 'hermetic-build.log',
                'gcc-version.log', 'g++-version.log', 'rpmbuild-version.log',
                'rpms-and-logs/' + build_identity(pin)['source_rpm']}
    if not required <= files.keys():
        raise ValueError('incomplete Mock build evidence')
    for name, identity in files.items():
        item = directory / mock_destination(name, pin)
        verify_file(item, identity['sha256'], identity['size'])
    expected_mock = {name: identity for name, identity in files.items() if not name.endswith('.rpm')}
    if tree_inventory(provenance / 'mock') != expected_mock:
        raise ValueError('Mock log inventory changed')
    probes = json.loads((provenance / 'mock/compiler-probes.json').read_text())
    expected_elf = (1, 3) if architecture == 'i686' else (2, 62)
    if set(probes) != {'gcc', 'g++'} or any(
            (probe.get('elf_class'), probe.get('elf_machine')) != expected_elf
            or not isinstance(probe.get('rpm_optflags'), list) for probe in probes.values()):
        raise ValueError('Mock compiler probe target mismatch')
    expected_rpms = {Path(name).name for name in files if name.endswith('.rpm') and not name.endswith('.src.rpm')}
    if {p.name for p in (directory / 'rpms').iterdir()} != expected_rpms:
        raise ValueError('Mock RPM inventory changed')
    allowed = {mock_destination(name, pin) for name in files}
    allowed.update('provenance/' + name for name in (
        'mock-producer.json', 'resolution-manifest.json', 'buildroot_lock.json',
        'resolution-reference.json', pin['filename'], 'fedora-44.asc'))
    allowed.update('provenance/mock-config/' + name for name in configs)
    allowed.update('provenance/resolution/' + name for name in retained)
    actual_paths = set(tree_inventory(directory))
    actual_paths.discard('provenance/contract.json')
    if actual_paths != allowed:
        raise ValueError('unexpected files in normalized Mock artifact')
    source(provenance, pin)
    for name in (pin['filename'], 'fedora-44.asc'):
        identity = resolution['files'].get('source/' + name)
        if identity != {'sha256': sha(provenance / name), 'size': (provenance / name).stat().st_size}:
            raise ValueError('source input not bound to resolution')
    resolution_ref = json.loads((provenance / 'resolution-reference.json').read_text())
    if resolution_ref.get('basis') != 'caller-supplied OCI reference; bundle bytes checked against manifest':
        raise ValueError('unknown resolution reference basis')
    return {'mode': 'mock-hermetic', 'producer_sha256': sha(provenance / 'mock-producer.json'),
            'runtime_image': record['tools']['runtime_image'],
            'resolution_ref': reference(resolution_ref['reference']),
            'resolution_ref_basis': resolution_ref['basis'],
            'resolution_manifest_sha256': record['resolution_manifest_sha256'],
            'lockfile_sha256': record['lockfile_sha256']}


def import_mock(architecture, pin, raw, bundle, destination, resolution_ref):
    raw, bundle, destination = map(Path, (raw, bundle, destination))
    reference(resolution_ref)
    resolution = json.loads((bundle / 'manifest.json').read_text())
    resolved_files = tree_inventory(bundle)
    resolved_files.pop('manifest.json')
    if resolved_files != resolution['files']:
        raise ValueError('resolution bundle changed')
    record = json.loads((raw / 'producer.json').read_text())
    actual = tree_inventory(raw)
    actual.pop('producer.json')
    if actual != record['files']:
        raise ValueError('raw Mock output changed')
    if destination.exists() or destination.is_symlink():
        raise ValueError('import destination must not exist')
    # Build an unpublished sibling, then validate the same normalized layout the
    # merger will independently verify. No host tool or buildroot is fabricated.
    with tempfile.TemporaryDirectory(prefix='.wine-import-', dir=destination.parent) as temporary:
        stage = Path(temporary) / 'output'
        (stage / 'rpms').mkdir(parents=True)
        for name in actual:
            target_path = stage / mock_destination(name, pin)
            target_path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(raw / name, target_path)
        for src, name in ((raw / 'producer.json', 'mock-producer.json'),
                          (bundle / 'manifest.json', 'resolution-manifest.json'),
                          (bundle / 'resolution/buildroot_lock.json', 'buildroot_lock.json'),
                          (bundle / 'source' / pin['filename'], pin['filename']),
                          (bundle / 'source/fedora-44.asc', 'fedora-44.asc')):
            if src.is_symlink() or not src.is_file():
                raise ValueError('nonregular Mock evidence')
            shutil.copyfile(src, stage / 'provenance' / name)
        shutil.copytree(bundle / 'mock-config', stage / 'provenance/mock-config')
        for name in retained_resolution_files(resolution['files'], pin):
            # tree_inventory(bundle) has already established these are actual
            # regular relative paths, rather than trusting a manifest filename.
            target_path = stage / 'provenance/resolution' / name
            target_path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(bundle / name, target_path)
        (stage / 'provenance/resolution-reference.json').write_text(json.dumps({
            'reference': resolution_ref,
            'basis': 'caller-supplied OCI reference; bundle bytes checked against manifest'}, indent=2) + '\n')
        output(architecture, pin, None, stage, mode='mock-hermetic')
        if destination.exists() or destination.is_symlink():
            raise ValueError('import destination appeared during validation')
        stage.rename(destination)


def output(architecture, pin, buildroot_ref, directory=Path('/wine-output'), write=True, mode=None):
    target(architecture)
    if mode not in (None, 'mock-hermetic'):
        raise ValueError('unsupported provenance mode')
    if mode == 'mock-hermetic' and buildroot_ref is not None:
        raise ValueError('Mock output must not claim an OCI buildroot')
    producer = validate_mock(directory, architecture, pin) if mode else None
    inventory, seen = {}, set()
    core_found = False
    for path in sorted((directory / 'rpms').iterdir()):
        if path.is_symlink() or not path.is_file() or path.suffix != '.rpm':
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
        raise ValueError('missing rebuilt source RPM for Tilo release identity')
    verify_srpm_spec(rebuilt_source, pin['build_spec_sha256'])
    contract = {'schema': 1, 'target': architecture, 'source': pin,
                'build_identity': build_identity(pin), 'rebuilt_source_sha256': sha(rebuilt_source),
                'without': ['new_wow64'],
                'rpms': inventory, 'execution_validated': False}
    if producer:
        contract['producer'] = producer
    else:
        contract['buildroot_ref'] = reference(buildroot_ref)
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
    elif operation == 'prepare-spec':
        prepare_spec(value, rest[0], pin)
    elif operation == 'setting':
        print(build_identity(pin)[value])
    elif operation == 'record-buildroot':
        Path('/usr/lib/tilo/wine-buildroot.json').write_text(json.dumps(buildroot(value, pin), indent=2) + '\n')
    elif operation == 'verify-buildroot':
        reference(rest[0])
        if buildroot(value, pin) != json.loads(Path('/usr/lib/tilo/wine-buildroot.json').read_text()):
            raise ValueError('buildroot compiler/package/source contract changed')
    elif operation == 'output':
        output(target(value), pin, rest[0])
    elif operation == 'import-mock':
        import_mock(target(value), pin, *rest)
    else:
        raise ValueError('unknown contract operation')


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error

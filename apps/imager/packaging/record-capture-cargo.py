#!/usr/bin/env python3
"""Observe a new Cargo invocation and preserve actual capture cohort evidence.

Use as CARGO for the canonical capture producer, or invoke with Cargo arguments.
It never accepts an old compiler stream. Cached artifacts reported by the new
Cargo invocation are valid; source differences fail without relabelling them.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import uuid

ROOT = Path(__file__).resolve().parents[3]
ROLES = {
    'worker': ('apps/imager', 'tilo-home-export', 'bin', False),
    'runtime': ('crates/runtime/runtime-dylib', 'runtime_dylib', 'dylib', False),
    'host': ('apps/imager', 'imager', 'bin', False),
    'imager_tests': ('apps/imager', 'imager', 'lib', True),
    'backend_tests': ('crates/platform/imager-backend', 'imager_backend', 'lib', True),
    'shell': ('crates/session/shell', 'shell', 'bin', False),
    'fs': ('crates/system/fs', 'fs', 'bin', False),
    'credential_helper': ('crates/system/account-custody', 'tilo-account-credential', 'bin', False),
    'prepared_helper': ('apps/imager', 'tilo-prepared-setup', 'bin', False),
    'home_seeds': ('crates/session/installer', 'tilo-firstboot-home-seeds', 'bin', False),
    'staging_handoff': ('crates/system/install-helpers', 'tilo-staging-handoff', 'bin', False),
    'handoff_copy': ('crates/system/install-helpers', 'tilo-install-handoff-copy', 'bin', False),
    'firstboot_files': ('crates/system/firstboot-files', 'tilo-firstboot-files', 'bin', False),
    'prepared_extract': ('apps/imager', 'tilo-installer-prepared-setup-extract', 'bin', False),
    'control': ('crates/session/control-cli', 'tilo-control', 'bin', False),
    'launcher': ('crates/packaging/launcher', 'tilo-launcher', 'bin', False),
    'files': ('apps/file-explorer', 'file-explorer', 'bin', False),
    'writer': ('apps/word-processor', 'word-processor', 'bin', False),
    'browser': ('apps/browser', 'browser', 'bin', False),
    'app_package': ('crates/packaging/tilo-app-package-cli', 'tilo-app-package', 'bin', False),
    'runtime_bundle': ('crates/packaging/runtime-bundle', 'tilo-runtime-bundle', 'bin', False),
    'install': ('crates/packaging/install', 'tilo-install', 'bin', False),
    'installer': ('crates/session/installer', 'tilo-installer', 'bin', False),
}


def require(value, message):
    if not value:
        raise ValueError(message)


def facts(path, maximum=1024 ** 3):
    path = Path(path).absolute()
    for edge in (path, *path.parents):
        require(not edge.is_symlink(), 'redirected build evidence')
    before = path.stat()
    require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= maximum
            and not before.st_mode & 0o022, 'unprotected or unbounded build evidence')
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    after = path.stat()
    identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
    require(identity(before) == identity(after), 'build evidence changed while measured')
    return {'sha256': digest.hexdigest(), 'bytes': after.st_size}


def source_requirements():
    names = {'Cargo.toml', 'Cargo.lock', 'apps/imager/build.rs',
             'apps/imager/capture-service.json', 'apps/imager/desktop-catalog.json',
             'apps/imager/release-catalog.json',
             'crates/platform/imager-backend/src/setup/home_capture/native_tests/signed_export.rs',
             'crates/platform/imager-backend/src/imager_model.rs',
             'crates/platform/imager-backend/Cargo.toml',
             'apps/imager/packaging/qualify-capture-release.py',
             'tools/ci/native-stores.sh', 'tools/ci/native-store-autoclear.py', 'tools/ci/native-store-virtio.py',
             'tools/apps/pack-runtime-package.sh', 'tools/apps/stage_sdk_services.py',
             'scripts/headless-examples/_control.py',
             'scripts/browser/cef-lock.json', 'scripts/browser/build-host.py',
             'tools/apps/browser_runtime_release.py', 'tools/apps/app_package_isa.py'}
    # Full admission also measures non-Rust prerequisites (PAM, publisher roots,
    # fixture executables). Capture its authoritative closure before compilation,
    # rather than discovering missing inputs after the producer has finished.
    spec = importlib.util.spec_from_file_location(
        'capture_source_closure', ROOT / 'apps/imager/packaging/qualify-capture-release.py')
    qualifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(qualifier)
    names.update(qualifier.FIXTURE_FILES)
    names.update(qualifier.PACKAGE_FILES)
    return names


def sources():
    names = source_requirements()
    listed = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], cwd=ROOT)
    for item in listed.split(b'\0'):
        if not item:
            continue
        name = item.decode('utf-8')
        if (name.endswith(('.rs', '.toml', '.ftl')) or name == 'Cargo.lock'
                or name.startswith(('apps/imager/packaging/', 'apps/browser/chromium/', 'os/scripts/', 'os/usr/libexec/'))):
            names.add(name)
    require(1 <= len(names) <= 65536, 'build source inventory exceeds bounds')
    return {name: facts(ROOT / name) for name in sorted(names)}


def write_document(path, value):
    with path.open('x') as output:
        json.dump(value, output, sort_keys=True, indent=2)
        output.write('\n')
    path.chmod(0o600)


def select_artifact(messages, role):
    directory, name, kind, test = ROLES[role]
    matches = [row for row in messages if row.get('reason') == 'compiler-artifact'
               and row.get('target', {}).get('name') == name
               and row['target'].get('kind') == [kind]
               and row.get('profile', {}).get('test') is test
               and row.get('manifest_path') == str(ROOT / directory / 'Cargo.toml')]
    require(len(matches) <= 1, 'ambiguous actual Cargo artifact for ' + role)
    if not matches:
        return None
    row = matches[0]
    if role == 'runtime':
        candidates = [Path(name) for name in row['filenames'] if Path(name).name == 'libruntime_dylib.so']
        require(len(candidates) == 1, 'actual runtime dylib output is ambiguous')
        path = candidates[0]
    else:
        require(row.get('executable'), 'actual Cargo executable is absent')
        path = Path(row['executable'])
    return row, path


def build(arguments):
    require(arguments and arguments[0] in ('build', 'test') and '--locked' in arguments,
            'capture evidence records only locked build/test invocations')
    directory = Path(os.environ['TILO_CAPTURE_BUILD_EVIDENCE']).absolute()
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    require(not directory.is_symlink() and directory.stat().st_uid == os.geteuid()
            and stat.S_IMODE(directory.stat().st_mode) == 0o700, 'build evidence directory must be privately owned')
    invocation = directory / ('cargo-' + uuid.uuid4().hex)
    invocation.mkdir(mode=0o700)
    before = sources()
    write_document(invocation / 'source-before.json', before)
    command = ['cargo', *arguments]
    formats = [arg for arg in arguments if arg.startswith('--message-format')]
    require(not formats or formats == ['--message-format=json-render-diagnostics'],
            'capture build needs the actual Cargo JSON stream')
    if not formats:
        command.append('--message-format=json-render-diagnostics')
    compiler = invocation / 'compiler.jsonl'
    # The actual child stdout is captured unchanged; no reconstructed compiler
    # messages or caller-supplied build statuses enter this evidence.
    with compiler.open('xb') as output:
        completed = subprocess.run(command, cwd=ROOT, stdout=output, check=False)
    compiler.chmod(0o600)
    after = sources()
    write_document(invocation / 'source-after.json', after)
    unchanged = before == after
    status = {'schema': 1, 'command': command, 'exit_code': completed.returncode,
              'listed_source_unchanged': unchanged}
    write_document(invocation / 'build-status.json', status)
    # Preserve the ordinary Cargo stream for callers that already capture it.
    with compiler.open('rb') as captured:
        shutil.copyfileobj(captured, sys.stdout.buffer)
    sys.stdout.buffer.flush()
    require(completed.returncode == 0, 'actual Cargo build failed; retained its failure evidence')
    require(unchanged, 'source changed during actual Cargo build; no cohort roles emitted')
    require(facts(compiler, 32 * 1024 ** 2)['bytes'] <= 32 * 1024 ** 2, 'Cargo stream exceeds bounds')
    messages = [json.loads(line) for line in compiler.read_text().splitlines() if line.strip()]
    require([row.get('success') for row in messages if row.get('reason') == 'build-finished'] == [True],
            'actual Cargo success record missing')
    for role in ROLES:
        artifact = select_artifact(messages, role)
        if artifact is None:
            continue
        row, executable = artifact
        # Test-build dependency artifacts are not role candidates.
        if role in ('imager_tests', 'backend_tests') and command[1] != 'test':
            continue
        if role not in ('imager_tests', 'backend_tests') and command[1] != 'build':
            continue
        original = facts(executable)
        saved = invocation / (role + '.elf')
        shutil.copyfile(executable, saved)
        saved.chmod(0o700)
        require(facts(saved) == original == facts(executable), 'compiled artifact changed while preserving')
        with saved.open('rb') as preserved:
            require(preserved.read(4) == b'\x7fELF', 'actual compiled artifact is not ELF')
        receipt = {'schema': 1, 'role': role, 'compiled_root': str(ROOT), 'command': command,
                   'build_exit': completed.returncode, 'listed_source_unchanged': unchanged,
                   'artifacts': [{'role': role, 'sha256': original['sha256'], 'compiler_artifact': row}]}
        paths = {'compiler': compiler, 'source_before': invocation / 'source-before.json',
                 'source_after': invocation / 'source-after.json', 'build_status': invocation / 'build-status.json'}
        for field, path in paths.items():
            receipt[field + '_sha256'] = facts(path)['sha256']
        producer = invocation / ('producer-' + role + '.json')
        write_document(producer, receipt)
        result = {'compiled_root': str(ROOT), 'path': str(saved), 'sha256': original['sha256'],
                  'producer': str(producer), 'producer_sha256': facts(producer)['sha256']}
        for field, path in paths.items():
            result[field] = str(path)
            result[field + '_sha256'] = facts(path)['sha256']
        write_document(invocation / ('role-' + role + '.json'), result)
    return 0


def cohort(directory, output, source_only):
    current = sources()
    required = {'worker', 'runtime', 'host', 'imager_tests', 'backend_tests'} if source_only else set(ROLES)
    roles = {}
    for role in sorted(required):
        matches = sorted(directory.glob('cargo-*/role-' + role + '.json'),
                         key=lambda path: path.stat().st_mtime_ns)
        usable = []
        for path in matches:
            row = json.loads(path.read_text())
            before = json.loads(Path(row['source_before']).read_text())
            after = json.loads(Path(row['source_after']).read_text())
            if before == after == current:
                usable.append(row)
        require(usable, 'no actual unchanged current build evidence for ' + role)
        roles[role] = usable[-1]
    value = {'schema': 1, 'source_root': str(ROOT),
             'source_inputs': {name: row['sha256'] for name, row in current.items()}, 'roles': roles}
    # Validate the same production admission contract before exposing a cohort.
    import importlib.util
    spec = importlib.util.spec_from_file_location('capture_qualifier', ROOT / 'apps/imager/packaging/qualify-capture-release.py')
    qualifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(qualifier)
    for role, row in roles.items():
        qualifier.validate_role(ROOT, role, row, value['source_inputs'])
    require(sources() == current, 'source changed during cohort assembly')
    write_document(output, value)


def assembly(sidecar, pins):
    directory = Path(os.environ['TILO_CAPTURE_BUILD_EVIDENCE']).absolute()
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    require(not directory.is_symlink() and directory.stat().st_uid == os.geteuid()
            and stat.S_IMODE(directory.stat().st_mode) == 0o700, 'assembly evidence must be privately owned')
    invocation = directory / ('assembly-' + uuid.uuid4().hex)
    invocation.mkdir(mode=0o700)
    before = sources()
    write_document(invocation / 'source-before.json', before)
    command = ['bash', str(ROOT / 'apps/imager/packaging/package-capture-service.sh'),
               '--assemble-only', str(sidecar.absolute()), str(pins.absolute())]
    with (invocation / 'assembly.stdout').open('xb') as stdout, (invocation / 'assembly.stderr').open('xb') as stderr:
        completed = subprocess.run(command, cwd=ROOT, stdout=stdout, stderr=stderr, check=False)
    for name in ('assembly.stdout', 'assembly.stderr'):
        (invocation / name).chmod(0o600)
    after = sources()
    write_document(invocation / 'source-after.json', after)
    write_document(invocation / 'assembly-status.json', {'schema': 1, 'command': command,
                   'exit_code': completed.returncode, 'listed_source_unchanged': before == after})
    require(completed.returncode == 0, 'actual capture assembly failed; its original logs are preserved')
    require(before == after, 'source changed during actual capture assembly; no success receipt emitted')
    import importlib.util
    spec = importlib.util.spec_from_file_location('capture_admission', ROOT / 'apps/imager/packaging/admit-capture-sidecar.py')
    admission = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(admission)
    decoded = json.loads(admission.admit(pins, sidecar, os.uname().machine))
    require(sources() == after, 'source changed during exact signed candidate admission')
    receipt = {'schema': 1, 'scope': 'signed-artifact-assembly', 'command': command,
               'exit_code': completed.returncode, 'listed_source_unchanged': True,
               'source_before_sha256': facts(invocation / 'source-before.json')['sha256'],
               'source_after_sha256': facts(invocation / 'source-after.json')['sha256'],
               'pins': {'path': str(pins.absolute()), **facts(pins)},
               'worker': facts(sidecar / 'service/payload/bin/tilo-home-export'),
               'runtime': facts(sidecar / 'runtime' / decoded['runtime_version'] / 'lib/libruntime_dylib.so'),
               'qualification': False}
    write_document(invocation / 'assembly-receipt.json', receipt)
    print(str(invocation / 'assembly-receipt.json'))


def main():
    if sys.argv[1:2] == ['assemble']:
        import argparse
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument('mode', choices=('assemble',))
        parser.add_argument('--sidecar', type=Path, required=True)
        parser.add_argument('--pins', type=Path, required=True)
        args = parser.parse_args()
        assembly(args.sidecar.absolute(), args.pins.absolute())
        return 0
    if sys.argv[1:2] == ['cohort']:
        import argparse
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument('mode', choices=('cohort',))
        parser.add_argument('--evidence', type=Path, required=True)
        parser.add_argument('--output', type=Path, required=True)
        parser.add_argument('--source-backend-only', action='store_true')
        args = parser.parse_args()
        cohort(args.evidence.absolute(), args.output.absolute(), args.source_backend_only)
        return 0
    return build(sys.argv[1:])


if __name__ == '__main__':
    sys.exit(main())

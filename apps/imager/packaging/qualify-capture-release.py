#!/usr/bin/env python3
"""Run exact current native capture cases; a signed candidate is not a qualification.

The operator supplies genuine compiler/source-pair receipts and an immutable
native image. This program executes cases itself, never accepts saved success.
All native work belongs to one newly owned rootful, network-isolated container.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import stat
import subprocess
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[3]
SOURCE_CASES = (
    'setup::home_capture::native_tests::signed_export::actual_signed_export_and_native_import_preserve_active_home',
    'setup::home_capture::native_tests::signed_export::cancellation::actual_export_cancellation_reaps_namespace_and_releases_epoch',
)
TARGET_CASE = 'installed-firstuse-native-python'
TARGET_FIXTURE = 'os/scripts/installer-receive-fixture/receive.py'
FIXTURE_FILES = (
    TARGET_FIXTURE, 'os/scripts/installer-receive-fixture/semantic.py',
    'os/scripts/installer-receive-fixture/delivery.py',
    'os/scripts/installer-receive-fixture/firstuse.py',
    'os/scripts/installer-receive-fixture/namespace.py',
    'os/scripts/installer-receive-fixture/apps.py',
    'os/scripts/installer-receive-fixture/cloud.py',
    'os/scripts/installer-receive-fixture/packages.py',
    'os/scripts/installer-receive-fixture/service_sessions.py',
    'os/scripts/installer-receive-fixture/service_processes.py',
    'os/scripts/installer-receive-fixture/service_contract.py',
    'os/scripts/installer-source-fixture/source.py',
    'os/scripts/installer-source-fixture/owned_gpt.py',
    'os/scripts/installer-source-fixture/bounded_native.py',
    'os/scripts/installer-source-fixture/flatpak_material.py',
    'os/scripts/installer-source-fixture/source_sessions.py',
    'os/scripts/installer-source-fixture/secret_owner.py',
    'scripts/headless-examples/_control.py',
    'apps/imager/packaging/service-source-assets.py',
    'os/scripts/installer-receive-fixture/prerequisite-image.py',
    'os/scripts/installer-receive-fixture/prerequisite-readiness.py',
    'os/scripts/installer-source-fixture/flatpak-runtime/prepare-runtime.py',
    'os/scripts/installer-source-fixture/flatpak-runtime/dependencies/assemble-app-runtime.py',
    'os/scripts/installer-source-fixture/flatpak-runtime/dependencies/bounded_native.py',
    'os/scripts/installer-source-fixture/flatpak-runtime/dependencies/common.py',
    'apps/imager/src/setup/flatpak_transfer/portable/native_tests/runnable/fixture-app',
    'os/usr/libexec/tilo-firstboot-user',
    'os/usr/libexec/tilo-firstboot-home-seeds.py',
    'os/usr/libexec/tilo-installer-prepared-setup-extract.py',
    'os/usr/libexec/tilo-staging-handoff',
    'os/usr/libexec/tilo-store-volume',
    'os/usr/libexec/tilo-install-handoff-copy.py',
    'os/scripts/verify-account-consumer-capability.py',
    'os/scripts/service-consumer-capability.py',
    'os/etc/pam.d/tilo-unlock',
    'os/usr/lib/sysusers.d/tilo-identity.conf',
    'os/usr/lib/tmpfiles.d/tilo.conf',
    'os/registry-runtime-publisher.hex',
)
PACKAGE_FILES = (
    'apps/file-explorer/tilo-app.toml', 'apps/file-explorer/surface.toml',
    'apps/word-processor/tilo-app.toml', 'apps/word-processor/surface.toml',
    'tools/apps/pack-runtime-package.sh', 'tools/apps/stage_sdk_services.py',
    'os/scripts/stamp-runtime.sh',
)
PACKAGES = {'worker': 'apps/imager', 'runtime': 'crates/runtime/runtime-dylib', 'host': 'apps/imager', 'imager_tests': 'apps/imager',
            'shell': 'crates/session/shell', 'fs': 'crates/system/fs',
            'credential_helper': 'crates/system/account-custody',
            'prepared_helper': 'apps/imager', 'control': 'crates/session/control-cli',
            'files': 'apps/file-explorer', 'writer': 'apps/word-processor',
            'app_package': 'crates/packaging/tilo-app-package-cli',
            'runtime_bundle': 'crates/packaging/runtime-bundle', 'install': 'crates/packaging/install'}
TARGETS = {'worker': 'tilo-home-export', 'runtime': 'runtime_dylib', 'host': 'imager', 'imager_tests': 'imager', 'shell': 'shell', 'fs': 'fs',
           'credential_helper': 'tilo-account-credential', 'prepared_helper': 'tilo-prepared-setup',
           'control': 'tilo-control', 'files': 'file-explorer', 'writer': 'word-processor',
           'app_package': 'tilo-app-package', 'runtime_bundle': 'tilo-runtime-bundle', 'install': 'tilo-install'}
BINDINGS = {'shell': '/usr/bin/shell', 'fs': '/usr/bin/fs',
            'credential_helper': '/usr/libexec/tilo-account-credential',
            'prepared_helper': '/usr/libexec/tilo-prepared-setup', 'control': '/usr/bin/tilo-control',
            'app_package': '/usr/bin/tilo-app-package',
            'runtime_bundle': '/usr/bin/tilo-runtime-bundle', 'install': '/usr/bin/tilo-install'}


def require(value, message):
    if not value:
        raise ValueError(message)


def unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate qualification field')
        result[key] = value
    return result



def target_completion(output):
    require(type(output) is bytes and 0 < len(output) <= 4096,
            'qualification receiving completion exceeds bound')
    value = json.loads(output, object_pairs_hook=unique)
    expected = {'schema': 1, 'status': 'completed',
                'shared_files_readwrite': True, 'writer_shared_file_edit': True,
                'apps_data_and_launch': True, 'protected_cloud_custody': True,
                'cloud_fs_and_files': True, 'identity_headless_lifecycle': True,
                'owned_resources_retired': True}
    require(type(value) is dict and set(value) == set(expected)
            and type(value['schema']) is int
            and all(type(value[key]) is bool for key in expected
                    if key not in ('schema', 'status'))
            and value == expected, 'qualification genuine receiving conditions incomplete')
    return value

def read_input(path, maximum=1024 ** 3, retain=False):
    path = Path(path).absolute()
    for edge in (path, *path.parents):
        require(not edge.is_symlink(), 'redirected qualification input')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= maximum
                and not before.st_mode & 0o022, 'unprotected or unbounded qualification input')
        digest = hashlib.sha256()
        size = 0
        captured = bytearray()
        while part := os.read(fd, min(1024 * 1024, maximum - size + 1)):
            size += len(part)
            require(size <= maximum, 'qualification input grew beyond bound')
            digest.update(part)
            if retain:
                captured.extend(part)
        after = os.fstat(fd)
        identity = lambda m: (m.st_dev, m.st_ino, m.st_size, m.st_mode, m.st_uid,
                              m.st_gid, m.st_mtime_ns, m.st_ctime_ns)
        require(identity(before) == identity(after) == identity(path.stat()),
                'qualification input changed while held')
        return {'sha256': digest.hexdigest(), 'bytes': size}, bytes(captured)
    finally:
        os.close(fd)


def facts(path, maximum=1024 ** 3):
    return read_input(path, maximum)[0]


def document(path):
    return json.loads(read_input(path, 32 * 1024 ** 2, True)[1], object_pairs_hook=unique)


def bound_file(path, expected):
    require(type(expected) is str and re.fullmatch('[0-9a-f]{64}', expected),
            'qualification digest is invalid')
    require(facts(path)['sha256'] == expected, 'qualification bytes differ')


def validate_role(root, role, row, sources):
    fields = {'compiled_root', 'path', 'sha256', 'compiler', 'compiler_sha256', 'producer', 'producer_sha256',
              'source_before', 'source_before_sha256', 'source_after', 'source_after_sha256',
              'build_status', 'build_status_sha256'}
    require(type(row) is dict and set(row) == fields, 'qualification role shape differs')
    for field in ('path', 'compiler', 'producer', 'source_before', 'source_after', 'build_status'):
        bound_file(row[field], row['sha256' if field == 'path' else field + '_sha256'])
    with Path(row['path']).open('rb') as executable:
        require(executable.read(4) == b'\x7fELF', 'qualification artifact is not ELF')
    before, after = document(row['source_before']), document(row['source_after'])
    require(before == after, 'qualification role source pair changed')
    for name, digest in sources.items():
        recorded = before.get(name)
        require(type(recorded) is dict and recorded.get('sha256') == digest,
                'qualification compiler source ledger omits current source')
        bound_file(root / name, digest)
    status, producer = document(row['build_status']), document(row['producer'])
    require(type(status.get('exit_code')) is int and status['exit_code'] == 0
            and status.get('listed_source_unchanged') is True,
            'qualification actual build did not succeed unchanged')
    require(type(row['compiled_root']) is str and Path(row['compiled_root']).is_absolute(),
            'qualification compiled root is missing')
    require(producer.get('compiled_root') == row['compiled_root']
            and type(producer.get('schema')) is int and producer.get('schema') == 1 and producer.get('role') == role
            and type(producer.get('build_exit')) is int and producer.get('build_exit') == 0 and producer.get('listed_source_unchanged') is True
            and producer.get('command') == status.get('command'),
            'qualification producer command or role differs')
    command = status['command']
    require(type(command) is list and len(command) >= 2 and command[:1] == ['cargo'] and '--locked' in command
            and command[1] == ('test' if role == 'imager_tests' else 'build'),
            'qualification compiler command differs')
    if role == 'imager_tests':
        require('--lib' in command and '--no-run' in command
                and 'native-capture-qualification' in ','.join(command),
                'qualification target was not compiled with native cases')
    for field in ('compiler', 'source_before', 'source_after', 'build_status'):
        require(producer.get(field + '_sha256') == row[field + '_sha256'],
                'qualification producer does not bind source and compiler receipts')
    messages = [json.loads(line, object_pairs_hook=unique)
                for line in read_input(row['compiler'], 32 * 1024 ** 2, True)[1].splitlines() if line.strip()]
    require([m.get('success') for m in messages if m.get('reason') == 'build-finished'] == [True],
            'qualification actual compiler success is absent')
    kind = 'lib' if role == 'imager_tests' else ('dylib' if role == 'runtime' else 'bin')
    candidates = [m for m in messages if m.get('reason') == 'compiler-artifact'
                  and m.get('target', {}).get('name') == TARGETS[role]
                  and m['target'].get('kind') == [kind]
                  and m.get('profile', {}).get('test') is (role == 'imager_tests')
                  and (m.get('executable') or role == 'runtime')]
    require(len(candidates) == 1, 'qualification actual compiler role is ambiguous')
    artifact = candidates[0]
    require(artifact.get('manifest_path') == str(Path(row['compiled_root']) / PACKAGES[role] / 'Cargo.toml'),
            'qualification compiler package differs')
    require(any(item.get('compiler_artifact') == artifact and item.get('sha256') == row['sha256']
                and item.get('role') == role for item in producer.get('artifacts', [])),
            'qualification ELF is not bound to actual producer')


def admit_cohort(path, host, full):
    value = document(path)
    require(type(value) is dict and set(value) == {'schema', 'source_root', 'source_inputs', 'roles'}
            and type(value['schema']) is int and value['schema'] == 1,
            'qualification cohort shape differs')
    root = Path(value['source_root'])
    require(root == ROOT, 'qualification cohort belongs to another checkout')
    sources = value['source_inputs']
    require(type(sources) is dict and 1 <= len(sources) <= 65536,
            'qualification current source map absent')
    required = {'Cargo.toml', 'Cargo.lock', 'apps/imager/build.rs',
                'apps/imager/src/setup/home_capture/native_tests/signed_export.rs',
                'apps/imager/packaging/qualify-capture-release.py', 'tools/ci/native-stores.sh', 'tools/ci/native-store-autoclear.py'}
    if full:
        required.update(FIXTURE_FILES)
        required.update(PACKAGE_FILES)
    listed = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--others',
                                      '--exclude-standard'], cwd=root).split(b'\0')
    for item in listed:
        if not item:
            continue
        name = item.decode('utf-8')
        if (name.endswith(('.rs', '.toml', '.ftl')) or name in ('Cargo.lock',)
                or name.startswith(('apps/imager/packaging/', 'os/scripts/', 'os/usr/libexec/'))):
            required.add(name)
    require(required <= sources.keys(), 'qualification essential sources absent')
    for name, digest in sources.items():
        require(type(name) is str and not Path(name).is_absolute() and '..' not in Path(name).parts,
                'qualification source escapes checkout')
        bound_file(root / name, digest)
    roles = value['roles']
    required_roles = set(PACKAGES) if full else {'worker', 'runtime', 'host', 'imager_tests'}
    require(type(roles) is dict and set(roles) == required_roles, 'qualification roles differ')
    for role, row in roles.items():
        validate_role(root, role, row, sources)
    require(facts(host) == facts(roles['host']['path']), 'selected capture host differs from cohort')
    return value


def execute(command, log, timeout):
    """Bound both pipes while owning the process group until terminal observation."""
    with Path(log).open('xb') as sink:
        os.chmod(log, 0o600)
        child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, start_new_session=True)
        output = bytearray()
        observed = None
        try:
            deadline = time.monotonic() + timeout
            with selectors.DefaultSelector() as streams:
                for pipe in (child.stdout, child.stderr):
                    os.set_blocking(pipe.fileno(), False)
                    streams.register(pipe, selectors.EVENT_READ)
                while streams.get_map() or observed is None:
                    require(time.monotonic() < deadline, 'qualification owned command exceeded deadline')
                    for key, _ in streams.select(.05):
                        part = os.read(key.fd, 8192)
                        if not part:
                            streams.unregister(key.fileobj)
                        else:
                            require(len(output) + len(part) <= 1024 ** 2,
                                    'qualification private output exceeded bound')
                            output.extend(part)
                            sink.write(part)
                    if observed is None:
                        observed = os.waitid(os.P_PID, child.pid,
                                             os.WEXITED | os.WNOHANG | os.WNOWAIT)
            code = observed.si_status if observed.si_code == os.CLD_EXITED else -observed.si_status
            return code, bytes(output)
        finally:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=10)
            child.stdout.close()
            child.stderr.close()


CONTAINER_RUNNER = r'''
import hashlib,importlib.util,json,os,pathlib,shutil,signal,time
assert os.getpid()==1 and os.getuid()==0 and os.geteuid()==0
root=pathlib.Path('/owned');root.mkdir(mode=0o711)
candidate=root/'candidate';shutil.copytree('/qualified',candidate);candidate.chmod(0o755)
spec=importlib.util.spec_from_file_location('actual_qualification',candidate/'apps/imager/packaging/qualify-capture-release.py')
qualifier=importlib.util.module_from_spec(spec);spec.loader.exec_module(qualifier)
sequence=0
def call(command,timeout=30):
 global sequence
 sequence+=1
 return qualifier.execute(command,root/('command-'+str(sequence)+'.log'),timeout)
def checked(command,timeout=30):
 code,output=call(command,timeout)
 assert code==0
 return output
def cancelled(signum,frame):raise RuntimeError('owned qualification interrupted')
signal.signal(signal.SIGTERM,cancelled)
checked(['mount','--make-rprivate','/'],10)
case=os.environ['TILO_CAPTURE_EXACT_CASE']
expected=json.loads((candidate/'subjects.json').read_bytes())
for name,row in expected.items():
 if name in ('worker','runtime','host','imager_tests'):continue
 target=row['installed']
 if not target:continue
 incoming=candidate/name
 assert incoming.is_file() and not incoming.is_symlink()
 with incoming.open('rb') as file:assert hashlib.file_digest(file,'sha256').hexdigest()==row['sha256']
 destination=pathlib.Path(target)
 destination.parent.mkdir(mode=0o755,parents=True,exist_ok=True)
 assert not destination.is_symlink()
 shutil.copyfile(incoming,destination);destination.chmod(0o755)
 p=pathlib.Path(target);m=p.stat(follow_symlinks=False)
 assert p.is_file() and not p.is_symlink() and m.st_uid==0 and not m.st_mode&0o022
 with p.open('rb') as file:assert hashlib.file_digest(file,'sha256').hexdigest()==row['sha256']
unexpected=False;code=1
evidence=pathlib.Path('/evidence/owned');evidence.mkdir(mode=0o700)
os.environ['TILO_NATIVE_STORE_AUTOCLEAR']='1'
os.environ['TILO_NATIVE_STORE_ASSOCIATIONS']=str(evidence)
stores_created=False
try:
 values={}
 if case==qualifier.TARGET_CASE:
  qualifier.prepare_source(candidate)
  initial=root/'initial-pidns';initial.touch(mode=0o400)
  checked(['mount','--bind','/qualification-initial-pidns',str(initial)],10)
  checked(['mount','-o','remount,bind,ro',str(initial)],10)
 else:
  setup=checked(['bash',str(candidate/'native-stores.sh'),'up',str(root/'stores')],120)
  stores_created=True
  values=dict(line.split('=',1) for line in setup.decode().splitlines())
  assert values=={'TILO_FS_NATIVE_STORES':'/owned/stores/a:/owned/stores/b','TILO_FS_NATIVE_STORE':'/owned/stores/a','TILO_REQUIRE_FANOTIFY':'1'}

 for dirname in ['home','config','data','state','cache','runtime']:(root/dirname).mkdir(mode=0o700)
 env=dict(PATH='/usr/bin:/usr/sbin',LANG='C',LC_ALL='C',HOME=str(root/'home'),
          XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),
          XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),
          XDG_RUNTIME_DIR=str(root/'runtime'),**values,
          TILO_CAPTURE_NATIVE_HOST=str(candidate/'host/bin/tilo-imager'),
          TILO_CAPTURE_NATIVE_PINS=str(candidate/'capture-service.json'),
          TILO_CAPTURE_QUALIFIED_CANDIDATE=str(candidate))
 os.environ.clear();os.environ.update(env)
 if case==qualifier.TARGET_CASE:
  code,native=call(['/usr/bin/python3','-I',str(candidate/qualifier.TARGET_FIXTURE),
                   '--candidate',str(candidate),'--evidence',str(evidence)],600)
  if code==0:qualifier.target_completion(native)
 else:
  assert case in qualifier.SOURCE_CASES
  code,native=call([str(candidate/'imager-tests'),case,'--exact','--nocapture'],600)
 os.write(1,native)
finally:
 # This fresh container PID namespace has no unrelated processes. Reap the
 # actual child before mount cleanup, and refuse success with adopted writers.
 children=pathlib.Path('/proc/1/task/1/children')
 unexpected=bool(children.read_text().strip())
 deadline=time.monotonic()+10
 while children.read_text().strip():
  for pid in children.read_text().split():
   try:os.kill(int(pid),signal.SIGKILL)
   except ProcessLookupError:pass
  while True:
   try:
    pid,_=os.waitpid(-1,os.WNOHANG)
    if pid==0:break
   except ChildProcessError:break
  assert time.monotonic()<deadline
  time.sleep(.01)
 if stores_created:
  checked(['bash',str(candidate/'native-stores.sh'),'down',str(root/'stores')],120)
  assert not any((root/'stores'/name).exists() for name in ['a','b','a.img','b.img'])
 assert not children.read_text().strip()

if code or unexpected:raise SystemExit(code or 1)
'''


def fixed_module(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def source_assets(path):
    assets = document(path)
    helper = fixed_module(ROOT / 'apps/imager/packaging/service-source-assets.py', 'public_assets')
    require(hashlib.sha256((json.dumps(assets, sort_keys=True, indent=2) + '\n').encode()).hexdigest()
            == helper.ASSET_MANIFEST_SHA256, 'official public asset manifest differs')
    return assets


def asset_mounts(assets):
    paths = {Path(row['source_root']) for row in assets['assets'].values()}
    paths.update(Path(row['source_path']) for row in assets['archives'].values())
    result = []
    for path in sorted(paths):
        # Never overlay the candidate, runtime, interpreter or protected image.
        require(path.is_absolute() and '..' not in path.parts and ':' not in str(path)
                and len(path.parts) > 2 and path.parts[1] in ('tmp', 'var', 'home')
                and not path.is_symlink(), 'public asset mount escapes input namespace')
        result.extend(['--volume', str(path) + ':' + str(path) + ':ro'])
    return result


def prepare_source(candidate):
    """Measure protected installed prerequisites and public assets before producer work."""
    inputs = document(candidate / 'source-inputs.json')
    prerequisites = fixed_module(candidate / 'os/scripts/installer-receive-fixture/prerequisite-image.py',
                                 'fixed_prerequisites')
    for relative, (installed, mode) in prerequisites.ASSETS.items():
        path = Path('/') / installed
        info = path.lstat()
        require(info.st_uid == 0 and stat.S_IMODE(info.st_mode) == mode,
                'installed source prerequisite custody differs')
        bound_file(path, inputs[relative])
    helper = fixed_module(candidate / 'apps/imager/packaging/service-source-assets.py', 'fixed_public_assets')
    helper.stage(document(candidate / 'source-assets.json'), candidate)


def stage_sources(stage, sources):
    """Copy the admitted transitive fixture closure, binding both sides."""
    for relative in (*FIXTURE_FILES, *PACKAGE_FILES):
        bound_file(ROOT / relative, sources[relative])
        target = stage / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / relative, target)
        target.chmod(0o644)
        bound_file(target, sources[relative])
    (stage / 'source-inputs.json').write_text(json.dumps(sources, sort_keys=True))


def run_native(args, cohort, output):
    engine = (['sudo', '-n'] if os.geteuid() != 0 else []) + ['podman']
    code, raw = execute(engine + ['image', 'inspect', args.image, '--format', '{{.ID}} {{.Architecture}}'],
                        output / 'image.log', 30)
    image = raw.decode().split()
    require(code == 0 and len(image) == 2
            and image[0].removeprefix('sha256:') == args.image.removeprefix('sha256:')
            and image[1] == {'x86_64': 'amd64', 'aarch64': 'arm64'}[args.arch],
            'qualification immutable image differs')
    cases = (*SOURCE_CASES, *((TARGET_CASE,) if not args.source_backend_only else ()))
    records = []
    with tempfile.TemporaryDirectory(prefix='tilo-capture-qualification-') as scratch:
        stage = Path(scratch)
        host = stage / 'host/bin/tilo-imager'
        host.parent.mkdir(parents=True)
        shutil.copyfile(args.host, host)
        host.chmod(0o755)
        sidecar = stage / 'host/share/tilo-imager/capture'
        sidecar.parent.mkdir(parents=True)
        shutil.copytree(args.sidecar, sidecar)
        shutil.copyfile(args.pins, stage / 'capture-service.json')
        shutil.copyfile(cohort['roles']['imager_tests']['path'], stage / 'imager-tests')
        (stage / 'imager-tests').chmod(0o755)
        for filename in ('native-stores.sh', 'native-store-autoclear.py'):
            shutil.copyfile(ROOT / 'tools/ci' / filename, stage / filename)
        driver = stage / 'apps/imager/packaging/qualify-capture-release.py'
        driver.parent.mkdir(parents=True)
        shutil.copyfile(__file__, driver)
        if not args.source_backend_only:
            stage_sources(stage, cohort['source_inputs'])
            assets = source_assets(args.source_assets)
            (stage / 'source-assets.json').write_text(json.dumps(assets, sort_keys=True))
        subjects = {role: {'sha256': row['sha256'], 'installed': BINDINGS.get(role)}
                    for role, row in cohort['roles'].items()}
        (stage / 'subjects.json').write_text(json.dumps(subjects, sort_keys=True))
        require(facts(host) == facts(args.host)
                and facts(stage / 'imager-tests') == facts(cohort['roles']['imager_tests']['path']),
                'qualification staging changed actual ELF bytes')
        for role in BINDINGS:
            if role in cohort['roles']:
                shutil.copyfile(cohort['roles'][role]['path'], stage / role)
                (stage / role).chmod(0o755)
        for role in ('files', 'writer'):
            if role in cohort['roles']:
                shutil.copyfile(cohort['roles'][role]['path'], stage / TARGETS[role])
                (stage / TARGETS[role]).chmod(0o755)
        for index, case in enumerate(cases):
            name = 'tilo-capture-qualify-' + uuid.uuid4().hex
            evidence = output / f'{index}-owned-loop-evidence'
            evidence.mkdir(mode=0o700)
            launched = False
            command = engine + ['run', '--name', name, '--privileged', '--network=none',
                                '--security-opt', 'label=disable', '--volume', str(stage) + ':/qualified:ro',
                                '--volume', str(evidence.absolute()) + ':/evidence:rw',
                                '--env', 'TILO_CAPTURE_EXACT_CASE=' + case]
            if case == TARGET_CASE:
                command.extend(['--volume', '/proc/1/ns/pid:/qualification-initial-pidns:ro'])
                command.extend(asset_mounts(assets))
            try:
                if case in SOURCE_CASES:
                    listed, listing = execute(command + [args.image, '/qualified/imager-tests', '--list'],
                                              output / f'{index}-list.log', 30)
                    require(listed == 0 and listing.splitlines().count((case + ': test').encode()) == 1,
                            'qualification exact native case unavailable')
                    # Retire the listing container before creating the actual owned case.
                    retired, _ = execute(engine + ['rm', name], output / f'{index}-list-retire.log', 30)
                    require(retired == 0, 'qualification listing container retirement failed')
                launched = True
                status, native = execute(command + [args.image, 'python3', '-I', '-c', CONTAINER_RUNNER],
                                         output / f'{index}-native.log', 900)
                require(status == 0, 'qualification actual native transfer case failed')
                if case == TARGET_CASE:
                    target_completion(native)
                else:
                    require(re.search(rb'test result: ok\. 1 passed; 0 failed; 0 ignored;', native),
                            'qualification exact native source case failed')
                records.append({'case': case, 'exit_code': status,
                                'log_sha256': facts(output / f'{index}-native.log')['sha256']})
            finally:
                retired, _ = execute(engine + ['rm', '--force', '--ignore', name],
                                      output / f'{index}-retire.log', 30)
                absent, _ = execute(engine + ['container', 'exists', name],
                                     output / f'{index}-absent.log', 30)
                require(retired == 0 and absent == 1, 'qualification owned container remains')
                if launched and case in SOURCE_CASES:
                    observer = 'tilo-capture-observe-' + uuid.uuid4().hex
                    try:
                        observed, raw = execute(engine + ['run', '--name', observer, '--privileged',
                            '--network=none', '--security-opt', 'label=disable',
                            '--volume', str(stage) + ':/qualified:ro',
                            '--volume', str(evidence.absolute()) + ':/evidence:ro', args.image,
                            'python3', '-I', '/qualified/native-store-autoclear.py', 'observe',
                            '--journal', '/evidence/owned/associations.json'],
                            output / f'{index}-loop-observer.log', 30)
                        require(observed == 0 and json.loads(raw, object_pairs_hook=unique) == {
                            'schema': 1, 'observed_backings': 2, 'owned_associations_retired': True},
                            'qualification owned loop association retirement unproved')
                    finally:
                        removed, _ = execute(engine + ['rm', '--force', '--ignore', observer],
                            output / f'{index}-observer-retire.log', 30)
                        gone, _ = execute(engine + ['container', 'exists', observer],
                            output / f'{index}-observer-absent.log', 30)
                        require(removed == 0 and gone == 1, 'qualification observer remains')
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--host', type=Path, required=True)
    parser.add_argument('--pins', type=Path, required=True)
    parser.add_argument('--sidecar', type=Path, required=True)
    parser.add_argument('--arch', choices=('x86_64', 'aarch64'), required=True)
    parser.add_argument('--cohort', type=Path, default=os.environ.get('TILO_CAPTURE_QUALIFICATION_COHORT'))
    parser.add_argument('--image', default=os.environ.get('TILO_CAPTURE_QUALIFIER_IMAGE'))
    parser.add_argument('--output', type=Path)
    parser.add_argument('--source-backend-only', action='store_true')
    parser.add_argument('--source-assets', type=Path,
                        help='frozen official public distribution manifest; required for full migration')
    args = parser.parse_args()
    require(args.cohort is not None, 'actual current compiler/source qualification cohort required')
    require(args.source_backend_only or args.source_assets is not None,
            'full qualification requires measured official public source assets')
    require(type(args.image) is str and re.fullmatch('sha256:[0-9a-f]{64}', args.image),
            'actual immutable native qualifier image required')
    args.host, args.pins, args.sidecar = [p.absolute() for p in (args.host, args.pins, args.sidecar)]
    cohort = admit_cohort(args.cohort, args.host, not args.source_backend_only)
    spec = importlib.util.spec_from_file_location('capture_admission', Path(__file__).with_name('admit-capture-sidecar.py'))
    admission = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(admission)
    pins = admission.admit(args.pins, args.sidecar, args.arch)
    bound_file(args.sidecar / 'service/payload/bin/tilo-home-export',
               cohort['roles']['worker']['sha256'])
    decoded = json.loads(pins, object_pairs_hook=unique)
    bound_file(args.sidecar / 'runtime' / decoded['runtime_version'] / 'lib/libruntime_dylib.so',
               cohort['roles']['runtime']['sha256'])
    original = {'cohort': facts(args.cohort), 'host': facts(args.host), 'pins': facts(args.pins)}
    output = args.output or Path(tempfile.mkdtemp(prefix='tilo-capture-qualification-receipt-'))
    if args.output:
        output.mkdir(mode=0o700)
    require(output.stat().st_uid == os.geteuid() and stat.S_IMODE(output.stat().st_mode) == 0o700,
            'qualification output is not privately owned')
    records = run_native(args, cohort, output)
    require(admission.admit(args.pins, args.sidecar, args.arch) == pins,
            'qualification sidecar changed during native execution')
    require(original == {'cohort': facts(args.cohort), 'host': facts(args.host), 'pins': facts(args.pins)},
            'qualification candidate changed during native execution')
    admit_cohort(args.cohort, args.host, not args.source_backend_only)
    receipt = {'schema': 1, 'scope': 'source-backend' if args.source_backend_only else 'full-migration',
               'image': args.image, 'arch': args.arch, 'candidate': original,
               'qualifier_sha256': facts(__file__)['sha256'], 'cases': records,
               'owned_resources_retired': True, 'publication_qualified': not args.source_backend_only}
    (output / 'receipt.json').write_text(json.dumps(receipt, sort_keys=True, indent=2) + '\n')
    (output / 'receipt.json').chmod(0o600)
    print(json.dumps({'schema': 1, 'scope': receipt['scope'],
                      'publication_qualified': receipt['publication_qualified']}, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError):
        raise SystemExit('capture qualification refused; inspect privately owned qualification diagnostics')

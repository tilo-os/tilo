"""The normal native authentication, installed retention and firstboot handoff."""
import json
import os
from pathlib import Path
import pwd
import stat
import time
import tempfile

EXTRACTOR = '/usr/libexec/tilo-installer-prepared-setup-extract.py'
PREPARED = '/usr/libexec/tilo-prepared-setup'
HANDOFF = '/usr/libexec/tilo-staging-handoff'


def fixed(command, log, secret=None, maximum=81920):
    # Loaded only from the admitted fixed receiving dependency directory.
    from semantic import bounded_command
    return bounded_command(command, time.monotonic() + 120, log, secret, maximum)


def private_write(path, content):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        with os.fdopen(fd, 'wb', closefd=False) as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
    finally:
        os.close(fd)


def prepare(package_directory, password, target_var, log):
    """Run only in the actual owned receiving mount namespace before login.

    The package's receipt and caller-supplied path confer no grants. The real
    native authenticate-install entry replays the protected declarations.
    """
    if os.geteuid() != 0:
        raise RuntimeError('receiving delivery requires the owned root fixture')
    package_directory = Path(package_directory)
    target_var = Path(target_var)
    source_info = package_directory.lstat()
    if not stat.S_ISDIR(source_info.st_mode) or source_info.st_mode & 0o022:
        raise RuntimeError('source published package directory is not protected')
    with tempfile.TemporaryDirectory(prefix='native-auth-', dir='/run') as quarantine:
        authenticated = fixed([PREPARED, '--authenticate-install', '--package',
                               str(package_directory / 'prepared-setup.sealed'), '--receipt',
                               str(package_directory / 'receipt.json'), '--quarantine',
                               quarantine + '/contents', '--verify-retained-source'], log, password)
        authenticated = fixed(['/usr/bin/python3', EXTRACTOR, 'preflight-authenticated',
                               '--source-dir', str(package_directory)], log, authenticated)
    reviewed = json.loads(authenticated)
    if not reviewed.get('retained_home') or not reviewed.get('pending_computer_applications_scope'):
        raise RuntimeError('actual producer did not publish the paired Computer declaration')
    profile = reviewed.get('install_profile')
    if not isinstance(profile, dict):
        raise RuntimeError('native setup lacks actual reviewed target account')
    username = profile['username']
    if not isinstance(username, str) or not username:
        raise RuntimeError('native setup target username differs')
    # Actual NSS availability, not a deletion or relabelling of an existing user.
    try:
        pwd.getpwnam(username)
    except KeyError:
        pass
    else:
        raise RuntimeError('target account already exists')
    try:
        pwd.getpwuid(1000)
    except KeyError:
        pass
    else:
        raise RuntimeError('target image already has UID1000')
    fixed([HANDOFF, 'deliver-retained', '--target-var', str(target_var)], log, authenticated)
    fixed(['/usr/bin/python3', EXTRACTOR, 'retain', '--source-dir', str(package_directory),
           '--target-var', str(target_var)], log)
    fixed(['/usr/bin/python3', EXTRACTOR, 'retain-authenticated-binding', '--source-dir',
           str(package_directory), '--target-var', str(target_var)], log, authenticated)
    with tempfile.TemporaryDirectory(prefix='native-device-', dir='/run') as quarantine:
        fixed([PREPARED, '--install-enrolled-device', '--package',
               str(target_var / 'lib/tilo/prepared-setup/prepared-setup.sealed'),
               '--quarantine', quarantine + '/contents', '--system-root',
               str(target_var / 'lib/tilo')], log, password)
    fixed(['/usr/bin/systemd-sysusers', '--root=/', '/usr/lib/sysusers.d/tilo-identity.conf'], log)
    encoded = fixed(['/usr/bin/openssl', 'passwd', '-6', '-stdin'], log, password)
    if not encoded.startswith(b'$6$') or b'\n' in encoded.rstrip(b'\n'):
        raise RuntimeError('native password hash producer refused')
    private_write('/etc/tilo/install-user.conf', username.encode() + b':' + encoded.rstrip(b'\n') + b'\n')
    private_write('/etc/tilo/install-user-full-name', profile['full_name'].encode() + b'\n')
    fixed(['/usr/libexec/tilo-firstboot-user'], log)
    account = pwd.getpwnam(username)
    if account.pw_uid != 1000 or Path(account.pw_dir) != Path('/var/home') / username:
        raise RuntimeError('actual firstboot account differs from reviewed source UID')
    if Path('/etc/tilo/install-user.conf').exists():
        raise RuntimeError('normal firstboot did not consume the credential seed')
    return reviewed, account


class AuthenticatedStagingPlan:
    """Private fixture observation of the native-authenticated composite Plan.

    This cannot grant source access. The actual read/copy consumers still replay
    their protected root declarations and signed source witnesses independently.
    """
    def __init__(self, plan, scope, native_services, service_protocol=None):
        self.plan = plan
        self.scope = scope
        self.native_services = native_services
        self.service_protocol = service_protocol


def bind_service_candidate(attested, candidate, setup_binding):
    """Bind observations to the native-authenticated manifest, not source hints.

    This is not installed consumer admission. The temporary native door is
    root-owned, namespace/actor bound and independently checks the sealed person.
    """
    from service_sessions import selections
    if (type(candidate) is not dict or type(candidate.get('required_protocol')) is not int
            or candidate['required_protocol'] != 2
            or type(attested.service_protocol) is not int
            or attested.service_protocol != candidate['required_protocol']):
        raise RuntimeError('authenticated service manifest differs from candidate protocol')
    rows = selections(attested.native_services)
    if any(row['setup_binding'] != setup_binding for row in rows):
        raise RuntimeError('authenticated service manifest differs from source setup binding')
    return rows


def authenticated_staging_plan(package_directory, password, source, evidence, log):
    import hashlib
    if os.getuid() != 0 or os.geteuid() != 0 or not source.source_epoch_retired:
        raise RuntimeError('native staging read requires actual retired source epoch and root owner')
    source.verify()
    # Unlike source verification, pure native package authentication needs no
    # source mount/service epoch. Physical source readback is independently
    # checked by the owner before mutation and by normal delivery afterwards.
    with tempfile.TemporaryDirectory(prefix='staging-auth-', dir=evidence) as private:
        root = Path(private)
        authenticated = fixed([PREPARED, '--authenticate-install', '--package',
                               str(Path(package_directory) / 'prepared-setup.sealed'),
                               '--receipt', str(Path(package_directory) / 'receipt.json'),
                               '--quarantine', str(root / 'contents')], log, password)
        receipt = json.loads(authenticated)
        manifest_path = root / 'contents/home/manifest.json'
        fd = os.open(manifest_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
        try:
            before = os.fstat(fd)
            if (not stat.S_ISREG(before.st_mode) or before.st_uid != 0
                    or before.st_mode & 0o077 or not 0 < before.st_size <= 131072):
                raise RuntimeError('native authenticated manifest is not bounded private custody')
            raw = os.read(fd, before.st_size + 1)
            after = os.fstat(fd)
            if len(raw) != before.st_size or (before.st_dev, before.st_ino, before.st_size,
                    before.st_mtime_ns, before.st_ctime_ns) != (after.st_dev, after.st_ino,
                    after.st_size, after.st_mtime_ns, after.st_ctime_ns):
                raise RuntimeError('native authenticated manifest changed while observed')
            manifest = json.loads(raw)
        finally:
            os.close(fd)
        composite = manifest.get('computer_applications')
        scope = receipt.get('pending_computer_applications_scope')
        if not isinstance(composite, dict) or not isinstance(scope, dict):
            raise RuntimeError('actual native package lacks composite staging declaration')
        plan = composite['staging_plan']
        # Native Plan contains only ordered struct fields, strings and integers.
        # The authenticated manifest was emitted by that same serde serializer;
        # preserve its field order rather than sorting or constructing geometry.
        canonical = json.dumps(plan, separators=(',', ':'), ensure_ascii=False,
                               allow_nan=False).encode()
        if (hashlib.sha256(canonical).hexdigest() != scope['staging_plan_sha256']
                or composite['staging_operation'] != scope['staging_operation']
                or composite['source_operation'] != scope['source_operation']):
            raise RuntimeError('authenticated native staging scope differs from its exact Plan')
        disk = plan['basis']['disk']
        if disk['id'] != str(source.disk) or disk['guid'].lower() != source.disk_guid.lower():
            raise RuntimeError('native staging targets a different physical owned source disk')
        partitions = [item for item in disk['partitions']
                      if item['guid'].lower() == source.partition_guid.lower()]
        if len(partitions) != 1 or plan['shrink'] is not None:
            raise RuntimeError('native fixture does not have unchanged genuine source partition')
        source.verify()
        selected = manifest.get('service_sessions')
        native_services = None
        if selected is not None:
            native_services = {'schema': 1, 'sessions': [
                {'id': row['id'], 'service': row['service'], 'setup_binding': manifest['setup_binding']}
                for row in selected['sessions']]}
        return AuthenticatedStagingPlan(plan, scope, native_services,
                                        selected.get('consumer_protocol') if selected is not None else None)

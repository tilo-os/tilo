#!/usr/bin/env python3
"""Held native source for the full same-computer installer qualification.

Only creates its own 64 GiB raw GPT disk with an 8 GiB Ext4 partition. Never
accepts an existing backing image/device, loads modules, or invents installer
inventory. The ordinary source installer discovers this actual block device.
The caller keeps this context through semantic preparation, receiving first
use and observed consumer retirement. It runs only in the dedicated rootful,
no-network native qualification namespace, never the user's desktop.
"""
from contextlib import contextmanager
import array
import fcntl
import hashlib
import json
import os
from pathlib import Path
import select
import stat
import subprocess
import struct
import time
import uuid
from bounded_native import bounded, retire_child, observe_exit, assert_children_retired

NS_GET_TGID_FROM_PIDNS = 0x8004b707  # _IOR(0xb7, 0x7, int), linux/nsfs.h
SIZE = 64 * 1024 * 1024 * 1024
PARTITION_SECTORS = 8 * 1024 * 1024 * 1024 // 512
_LIVE_ATTACHMENTS = {}
_LIVE_RECEIVING = {}
OBSERVATION_LIMIT = 4096
TOOL_BYTES = 512 * 1024 * 1024
def checked(command, *, pass_fds=()):
    code, out, err = bounded(command, timeout=60, pass_fds=pass_fds)
    if code:
        raise RuntimeError(f'{Path(command[0]).name} failed: {code}: {err[:512].decode(errors="replace")}')
    return out


def limited_read(path, limit=4096):
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        data = os.read(fd, limit + 1)
        if len(data) > limit:
            raise RuntimeError('native observation exceeds its byte bound')
        return data
    finally:
        os.close(fd)


def file_hash(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        metadata = os.fstat(fd)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > TOOL_BYTES:
            raise RuntimeError('native tool hash subject exceeds its fixed bound')
        digest = hashlib.sha256()
        remaining = TOOL_BYTES + 1
        deadline = time.monotonic() + 10
        while remaining:
            if time.monotonic() >= deadline:
                raise RuntimeError('native tool hash observation deadline exceeded')
            part = os.read(fd, min(1024 * 1024, remaining))
            if not part:
                return digest.hexdigest()
            digest.update(part)
            remaining -= len(part)
        raise RuntimeError('native tool grew beyond its fixed hash bound')
    finally:
        os.close(fd)


def start(pid):
    content = limited_read(f'/proc/{pid}/stat').decode('utf-8', errors='strict')
    fields = content.rsplit(') ', 1)[1].split()
    if fields[0] == 'Z':
        raise RuntimeError('owned NBD process exited')
    return int(fields[19])


def scan(path, limit=OBSERVATION_LIMIT):
    deadline = time.monotonic() + 2
    with os.scandir(path) as entries:
        for index, entry in enumerate(entries):
            if index >= limit or time.monotonic() >= deadline:
                raise RuntimeError('native directory observation bound exceeded')
            yield entry


def check_process(pid, stamp, image):
    if start(pid) != stamp:
        raise RuntimeError('owned NBD process identity changed')
    actual = Path(f'/proc/{pid}/exe').stat()
    expected = Path('/usr/bin/qemu-nbd').stat()
    if (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino):
        raise RuntimeError('NBD child is not the exact selected executable')
    inode = image.stat(follow_symlinks=False)
    found = False
    for entry in scan(f'/proc/{pid}/fd'):
        try:
            value = entry.stat()
        except FileNotFoundError:
            continue
        if (value.st_dev, value.st_ino) == (inode.st_dev, inode.st_ino):
            found = True
            break
    if not found or start(pid) != stamp:
        raise RuntimeError('NBD child does not hold the exact owned image inode')


class Attachment:
    def __init__(self, scratch, namespace, image, device, descriptor):
        self.scratch, self.namespace, self.image = scratch, namespace, image
        self.device, self.descriptor = device, descriptor
        self.pidfd, self.pid, self.stamp = None, None, None
        self.mount = None
        self.partition_fd = None
        self.original_mount = None
        self.mount_origin = None
        self.owned = False
        self.child = None
        self.device_rdev = os.fstat(descriptor).st_rdev
        self.backing_fd = os.open(image, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        self.backing_identity = os.fstat(self.backing_fd)
        _LIVE_ATTACHMENTS[id(self)] = self

    def connect(self):
        # QEMU10.2.2: --connect daemonizes only without --verbose or with
        # --fork. Foreground ownership is retained before any readiness poll.
        command = ['/usr/bin/qemu-nbd', '--verbose', '--persistent', '--format=raw',
                   f'--socket={self.scratch / "export.sock"}',
                   f'--connect={self.device}', str(self.image)]
        self.child = subprocess.Popen(command, env={'PATH': '/usr/bin', 'LANG': 'C', 'LC_ALL': 'C'},
                                      stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL, start_new_session=True)
        self.pid = self.child.pid
        self.pidfd = os.pidfd_open(self.pid)
        self.stamp = start(self.pid)
        deadline = time.monotonic() + 30
        while True:
            if observe_exit(self.child) is not None:
                raise RuntimeError('owned foreground NBD child exited before readiness')
            if time.monotonic() >= deadline:
                raise RuntimeError('owned NBD attachment readiness deadline exceeded')
            node = Path('/sys/class/block') / self.device.name
            if (node / 'pid').exists() and limited_read(node / 'size').strip() == str(SIZE // 512).encode():
                self.verify()
                self.owned = True
                break
            time.sleep(.05)
        (self.scratch / 'attachment.json').write_text(json.dumps({
            'device': str(self.device), 'device_rdev': self.device_rdev,
            'private_pid': self.pid, 'start': self.stamp,
            'image': str(self.image), 'image_inode': self.backing_identity.st_ino,
            'image_dev': self.backing_identity.st_dev}))

    def verify(self):
        named = self.image.stat(follow_symlinks=False)
        held = os.fstat(self.backing_fd)
        if (not stat.S_ISREG(named.st_mode) or named.st_uid or named.st_mode & 0o077
                or (held.st_dev, held.st_ino) != (self.backing_identity.st_dev, self.backing_identity.st_ino)
                or (named.st_dev, named.st_ino) != (held.st_dev, held.st_ino)
                or held.st_size != SIZE):
            raise RuntimeError('owned source backing inode changed')
        if self.pidfd is None or select.select([self.pidfd], [], [], 0)[0] or start(self.pid) != self.stamp:
            raise RuntimeError('owned NBD process custody retired')
        # Revalidation's temporary pidfd must be closed, retaining original pin.
        check_process(self.pid, self.stamp, self.image)
        device = self.device.stat(follow_symlinks=False)
        if (os.fstat(self.descriptor).st_rdev != self.device_rdev
                or not stat.S_ISBLK(device.st_mode) or device.st_rdev != self.device_rdev):
            raise RuntimeError('owned NBD process/device identity changed')
        global_pid = int(limited_read(f'/sys/class/block/{self.device.name}/pid').strip())
        value = array.array('i', [global_pid])
        # Translate only this exact kernel NBD task through a held initial PID
        # namespace handle. No host proc enumeration, setns or PID-host fallback.
        fcntl.ioctl(self.namespace, NS_GET_TGID_FROM_PIDNS, value, True)
        if value[0] != self.pid:
            raise RuntimeError('NBD kernel client is not the owned image server')
        current_bytes = struct.unpack('Q', fcntl.ioctl(self.descriptor, 0x80081272, bytes(8)))[0]
        if current_bytes != SIZE:
            raise RuntimeError('owned NBD geometry changed')

    def unmount_source(self):
        if self.mount is None:
            return
        self.verify()
        source = checked(['/usr/bin/findmnt', '-n', '-o', 'SOURCE', '--target', str(self.mount)]).decode().strip()
        if source != str(self.device) + 'p1':
            raise RuntimeError('owned GPT mount was replaced; no unmount performed')
        checked(['/usr/bin/umount', str(self.mount)])
        restored = self.mount.stat(follow_symlinks=False)
        if (restored.st_dev, restored.st_ino) != self.mount_origin:
            raise RuntimeError('owned source mountpoint identity changed during retirement')
        self.mount.rmdir()
        self.mount = None

    def cleanup(self):
        if any(target.source.attachment is self for target in _LIVE_RECEIVING.values()):
            raise RuntimeError('receiving physical custody has not retired')
        self.unmount_source()
        if self.partition_fd is not None:
            os.close(self.partition_fd)
            self.partition_fd = None
        if self.owned:
            self.verify()
            checked(['/usr/bin/qemu-nbd', '--disconnect', str(self.device)])
            self.owned = False
        if self.child is not None:
            # The actual foreground Child exists even if pidfd acquisition or
            # executable/backing/readiness validation failed. No pidfile recovery.
            retire_child(self.child)
            self.child = None
        if self.pidfd is not None:
            os.close(self.pidfd)
            self.pidfd = None
        # A daemon exit alone does not prove the kernel released its configured
        # block endpoint. Keep the image/release receipt withheld until the exact
        # still-held node is observed idle; never disconnect unproven ownership.
        verify_idle_node(self.device, self.descriptor, self.device_rdev)
        os.close(self.descriptor)
        os.close(self.backing_fd)
        _LIVE_ATTACHMENTS.pop(id(self), None)


def verify_idle_node(device, descriptor, expected_rdev):
    node = Path('/sys/class/block') / device.name
    deadline = time.monotonic() + 5
    while True:
        held = os.fstat(descriptor)
        named = device.stat(follow_symlinks=False)
        if (not stat.S_ISBLK(held.st_mode) or not stat.S_ISBLK(named.st_mode)
                or held.st_rdev != expected_rdev or named.st_rdev != expected_rdev):
            raise RuntimeError('owned NBD cleanup node identity changed')
        if not (node / 'pid').exists() and limited_read(node / 'size').strip().decode() == '0':
            return
        if time.monotonic() >= deadline:
            raise RuntimeError('owned NBD kernel release unproven; preserve image and no release receipt')
        time.sleep(0.05)


def free_device():
    # No module load/install, physical device enumeration or existing selection.
    for entry in scan('/sys/class/block', limit=1024):
        node = Path(entry.path)
        if not node.name.startswith('nbd') or not node.name[3:].isdigit():
            continue
        device = Path('/dev') / node.name
        if (node / 'pid').exists() or limited_read(node / 'size').strip().decode() != '0':
            continue
        descriptor = os.open(device, os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            if not stat.S_ISBLK(os.fstat(descriptor).st_mode):
                raise RuntimeError('NBD node is not a block device')
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            if (node / 'pid').exists() or limited_read(node / 'size').strip().decode() != '0':
                os.close(descriptor)
                continue
            return device, descriptor
        except BlockingIOError:
            os.close(descriptor)
        except BaseException:
            os.close(descriptor)
            raise
    raise RuntimeError('no already available unoccupied exclusively locked NBD device; qualification incomplete')


def create_gpt(image):
    fd = os.open(image, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    os.ftruncate(fd, SIZE)
    os.close(fd)
    disk, partition = str(uuid.uuid4()), str(uuid.uuid4())
    sectors = PARTITION_SECTORS
    table = f'label: gpt\nlabel-id: {disk}\nunit: sectors\n\nstart=2048,size={sectors},type=0fc63daf-8483-4772-8e79-3d69d8477de4,uuid={partition}\n'
    # Only this newly created REGULAR image is ever passed to partition writer.
    table_path = image.parent / 'gpt.table'
    descriptor = os.open(table_path, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, 'w+b') as stream:
        stream.write(table.encode())
        stream.seek(0)
        code, _, _ = bounded(['/usr/bin/sfdisk', '--no-reread', str(image)],
                             stdin=stream, timeout=30, limit=8192)
    if code:
        raise RuntimeError('fresh GPT regular-image creation failed')
    return disk, partition


@contextmanager
def owned_source(stage, namespace, verify_consumers_retired):
    if os.geteuid() != 0 or not stage.is_absolute() or stage.exists():
        raise RuntimeError('root and a fresh absolute root-private source stage required')
    if not callable(verify_consumers_retired):
        raise RuntimeError('actual receiving consumer retirement observer required')
    if fcntl.ioctl(namespace, 0xb703) != 0x20000000:
        raise RuntimeError('held initial namespace is not a PID namespace')
    parent = stage.parent.stat(follow_symlinks=False)
    if not stat.S_ISDIR(parent.st_mode) or parent.st_uid or parent.st_mode & 0o077:
        raise RuntimeError('source stage parent must be an existing root-private directory')
    namespace = os.dup(namespace)
    fs = 'ext4'
    sectors = PARTITION_SECTORS
    inventory = immutable_tools()
    stage.mkdir(mode=0o700)
    (stage / 'tool-inventory.json').write_text(json.dumps(inventory))
    image = stage / 'source.img'
    disk, partition = create_gpt(image)
    device, fd = free_device()
    attachment = Attachment(stage, namespace, image, device, fd)
    try:
        attachment.connect()
        child = Path(str(device) + 'p1')
        deadline = time.monotonic() + 10
        while not child.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        if not child.exists():
            raise RuntimeError('native NBD partition scanning unavailable; qualification incomplete')
        attachment.verify()
        before = json.loads(checked(['/usr/bin/lsblk', '-J', '-b', '-p', '-o',
                                     'NAME,TYPE,SIZE,PTTYPE,PTUUID,PARTUUID,START', str(device)]))
        row = before['blockdevices'][0]
        if row['type'] != 'disk' or row['pttype'] != 'gpt' or row['ptuuid'].lower() != disk:
            raise RuntimeError('production TYPE=disk/GPT identity unavailable')
        if len(row.get('children', [])) != 1:
            raise RuntimeError('owned GPT partition identity changed')
        part = row['children'][0]
        if (part['name'] != str(child) or part['type'] != 'part'
                or part['partuuid'].lower() != partition
                or int(part['start']) != 2048 or int(part['size']) != sectors * 512
                or int(row['size']) != SIZE):
            raise RuntimeError('owned GPT partition geometry/type changed')
        if (Path('/sys/class/block') / child.name).resolve().parent.name != device.name:
            raise RuntimeError('owned GPT partition kernel parent changed')
        child_fd = os.open(child, os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            child_metadata = os.fstat(child_fd)
            if not stat.S_ISBLK(child_metadata.st_mode):
                raise RuntimeError('owned GPT partition is not a block device')
            expected_dev = limited_read(Path('/sys/class/block') / child.name / 'dev').strip().decode()
            if expected_dev != f'{os.major(child_metadata.st_rdev)}:{os.minor(child_metadata.st_rdev)}':
                raise RuntimeError('owned GPT partition node identity mismatch')
            attachment.verify()
            held_child = f'/proc/self/fd/{child_fd}'
            checked(['/usr/bin/mkfs.ext4', '-q', '-F', held_child], pass_fds=(child_fd,))
            if child.stat().st_rdev != child_metadata.st_rdev:
                raise RuntimeError('owned GPT partition path changed after formatting')
        except BaseException:
            os.close(child_fd)
            raise
        attachment.partition_fd = child_fd
        attachment.verify()
        # Original UID1000 peer must traverse the source mount. Backing images
        # and receipts remain in root-private stage; expose only this mount.
        mount = Path('/var/tmp') / f'tilo-installer-original-{uuid.uuid4()}'
        mount.mkdir(mode=0o711)
        original = mount.stat(follow_symlinks=False)
        attachment.mount_origin = (original.st_dev, original.st_ino)
        attachment.original_mount = mount
        checked(['/usr/bin/mount', '-t', fs, '-o', 'nodev,nosuid,noexec', held_child, str(mount)], pass_fds=(child_fd,))
        attachment.mount = mount
        marker = mount / '.tilo-original-source-qualification'
        marker.write_bytes(b'disposable-original-source-only')
        os.chmod(marker, 0o600)
        checked(['/usr/bin/sync', '-f', str(mount)])
        yield OwnedSource(attachment, child, mount, disk, partition, fs)
    finally:
        # Completion/adoption alone cannot retire a receiver's source descriptors.
        # The owning fixture must observe its actual root children and proofs.
        verify_consumers_retired()
        assert_children_retired()
        attachment.cleanup()
        # Image is deleted ONLY once exact owned device/process release succeeds.
        image.unlink()
        (stage / 'release-complete').write_text('tilo-owned-gpt-release-v1\n')
        os.close(namespace)



class OwnedSource:
    """Observed paths/identities only; never grants or fabricated installer facts."""
    def __init__(self, attachment, partition, mount, disk_guid, partition_guid, filesystem):
        self.attachment = attachment
        self.disk = attachment.device
        self.partition = partition
        self.mount = mount
        self.disk_guid = disk_guid
        self.partition_guid = partition_guid
        self.source_epoch_retired = False
        self.home = mount / 'home/source'
        self.system_store = mount / 'var/lib/tilo-imager/home-capture'
        self.filesystem = filesystem

    def verify(self):
        self.attachment.verify()
        descriptor = self.attachment.partition_fd
        if descriptor is None:
            raise RuntimeError('owned source partition custody missing')
        held = os.fstat(descriptor)
        named = self.partition.stat(follow_symlinks=False)
        if not stat.S_ISBLK(named.st_mode) or named.st_rdev != held.st_rdev:
            raise RuntimeError('owned source partition identity changed')
        if self.attachment.mount is None:
            # Only the explicit observed retirement transition removes this
            # known mount. A vanished/replaced live mount never enters it.
            if not self.source_epoch_retired or os.path.lexists(self.mount):
                raise RuntimeError('retired original source mount absence is unproven')
            return
        source = checked(['/usr/bin/findmnt', '-n', '-o', 'SOURCE', '--target', str(self.mount)]).decode().strip()
        mount_type = checked(['/usr/bin/findmnt', '-n', '-o', 'FSTYPE', '--target', str(self.mount)]).decode().strip()
        if (source != str(self.partition) or mount_type != self.filesystem
                or (self.filesystem == 'ext4' and self.mount.stat().st_dev != held.st_rdev)):
            raise RuntimeError('owned source mounted partition identity changed')

    def retire_source_epoch(self, verify_source_epoch_retired):
        if not callable(verify_source_epoch_retired) or self.source_epoch_retired:
            raise RuntimeError('fresh actual source epoch retirement observer required')
        self.verify()
        verify_source_epoch_retired()
        assert_children_retired()
        old = self.attachment.partition_fd
        descriptor = os.open(self.partition, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
        try:
            if os.fstat(descriptor).st_rdev != os.fstat(old).st_rdev:
                raise RuntimeError('source epoch readonly partition pin changed')
            self.attachment.unmount_source()
            os.close(old)
            self.attachment.partition_fd = descriptor
        except BaseException:
            os.close(descriptor)
            raise
        self.source_epoch_retired = True
        if fcntl.fcntl(descriptor, fcntl.F_GETFL) & os.O_ACCMODE != os.O_RDONLY:
            raise RuntimeError('source epoch partition pin is not readonly')
        self.verify()
        (self.attachment.scratch / 'source-epoch-retired.json').write_text(json.dumps({
            'schema': 1, 'original_mount': str(self.mount),
            'partition_rdev': os.fstat(descriptor).st_rdev,
            'old_source_mount_absent': True,
            'partition_pin_readonly': True,
        }))

    @contextmanager
    def prepare_receiving_extent(self, attested, evidence, retirement_probe):
        # Only the private observation returned by the actual native package
        # authenticator is accepted; this object itself grants no source access.
        from delivery import AuthenticatedStagingPlan
        if (not isinstance(attested, AuthenticatedStagingPlan)
                or not callable(retirement_probe) or not self.source_epoch_retired):
            raise RuntimeError('native-authenticated Plan and retired original epoch required')
        self.verify()
        plan = attested.plan
        canonical = json.dumps(plan, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode()
        if hashlib.sha256(canonical).hexdigest() != attested.scope['staging_plan_sha256']:
            raise RuntimeError('authenticated Plan changed before physical staging')
        disk = plan['basis']['disk']
        if (disk['id'] != str(self.disk) or disk['guid'].lower() != self.disk_guid.lower()
                or disk['size_bytes'] != SIZE or disk['sector_bytes'] != 512 or plan['shrink'] is not None):
            raise RuntimeError('authenticated Plan does not select unchanged owned source disk')
        table = json.loads(checked(['/usr/bin/sfdisk', '--json', str(self.disk)]))['partitiontable']
        parts = table['partitions']
        if table['id'].lower() != self.disk_guid.lower() or len(parts) != 1:
            raise RuntimeError('original GPT changed before planned staging')
        original = parts[0]
        originals = disk['partitions']
        if (len(originals) != 1 or original['node'] != str(self.partition)
                or original['uuid'].lower() != self.partition_guid.lower()
                or originals[0]['number'] != 1 or originals[0]['guid'].lower() != self.partition_guid.lower()
                or original['start'] != originals[0]['first_lba']
                or original['start'] + original['size'] - 1 != originals[0]['last_lba']
                or original['type'].lower() != originals[0]['type_guid'].lower()):
            raise RuntimeError('authenticated original partition differs from actual GPT')
        old_header = os.pread(self.attachment.partition_fd, 16384, 0)
        if len(old_header) != 16384:
            raise RuntimeError('original filesystem byte observation incomplete')
        extents = []
        seen = {self.partition_guid.lower()}
        for key, kind in [('staging_esp', 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'),
                          ('staging_root', '0fc63daf-8483-4772-8e79-3d69d8477de4')]:
            item = plan[key]
            first, last, guid = item['first_lba'], item['last_lba'], item['partition_guid']
            if (type(first) is not int or type(last) is not int or first > last
                    or first < disk['first_usable_lba'] or last > disk['last_usable_lba']
                    or not isinstance(guid, str) or str(uuid.UUID(guid)) != guid.lower()
                    or uuid.UUID(guid).int == 0 or guid.lower() in seen
                    or not (last < original['start'] or first >= original['start'] + original['size'])):
                raise RuntimeError('native planned staging extent/GUID is invalid or overlaps original')
            if any(not (last < a or first > b) for a, b, _, _ in extents):
                raise RuntimeError('native staging extents overlap')
            seen.add(guid.lower())
            extents.append((first, last, guid, kind))
        lines = ''.join(f'start={a},size={b-a+1},type={kind},uuid={guid}\n'
                        for a, b, guid, kind in extents)
        input_fd = os.memfd_create('actual-native-staging-gpt', os.MFD_CLOEXEC)
        try:
            os.write(input_fd, lines.encode())
            os.lseek(input_fd, 0, os.SEEK_SET)
            code, _, error = bounded(['/usr/bin/sfdisk', '--append', '--no-reread', '--no-tell-kernel',
                                      '--lock=no', '--wipe=never', '--wipe-partitions=never', str(self.disk)],
                                     stdin=input_fd, timeout=30, limit=81920)
            if code:
                raise RuntimeError('actual native planned GPT append failed: ' + error[:512].decode(errors='replace'))
        finally:
            os.close(input_fd)
        checked(['/usr/bin/partx', '--add', '--nr', '2:3', str(self.disk)])
        self.verify()
        after = json.loads(checked(['/usr/bin/sfdisk', '--json', str(self.disk)]))['partitiontable']
        if after['id'] != table['id'] or len(after['partitions']) != 3 or after['partitions'][0] != original:
            raise RuntimeError('planned staging changed original GPT partition')
        if os.pread(self.attachment.partition_fd, 16384, 0) != old_header:
            raise RuntimeError('planned staging changed original filesystem bytes')
        for number, (first, last, guid, kind) in enumerate(extents, 2):
            item = after['partitions'][number-1]
            if (item['node'] != str(self.disk) + 'p' + str(number) or item['start'] != first
                    or item['size'] != last-first+1 or item['uuid'].lower() != guid.lower()
                    or item['type'].lower() != kind):
                raise RuntimeError('actual staging GPT differs from authenticated Plan')
        partition = Path(str(self.disk) + 'p3')
        end = time.monotonic() + 10
        while not partition.exists():
            if time.monotonic() >= end:
                raise RuntimeError('actual planned receiving partition node unavailable')
            time.sleep(.05)
        descriptor = os.open(partition, os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
        mount = None
        target = PlannedReceiving(self, partition, descriptor, None, None)
        _LIVE_RECEIVING[id(target)] = target
        try:
            metadata = os.fstat(descriptor)
            expected_dev = limited_read(Path('/sys/class/block') / partition.name / 'dev').strip().decode()
            if (not stat.S_ISBLK(metadata.st_mode) or expected_dev != f'{os.major(metadata.st_rdev)}:{os.minor(metadata.st_rdev)}'
                    or metadata.st_rdev == os.fstat(self.attachment.partition_fd).st_rdev):
                raise RuntimeError('planned receiving block identity is unavailable or not disjoint')
            held_path = f'/proc/self/fd/{descriptor}'
            checked(['/usr/bin/mkfs.btrfs', '-q', '-f', held_path], pass_fds=(descriptor,))
            mount = Path('/var/tmp') / f'tilo-installer-receiving-{uuid.uuid4()}'
            mount.mkdir(mode=0o700)
            origin = mount.stat(follow_symlinks=False)
            checked(['/usr/bin/mount', '-t', 'btrfs', '-o', 'nodev,nosuid', held_path, str(mount)], pass_fds=(descriptor,))
            target.mount = mount
            target.origin = origin
            target.system_store = mount / 'var/lib/tilo-imager/home-capture'
            target.verify()
            yield target
        finally:
            retirement_probe()
            assert_children_retired()
            if target.mount is not None:
                target.verify()
                checked(['/usr/bin/umount', str(mount)])
                restored = mount.stat(follow_symlinks=False)
                if (restored.st_dev, restored.st_ino) != (origin.st_dev, origin.st_ino):
                    raise RuntimeError('planned receiving mountpoint changed during cleanup')
                mount.rmdir()
            os.close(descriptor)
            _LIVE_RECEIVING.pop(id(target), None)


class PlannedReceiving:
    def __init__(self, source, partition, descriptor, mount, origin):
        self.source = source
        self.partition = partition
        self.descriptor = descriptor
        self.mount = mount
        self.origin = origin
        self.filesystem = 'btrfs'
        self.system_store = mount / 'var/lib/tilo-imager/home-capture' if mount else None

    def verify(self):
        self.source.verify()
        held = os.fstat(self.descriptor)
        named = self.partition.stat(follow_symlinks=False)
        if not stat.S_ISBLK(named.st_mode) or named.st_rdev != held.st_rdev:
            raise RuntimeError('actual planned receiving partition identity changed')
        fields = checked(['/usr/bin/findmnt', '-n', '-o', 'SOURCE,FSTYPE', '--target', str(self.mount)]).decode().split()
        if fields != [str(self.partition), 'btrfs']:
            raise RuntimeError('actual planned receiving mount changed')


def immutable_tools():
    version = checked(['/usr/bin/qemu-nbd', '--version'])
    if not version.startswith(b'qemu-nbd 10.2.2 '):
        raise RuntimeError('foreground QEMU semantics require the independently reviewed installed10.2.2 tool')
    result = {'qemu_version': version.decode('utf-8', errors='strict').splitlines()[0]}
    for name in ('qemu-nbd', 'sfdisk', 'mkfs.ext4', 'mkfs.btrfs', 'partx', 'lsblk', 'mount', 'umount', 'findmnt', 'sync'):
        path = Path('/usr/bin') / name
        resolved = path.resolve(strict=True)
        for component in (resolved, *resolved.parents):
            info = component.stat(follow_symlinks=False)
            if info.st_uid or info.st_mode & 0o022:
                raise RuntimeError('native source tool is not root-controlled')
        if not resolved.is_file():
            raise RuntimeError('native source tool is not a regular file')
        result[name] = {'path': str(path), 'sha256': file_hash(resolved)}
    return {'schema': 1, 'tools': result}

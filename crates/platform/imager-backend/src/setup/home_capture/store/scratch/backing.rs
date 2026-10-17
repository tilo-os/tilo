//! Retained kernel block topology for an already granted writable directory.
//! This excludes observable kernel RAM/loop storage, not a hypervisor's backend.
mod dm;
use crate::setup::migration_durability;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
};
const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 32;
const FS_INFO: libc::c_ulong = 0x8400_941f;
const DISKSEQ: libc::c_ulong = 0x8008_1280;
#[repr(C)]
struct FsInfo {
    max_id: u64,
    devices: u64,
    fsid: [u8; 16],
    remaining: [u8; 992],
}
const _: () = assert!(std::mem::size_of::<FsInfo>() == 1024);
#[derive(Clone, Debug, PartialEq, Eq)]
struct Members {
    filesystem: Option<[u8; 16]>,
    devices: Vec<u64>,
    device_ids: Vec<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeFacts {
    directory: (u64, u64),
    device: u64,
    sequence: u64,
    dependencies: Vec<u64>,
    mapper: Option<dm::DmState>,
}
struct Node {
    directory: File,
    block: File,
    facts: NodeFacts,
}
struct Observation {
    members: Members,
    nodes: BTreeMap<u64, Node>,
}
pub struct BackingWitness {
    directory: File,
    identity: (u64, u64),
    mount: u64,
    filesystem: libc::c_long,
    sysfs: File,
    control: Option<File>,
    observation: Observation,
}
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn magic(file: &File) -> io::Result<libc::c_long> {
    let mut value = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(file.as_raw_fd(), value.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { value.assume_init() }.f_type)
}
fn identity(file: &File) -> io::Result<(u64, u64)> {
    let facts = file.metadata()?;
    Ok((facts.dev(), facts.ino()))
}
fn kernel_directory(root: &File, path: &Path) -> io::Result<File> {
    let file = cap_primitives::fs::open_dir(root, path)?;
    if magic(&file)? != libc::SYSFS_MAGIC || file.metadata()?.uid() != 0 {
        return Err(refused("Backing topology is not kernel sysfs"));
    }
    Ok(file)
}
fn kernel_relative(directory: &File) -> io::Result<PathBuf> {
    let path = std::fs::read_link(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
    path.strip_prefix("/sys")
        .map(Path::to_path_buf)
        .map_err(|_| refused("Backing kernel path escaped sysfs"))
}
fn kernel_child(sysfs: &File, directory: &File, path: &Path) -> io::Result<File> {
    kernel_directory(sysfs, &kernel_relative(directory)?.join(path))
}
fn direct_leaf(sysfs: &File, directory: &File) -> io::Result<()> {
    let relative = kernel_relative(directory)?;
    let path = Path::new("/sys").join(&relative);
    if !(path.starts_with("/sys/devices/pci0000:")
        || path
            .to_str()
            .is_some_and(|v| v.starts_with("/sys/devices/pci"))
        || path.starts_with("/sys/devices/platform"))
    {
        return Err(refused("Backing device is not a supported disk leaf"));
    }
    let mut ancestor = relative.as_path();
    for _ in 0..MAX_DEPTH {
        match kernel_directory(sysfs, &ancestor.join("driver")) {
            Ok(driver) => {
                let driver = kernel_relative(&driver)?;
                if [
                    "bus/pci/drivers/nvme",
                    "bus/scsi/drivers/sd",
                    "bus/virtio/drivers/virtio_blk",
                    "bus/mmc/drivers/mmcblk",
                ]
                .iter()
                .any(|name| driver == Path::new(name))
                {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        ancestor = ancestor
            .parent()
            .filter(|parent| parent.starts_with("devices"))
            .ok_or_else(|| refused("Backing disk driver unavailable"))?;
    }
    Err(refused("Backing disk driver exceeds bound"))
}
fn attribute(directory: &File, name: &str) -> io::Result<String> {
    let name = std::ffi::CString::new(name).map_err(io::Error::other)?;
    let raw = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(raw) };
    if magic(&file)? != libc::SYSFS_MAGIC {
        return Err(refused("Backing attribute is not kernel sysfs"));
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(refused("Backing attribute exceeds bound"));
    }
    String::from_utf8(bytes)
        .map(|value| value.trim().to_owned())
        .map_err(io::Error::other)
}
fn device_number(text: &str) -> io::Result<u64> {
    let (major, minor) = text
        .split_once(':')
        .ok_or_else(|| refused("Backing device number invalid"))?;
    let major = major.parse::<u32>().map_err(io::Error::other)?;
    let minor = minor.parse::<u32>().map_err(io::Error::other)?;
    let device = libc::makedev(major, minor);
    if libc::major(device) != major || libc::minor(device) != minor {
        return Err(refused("Backing device number overflow"));
    }
    Ok(device)
}
fn number(device: u64) -> String {
    format!("{}:{}", libc::major(device), libc::minor(device))
}
fn bounded_names(directory: &File) -> io::Result<Vec<std::ffi::OsString>> {
    let mut names = Vec::new();
    for entry in cap_primitives::fs::read_base_dir(directory)? {
        if names.len() == MAX_NODES {
            return Err(refused("Backing topology exceeds bound"));
        }
        names.push(entry?.file_name());
    }
    names.sort();
    Ok(names)
}
fn fs_info(directory: &File) -> io::Result<FsInfo> {
    let mut info: FsInfo = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(directory.as_raw_fd(), FS_INFO, &raw mut info) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if info.devices == 0 || info.devices > MAX_NODES as u64 {
        return Err(refused("Backing Btrfs members unavailable or exceed bound"));
    }
    Ok(info)
}
fn members(directory: &File, sysfs: &File, filesystem: libc::c_long) -> io::Result<Members> {
    if filesystem != libc::BTRFS_SUPER_MAGIC {
        return Ok(Members {
            filesystem: None,
            devices: vec![directory.metadata()?.dev()],
            device_ids: Vec::new(),
        });
    }
    let before = fs_info(directory)?;
    let root = kernel_directory(
        sysfs,
        &PathBuf::from("fs/btrfs").join(uuid::Uuid::from_bytes(before.fsid).to_string()),
    )?;
    let devinfo = kernel_child(sysfs, &root, Path::new("devinfo"))?;
    let mut ids = Vec::new();
    for name in bounded_names(&devinfo)? {
        let id = name
            .to_str()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|id| *id != 0)
            .ok_or_else(|| refused("Backing Btrfs member ID invalid"))?;
        let info = kernel_child(sysfs, &devinfo, Path::new(&name))?;
        if attribute(&info, "missing")? != "0"
            || attribute(&info, "replace_target")? != "0"
            || attribute(&info, "writeable")? != "1"
            || attribute(&info, "in_fs_metadata")? != "1"
            || attribute(&info, "fsid")? != uuid::Uuid::from_bytes(before.fsid).to_string()
        {
            return Err(refused(
                "Backing Btrfs member is missing, replaced or ambiguous",
            ));
        }
        ids.push(id);
    }
    ids.sort_unstable();
    let devices = kernel_child(sysfs, &root, Path::new("devices"))?;
    let mut numbers = Vec::new();
    for name in bounded_names(&devices)? {
        let member = kernel_child(sysfs, &devices, Path::new(&name))?;
        numbers.push(device_number(&attribute(&member, "dev")?)?);
    }
    numbers.sort_unstable();
    numbers.dedup();
    let after = fs_info(directory)?;
    if before.fsid != after.fsid
        || before.devices != after.devices
        || before.max_id != after.max_id
        || ids.len() != before.devices as usize
        || numbers.len() != ids.len()
    {
        return Err(refused("Backing Btrfs membership changed or incomplete"));
    }
    Ok(Members {
        filesystem: Some(before.fsid),
        devices: numbers,
        device_ids: ids,
    })
}
fn block_pin(directory: &File, device: u64) -> io::Result<File> {
    let uevent = attribute(directory, "uevent")?;
    let names = uevent
        .lines()
        .filter_map(|line| line.strip_prefix("DEVNAME="))
        .collect::<Vec<_>>();
    let [name] = names.as_slice() else {
        return Err(refused("Backing kernel block name unavailable"));
    };
    let path = Path::new(name);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(refused("Backing kernel block name invalid"));
    }
    let dev = cap_primitives::fs::open_ambient_dir(
        Path::new("/dev"),
        cap_primitives::ambient_authority(),
    )?;
    let metadata = dev.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(refused("Backing block namespace is not system-owned"));
    }
    // /dev/dm-N and /dev/nvme... are real endpoints. Root retains a read-only
    // descriptor; an ordinary caller pins the same node without reading payload.
    let name = std::ffi::CString::new(name.as_bytes()).map_err(io::Error::other)?;
    let access = if rustix::process::geteuid().as_raw() == 0 {
        libc::O_RDONLY | libc::O_NONBLOCK
    } else {
        libc::O_PATH
    };
    let raw = unsafe {
        libc::openat(
            dev.as_raw_fd(),
            name.as_ptr(),
            access | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(raw) };
    let facts = file.metadata()?;
    if !facts.file_type().is_block_device() || facts.rdev() != device {
        return Err(refused("Backing actual block descriptor differs"));
    }
    Ok(file)
}
fn sequence(directory: &File, block: &File) -> io::Result<u64> {
    let value = attribute(directory, "diskseq")?
        .parse::<u64>()
        .map_err(io::Error::other)?;
    if value == 0 {
        return Err(refused("Backing disk sequence missing"));
    }
    if rustix::process::geteuid().as_raw() == 0 {
        let mut actual = 0u64;
        if unsafe { libc::ioctl(block.as_raw_fd(), DISKSEQ, &raw mut actual) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if value != actual {
            return Err(refused("Backing disk sequence differs"));
        }
    }
    Ok(value)
}
fn collect(
    sysfs: &File,
    control: &mut Option<File>,
    device: u64,
    depth: usize,
    visiting: &mut Vec<u64>,
    nodes: &mut BTreeMap<u64, Node>,
) -> io::Result<()> {
    if visiting.contains(&device) {
        return Err(refused("Backing topology cycle"));
    }
    if nodes.contains_key(&device) {
        return Ok(());
    }
    if depth == MAX_DEPTH || nodes.len() + visiting.len() >= MAX_NODES {
        return Err(refused("Backing topology exceeds bound"));
    }
    visiting.push(device);
    let directory = kernel_directory(sysfs, &PathBuf::from("dev/block").join(number(device)))?;
    if device_number(&attribute(&directory, "dev")?)? != device {
        return Err(refused("Backing kernel node differs"));
    }
    let block = block_pin(&directory, device)?;
    let partition = match attribute(&directory, "partition") {
        Ok(value) => {
            value.parse::<u32>().map_err(io::Error::other)?;
            true
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    let parent = if partition {
        Some(kernel_directory(
            sysfs,
            kernel_relative(&directory)?
                .parent()
                .ok_or_else(|| refused("Backing partition parent unavailable"))?,
        )?)
    } else {
        None
    };
    // Partition diskseq belongs to its whole disk, observed through held parent.
    let seq = sequence(parent.as_ref().unwrap_or(&directory), &block)?;
    let mut dependencies = Vec::new();
    let mut mapper = None;
    if let Some(parent) = &parent {
        dependencies.push(device_number(&attribute(parent, "dev")?)?);
    } else {
        let slaves = kernel_child(sysfs, &directory, Path::new("slaves"))?;
        for name in bounded_names(&slaves)? {
            let slave = kernel_child(sysfs, &slaves, Path::new(&name))?;
            dependencies.push(device_number(&attribute(&slave, "dev")?)?);
        }
        match kernel_child(sysfs, &directory, Path::new("dm")) {
            Ok(_) => {
                if control.is_none() {
                    let raw = unsafe {
                        libc::open(
                            c"/dev/mapper/control".as_ptr(),
                            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                        )
                    };
                    if raw < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let file = unsafe { File::from_raw_fd(raw) };
                    let facts = file.metadata()?;
                    if !facts.file_type().is_char_device()
                        || facts.uid() != 0
                        || facts.mode() & 0o022 != 0
                    {
                        return Err(refused("Backing mapper control differs"));
                    }
                    *control = Some(file);
                }
                let state = dm::inspect(
                    control
                        .as_ref()
                        .ok_or_else(|| refused("Backing mapper control missing"))?,
                    device,
                )?;
                dependencies.sort_unstable();
                dependencies.dedup();
                if dependencies != state.dependencies {
                    return Err(refused("Backing mapper dependencies differ"));
                }
                mapper = Some(state);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Closed direct-leaf classification. Kernel virtual block drivers
                // can store data in RAM despite presenting a block endpoint.
                if !dependencies.is_empty() {
                    return Err(refused("Backing stacked device type is not supported"));
                }
                direct_leaf(sysfs, &directory)?;
            }
            Err(error) => return Err(error),
        }
    }
    dependencies.sort_unstable();
    dependencies.dedup();
    if partition && dependencies.len() != 1 {
        return Err(refused("Backing partition parent missing"));
    }
    for dependency in &dependencies {
        collect(sysfs, control, *dependency, depth + 1, visiting, nodes)?;
    }
    if let Some(state) = &mapper {
        if dm::inspect(
            control
                .as_ref()
                .ok_or_else(|| refused("Backing mapper control missing"))?,
            device,
        )? != *state
        {
            return Err(refused("Backing mapper table changed"));
        }
    }
    if sequence(parent.as_ref().unwrap_or(&directory), &block)? != seq {
        return Err(refused("Backing disk changed during inspection"));
    }
    visiting.pop();
    nodes.insert(
        device,
        Node {
            facts: NodeFacts {
                directory: identity(&directory)?,
                device,
                sequence: seq,
                dependencies,
                mapper,
            },
            directory,
            block,
        },
    );
    Ok(())
}
fn observe(
    directory: &File,
    sysfs: &File,
    filesystem: libc::c_long,
    control: &mut Option<File>,
) -> io::Result<Observation> {
    let members = members(directory, sysfs, filesystem)?;
    let mut nodes = BTreeMap::new();
    for device in &members.devices {
        collect(sysfs, control, *device, 0, &mut Vec::new(), &mut nodes)?;
    }
    if self::members(directory, sysfs, filesystem)? != members {
        return Err(refused("Backing members changed during inspection"));
    }
    Ok(Observation { members, nodes })
}
impl BackingWitness {
    pub fn review(directory: &File) -> io::Result<Self> {
        let directory = migration_durability::readable_directory(directory)?;
        let filesystem = magic(&directory)?;
        if !matches!(
            filesystem,
            libc::BTRFS_SUPER_MAGIC | libc::EXT4_SUPER_MAGIC | libc::XFS_SUPER_MAGIC
        ) {
            return Err(refused("Backing filesystem is unsupported"));
        }
        let mut capacity: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(directory.as_raw_fd(), &raw mut capacity) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if capacity.f_flag & libc::ST_RDONLY != 0 {
            return Err(refused("Backing granted filesystem is readonly"));
        }
        let sysfs = File::open("/sys")?;
        if magic(&sysfs)? != libc::SYSFS_MAGIC || sysfs.metadata()?.uid() != 0 {
            return Err(refused("Backing kernel topology unavailable"));
        }
        let mount = fs_native::snapshot_lease::mount_id(&directory)?;
        let mut control = None;
        let observation = observe(&directory, &sysfs, filesystem, &mut control)?;
        let value = Self {
            identity: identity(&directory)?,
            directory,
            mount,
            filesystem,
            sysfs,
            control,
            observation,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn validate_directory(&self, directory: &File) -> io::Result<()> {
        if identity(directory)? != self.identity
            || fs_native::snapshot_lease::mount_id(directory)? != self.mount
        {
            return Err(refused("Backing granted directory differs"));
        }
        self.validate()
    }
    pub fn validate(&self) -> io::Result<()> {
        if identity(&self.directory)? != self.identity
            || fs_native::snapshot_lease::mount_id(&self.directory)? != self.mount
            || magic(&self.directory)? != self.filesystem
            || magic(&self.sysfs)? != libc::SYSFS_MAGIC
        {
            return Err(refused("Backing held namespace changed"));
        }
        let mut control = self.control.as_ref().map(File::try_clone).transpose()?;
        let current = observe(&self.directory, &self.sysfs, self.filesystem, &mut control)?;
        if current.members != self.observation.members
            || current.nodes.len() != self.observation.nodes.len()
        {
            return Err(refused("Backing topology changed"));
        }
        for (device, node) in &self.observation.nodes {
            let actual = current
                .nodes
                .get(device)
                .ok_or_else(|| refused("Backing device disappeared"))?;
            if actual.facts != node.facts
                || identity(&node.directory)? != node.facts.directory
                || node.block.metadata()?.rdev() != *device
            {
                return Err(refused("Backing retained device differs"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_numbers_require_complete_checked_kernel_pairs() {
        let value = device_number("259:1048575").unwrap();
        assert_eq!(libc::major(value), 259);
        assert_eq!(libc::minor(value), 1048575);
        for invalid in [
            "",
            "259",
            "259:1:2",
            "-1:0",
            "0:-1",
            "4294967296:0",
            "0:4294967296",
        ] {
            assert!(device_number(invalid).is_err());
        }
    }
    #[cfg(feature = "native-capture-qualification")]
    #[test]
    fn actual_held_writable_store_backing_and_directory_identity_are_observed() {
        use std::os::unix::fs::PermissionsExt;
        let store = PathBuf::from(
            std::env::var_os("TILO_FS_NATIVE_STORE")
                .expect("observable supported block-backed disposable store required"),
        );
        let temporary = tempfile::Builder::new()
            .prefix("inventory-backing-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(store)
            .unwrap();
        let directory = File::open(temporary.path()).unwrap();
        let backing = BackingWitness::review(&directory).unwrap();
        backing.validate_directory(&directory).unwrap();
        std::fs::create_dir(temporary.path().join("different")).unwrap();
        let different = File::open(temporary.path().join("different")).unwrap();
        assert!(backing.validate_directory(&different).is_err());
        backing.validate().unwrap();
        let ram = File::open("/dev/shm").expect("observable native tmpfs required");
        assert_eq!(magic(&ram).unwrap(), libc::TMPFS_MAGIC);
        assert!(BackingWitness::review(&ram).is_err());
    }
}

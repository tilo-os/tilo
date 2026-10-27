//! Actual fixed-format capacity proof; explicit native qualification only.
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const CHILD: &str = "TILO_NATIVE_MIGRATION_FORMAT_CHILD";
const TEST: &str = "media_layout::native_capacity_tests::fixed_migration_ext4_floor_matches_actual_native_capacity";
const LOOP_GET_STATUS64: libc::c_ulong = 0x4c05;
const LOOP_CLR_FD: libc::c_ulong = 0x4c01;
// Exact Linux UAPI loop_info64 from include/uapi/linux/loop.h.
#[repr(C)]
struct LoopInfo {
    device: u64,
    inode: u64,
    rdevice: u64,
    offset: u64,
    size_limit: u64,
    number: u32,
    encryption: u32,
    key_size: u32,
    flags: u32,
    filename: [u8; 64],
    crypt_name: [u8; 64],
    key: [u8; 32],
    init: [u64; 2],
}
fn failure(message: &str) -> io::Error {
    io::Error::other(message)
}
fn tool(program: &str) -> io::Result<Command> {
    let tool = block_devices::system_tool(program);
    if !Path::new(&tool).is_absolute() {
        return Err(failure("required canonical native tool is missing"));
    }
    let mut command = Command::new(tool);
    command.env("LC_ALL", "C");
    Ok(command)
}
fn run(command: &mut Command) -> io::Result<Vec<u8>> {
    let mut output = tempfile::tempfile()?;
    let mut diagnostics = tempfile::tempfile()?;
    command
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(diagnostics.try_clone()?));
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let killed = child.kill();
            let waited = child.wait();
            killed?;
            waited?;
            return Err(failure("native fixture tool exceeded sixty seconds"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    output.seek(SeekFrom::Start(0))?;
    diagnostics.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    let mut stderr = Vec::new();
    output.take(32 * 1024 + 1).read_to_end(&mut bytes)?;
    diagnostics.take(32 * 1024 + 1).read_to_end(&mut stderr)?;
    if bytes.len() > 32 * 1024 || stderr.len() > 32 * 1024 {
        return Err(failure("native tool output exceeded per-stream bound"));
    }
    if !status.success() {
        return Err(io::Error::other(format!(
            "native tool failed ({status}); stdout: {}; stderr: {}",
            String::from_utf8_lossy(&bytes),
            String::from_utf8_lossy(&stderr),
        )));
    }
    if !stderr.is_empty() {
        eprintln!("native tool stderr: {}", String::from_utf8_lossy(&stderr));
    }
    Ok(bytes)
}
fn info(file: &File) -> io::Result<LoopInfo> {
    let mut value = std::mem::MaybeUninit::<LoopInfo>::uninit();
    // SAFETY: exact Linux UAPI structure, live block FD and writable output.
    if unsafe { libc::ioctl(file.as_raw_fd(), LOOP_GET_STATUS64, value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful kernel ioctl initialized the entire output.
    Ok(unsafe { value.assume_init() })
}
struct Disposable {
    root: Option<tempfile::TempDir>,
    image: PathBuf,
    mount: PathBuf,
    backing: File,
    held: Option<File>,
    device: Option<String>,
    number: Option<u64>,
    mounted: bool,
    allocation_started: bool,
    construction_finished: bool,
    released: bool,
}
impl Disposable {
    fn new(extent: u64) -> io::Result<Self> {
        let store = std::env::var_os("TILO_FS_NATIVE_STORE")
            .ok_or_else(|| failure("independently supplied native store is required"))?;
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(store)?;
        let image = root.path().join("migration.ext4");
        let backing = File::options()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&image)?;
        backing.set_len(extent)?;
        let mount = root.path().join("mount");
        fs::create_dir(&mount)?;
        let mut fixture = Self {
            root: Some(root),
            image,
            mount,
            backing,
            held: None,
            device: None,
            number: None,
            mounted: false,
            allocation_started: false,
            construction_finished: false,
            released: false,
        };
        fixture.receipt("allocation-pending")?;
        let mut allocation = tool("losetup")?;
        allocation
            .args(["--find", "--show", "--"])
            .arg(&fixture.image);
        // A failed spawn/result cannot prove that allocation never occurred.
        // Retain backing evidence from this point until exact detach is proven.
        fixture.allocation_started = true;
        fixture.receipt("allocation-unknown")?;
        let output = run(&mut allocation)?;
        let device = String::from_utf8(output)
            .map_err(io::Error::other)?
            .trim()
            .to_owned();
        fixture.device = Some(device.clone());
        fixture.receipt("allocated-unattested")?;
        let suffix = device
            .strip_prefix("/dev/loop")
            .ok_or_else(|| failure("unexpected loop allocation"))?;
        if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(failure("invalid loop allocation"));
        }
        let held = File::open(&device)?;
        let metadata = held.metadata()?;
        if !metadata.file_type().is_block_device() {
            return Err(failure("allocated loop is not a block FD"));
        }
        fixture.number = Some(metadata.rdev());
        fixture.held = Some(held);
        fixture.verify_custody()?;
        fixture.receipt("attested-before-format")?;
        let mut args = block_devices::migration_ext4::MKFS_ARGS.to_vec();
        args.extend(["-q", "-L", "Tilo migration"]);
        fixture.device_command("mkfs.ext4", &args, None)?;
        fixture.device_command(
            "mount",
            &[
                "--no-canonicalize",
                "-t",
                "ext4",
                "-o",
                "nodev,nosuid,noexec",
                "--",
            ],
            Some(&fixture.mount),
        )?;
        fixture.mounted = true;
        fixture.receipt("mounted")?;
        fixture.construction_finished = true;
        Ok(fixture)
    }
    fn receipt(&self, phase: &str) -> io::Result<()> {
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| failure("fixture root custody lost"))?;
        let metadata = self.backing.metadata()?;
        let bytes = serde_json::to_vec(
            &serde_json::json!({"schema":1,"phase":phase,"image":"migration.ext4","mount":"mount","device":self.device,"rdev":self.number,"backing_dev":metadata.dev(),"backing_inode":metadata.ino(),"allocation_started":self.allocation_started,"construction_finished":self.construction_finished}),
        )?;
        let path = root.path().join("recovery.json");
        let mut file = File::options()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        fs::set_permissions(
            root.path().join("recovery.json"),
            fs::Permissions::from_mode(0o600),
        )?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        File::open(root.path())?.sync_all()
    }
    fn verify_custody(&self) -> io::Result<()> {
        let held = self
            .held
            .as_ref()
            .ok_or_else(|| failure("allocated loop has no held authority"))?;
        let value = info(held)?;
        let backing = self.backing.metadata()?;
        let path = fs::symlink_metadata(&self.image)?;
        if value.device != backing.dev()
            || value.inode != backing.ino()
            || value.offset != 0
            || value.size_limit != 0
            || value.encryption != 0
            || held.metadata()?.rdev()
                != self
                    .number
                    .ok_or_else(|| failure("loop device identity missing"))?
            || !path.is_file()
            || path.dev() != backing.dev()
            || path.ino() != backing.ino()
        {
            return Err(failure(
                "held loop does not refer to this exact owned backing inode",
            ));
        }
        Ok(())
    }
    fn device_command(&self, program: &str, args: &[&str], tail: Option<&Path>) -> io::Result<()> {
        self.verify_custody()?;
        let held = self
            .held
            .as_ref()
            .ok_or_else(|| failure("held loop authority missing"))?
            .try_clone()?;
        let fd = held.as_raw_fd();
        let mut command = tool(program)?;
        command.args(args).arg(format!("/proc/self/fd/{fd}"));
        if let Some(tail) = tail {
            command.arg(tail);
        }
        // SAFETY: after fork only async-signal-safe fcntl operates on the
        // retained duplicate. Parent's CLOEXEC and ownership remain unchanged.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        run(&mut command)?;
        self.verify_custody()
    }
    fn available(&self) -> io::Result<u64> {
        self.verify_custody()?;
        let file = File::open(&self.mount)?;
        let mut facts = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: live mounted directory FD and writable output.
        if unsafe { libc::fstatvfs(file.as_raw_fd(), facts.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatvfs initialized output.
        let facts = unsafe { facts.assume_init() };
        facts
            .f_bavail
            .checked_mul(facts.f_frsize)
            .ok_or_else(|| failure("native capacity overflow"))
    }
    fn close(&mut self) -> io::Result<()> {
        if self.released {
            return Ok(());
        }
        self.verify_custody()?;
        if self.mounted {
            run(tool("umount")?.arg("--").arg(&self.mount))?;
            self.mounted = false;
        }
        let held = self
            .held
            .as_ref()
            .ok_or_else(|| failure("held loop authority missing"))?;
        // SAFETY: detach only the verified retained owned loop FD, never a name.
        if unsafe { libc::ioctl(held.as_raw_fd(), LOOP_CLR_FD, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        drop(self.held.take());
        let named = File::open(
            self.device
                .as_ref()
                .ok_or_else(|| failure("loop locator missing"))?,
        )?;
        if named.metadata()?.rdev()
            != self
                .number
                .ok_or_else(|| failure("loop identity missing"))?
        {
            return Err(failure("loop changed before detach could be verified"));
        }
        match info(&named) {
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {}
            _ => return Err(failure("loop detach has not been independently verified")),
        }
        self.receipt("verified-detached")?;
        self.released = true;
        Ok(())
    }
}
impl Drop for Disposable {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if !self.allocation_started {
            // No allocation command was admitted, so removing our owned backing
            // cannot strand an allocated loop. Preserve the constructor error.
            return;
        }
        if let Err(error) = self.close() {
            let _recorded = self.receipt("cleanup-unproven");
            let retained = self.root.take().map(tempfile::TempDir::keep);
            eprintln!("native fixture cleanup unproven; retained {retained:?}: {error}");
            // Result-returning construction failures are not Rust unwinds.
            // Do not replace their original error with a secondary Drop panic.
            if self.construction_finished && !std::thread::panicking() {
                panic!("native fixture cleanup failed; owned backing retained");
            }
        }
    }
}
fn prove_extent(extent: u64) {
    let mut fixture = Disposable::new(extent).unwrap();
    let actual = fixture.available().unwrap();
    let floor = block_devices::migration_ext4::available_bytes(extent).unwrap();
    assert!(
        floor <= actual,
        "fixed format floor {floor} exceeds native available {actual} for {extent}"
    );
    let admitted =
        crate::setup::portable_home_media::payload_capacity_for_free_bytes(floor).unwrap();
    let actual_limit =
        crate::setup::portable_home_media::payload_capacity_for_free_bytes(actual).unwrap();
    assert!(admitted <= actual_limit);
    assert!(admitted < extent - 1);
    if extent == 64 * 1024 * 1024 {
        let file = File::options()
            .create_new(true)
            .write(true)
            .open(fixture.mount.join("sealed-allocation"))
            .unwrap();
        let limit = crate::setup::portable_transfer::Limits {
            plaintext_bytes: admitted,
            entries: 1,
            metadata_bytes: 1,
        };
        let sealed = limit.sealed_bound().unwrap();
        let length = libc::off_t::try_from(sealed).unwrap();
        // SAFETY: live owned fixture file and representable positive allocation.
        assert_eq!(
            unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, length) },
            0
        );
        file.sync_all().unwrap();
        assert_eq!(file.metadata().unwrap().len(), sealed);
        assert!(fixture.available().unwrap() >= 256 * 1024);
    }
    fixture.close().unwrap();
}
#[test]
fn fixed_migration_ext4_floor_matches_actual_native_capacity() {
    // SAFETY: read-only effective identity query.
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "explicit native fixture requires root/CAP_SYS_ADMIN"
    );
    if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("namespace")) {
        let output = run(Command::new("/usr/bin/unshare")
            .args(["--mount", "--propagation", "private", "--"])
            .arg(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--nocapture"])
            .env(CHILD, "namespace")
            .env(
                "TILO_NATIVE_MIGRATION_FORMAT_PARENT_NS",
                fs::read_link("/proc/self/ns/mnt").unwrap(),
            ))
        .unwrap();
        assert!(String::from_utf8_lossy(&output).contains("1 passed; 0 failed; 0 ignored"));
        return;
    }
    assert_ne!(
        fs::read_link("/proc/self/ns/mnt").unwrap(),
        PathBuf::from(
            std::env::var_os("TILO_NATIVE_MIGRATION_FORMAT_PARENT_NS")
                .expect("parent namespace proof required")
        )
    );
    for extent in [
        64 * 1024 * 1024,
        129 * 1024 * 1024 + 513,
        4 * 1024 * 1024 * 1024,
    ] {
        prove_extent(extent);
    }
    assert!(block_devices::migration_ext4::available_bytes(32 * 1024 * 1024).is_err());
}

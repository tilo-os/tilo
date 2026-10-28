//! Cache-layout facts from the genuine inherited disposable GPT descriptor.
//! This neither probes the boot disk nor claims that an OS has been installed.
use crate::setup::staging::{
    self,
    plan::{Basis, FirmwareFacts, Plan, Requirements},
};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn number(path: &Path) -> io::Result<u64> {
    let mut text = String::new();
    File::open(path)?.take(129).read_to_string(&mut text)?;
    if text.len() > 128 {
        return Err(denied());
    }
    text.trim().parse().map_err(io::Error::other)
}
fn firmware() -> io::Result<FirmwareFacts> {
    if !Path::new("/sys/firmware/efi").try_exists()? {
        return Ok(FirmwareFacts {
            uefi: false,
            secure_boot: false,
            boot_current: None,
            boot_order: vec![],
            boot_next: None,
        });
    }
    let read = |name: &str| -> io::Result<Option<Vec<u8>>> {
        let path = format!("/sys/firmware/efi/efivars/{name}-8be4df61-93ca-11d2-aa0d-00e098032b8c");
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if !file.metadata()?.is_file() {
            return Err(denied());
        }
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(denied());
        }
        staging::linux_parse::efivar_payload(&bytes)
            .map(Some)
            .map_err(io::Error::other)
    };
    let singleton = |bytes: Option<Vec<u8>>| -> io::Result<Option<String>> {
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let numbers = staging::efi::parse_u16_list(&bytes).map_err(io::Error::other)?;
        if numbers.len() != 1 {
            return Err(denied());
        }
        Ok(Some(staging::efi::format_boot_number(numbers[0])))
    };
    Ok(FirmwareFacts {
        uefi: true,
        secure_boot: read("SecureBoot")?.is_some_and(|bytes| bytes.first() == Some(&1)),
        boot_current: singleton(read("BootCurrent")?)?,
        boot_order: staging::efi::parse_u16_list(&read("BootOrder")?.unwrap_or_default())
            .map_err(io::Error::other)?
            .into_iter()
            .map(staging::efi::format_boot_number)
            .collect(),
        boot_next: singleton(read("BootNext")?)?,
    })
}
/// Never release an unobserved command child, including an error before its
/// stdout is extracted. The existing outer namespace watchdog owns unknown
/// cleanup; only an actual wait result can release this exact Child.
struct OwnedProbe {
    child: std::process::Child,
    status: Option<std::process::ExitStatus>,
}
impl OwnedProbe {
    fn observe(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status)
    }
}
impl Drop for OwnedProbe {
    fn drop(&mut self) {
        if self.status.is_some() {
            return;
        }
        // Signal only this still-owned Child once. Repeated unknown waits do
        // not repeatedly signal its numeric PID or imply a successful probe.
        let _ = self.child.kill();
        loop {
            if matches!(self.observe(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn dump(disk: &File) -> io::Result<String> {
    let tool = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/usr/sbin/sfdisk")?;
    let facts = tool.metadata()?;
    if !facts.is_file()
        || facts.uid() != 0
        || facts.mode() & 0o022 != 0
        || facts.mode() & 0o111 == 0
    {
        return Err(denied());
    }
    let duplicate = |file: &File| -> io::Result<File> {
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 256) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    };
    let held_tool = duplicate(&tool)?;
    let held_disk = duplicate(disk)?;
    let mut command = Command::new("/proc/self/fd/73");
    command
        .arg("--json")
        .arg("/proc/self/fd/74")
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .env_remove("LD_AUDIT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let raw = held_disk.as_raw_fd();
    let tool_raw = held_tool.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(tool_raw, 73) < 0
                || libc::fcntl(73, libc::F_SETFD, 0) < 0
                || libc::dup2(raw, 74) < 0
                || libc::fcntl(74, libc::F_SETFD, 0) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = OwnedProbe {
        child: command.spawn()?,
        status: None,
    };
    let mut output = child.child.stdout.take().ok_or_else(denied)?;
    let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
    let result = (|| {
        if flags < 0
            || unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(io::Error::last_os_error());
        }
        let deadline = tilo_test_env::hang_deadline();
        let mut bytes = Vec::new();
        let mut eof = false;
        loop {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut buffer = [0; 4096];
            match output.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(n) => {
                    if bytes.len() + n > 65536 {
                        return Err(denied());
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
            if let Some(status) = child.observe()? {
                if !status.success() {
                    return Err(denied());
                }
                if eof {
                    break;
                }
            }
            let mut poll = libc::pollfd {
                fd: output.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&raw mut poll, 1, 10) } < 0
                && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
            {
                return Err(io::Error::last_os_error());
            }
        }
        String::from_utf8(bytes).map_err(io::Error::other)
    })();
    // Successful completion already includes real try_wait/reap AND bounded
    // output EOF. Every error remains owned by the cleanup guard above.
    result
}
pub(super) fn review(disk: &File, home: &Path) -> io::Result<Plan> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(denied());
    }
    let metadata = disk.metadata()?;
    if !metadata.file_type().is_block_device()
        || metadata.uid() != 0
        || metadata.mode() & 0o007 != 0
    {
        return Err(denied());
    }
    let home_device = std::fs::metadata(home)?.dev();
    let partition = std::fs::canonicalize(format!(
        "/sys/dev/block/{}:{}",
        libc::major(home_device),
        libc::minor(home_device)
    ))?;
    let sysfs = std::fs::canonicalize(format!(
        "/sys/dev/block/{}:{}",
        libc::major(metadata.rdev()),
        libc::minor(metadata.rdev())
    ))?;
    if partition.parent() != Some(sysfs.as_path()) || number(&partition.join("partition"))? != 1 {
        return Err(denied());
    }
    let name = sysfs
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(denied)?;
    let suffix = name.strip_prefix("nbd").ok_or_else(denied)?;
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return Err(denied());
    }
    let path = PathBuf::from(format!("/dev/{name}"));
    let named = std::fs::symlink_metadata(&path)?;
    if !named.file_type().is_block_device()
        || named.rdev() != metadata.rdev()
        || (named.dev(), named.ino()) != (metadata.dev(), metadata.ino())
    {
        return Err(denied());
    }
    let sequence = number(&sysfs.join("diskseq"))?;
    let mut size = 0_u64;
    let mut sector = 0_u32;
    if unsafe { libc::ioctl(disk.as_raw_fd(), 0x80081272, &raw mut size) } != 0
        || unsafe { libc::ioctl(disk.as_raw_fd(), 0x1268, &raw mut sector) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut parsed = staging::linux_parse::disk_from_sfdisk(&dump(disk)?, size, u64::from(sector))
        .map_err(io::Error::other)?;
    let p1 = parsed
        .partitions
        .iter()
        .find(|p| p.number == 1)
        .ok_or_else(denied)?;
    let start = number(&partition.join("start"))?;
    let sectors = number(&partition.join("size"))?;
    if parsed.sector_bytes != u64::from(sector)
        || sector != 512
        || parsed.partitions.len() != 1
        || p1.first_lba != start
        || p1
            .last_lba
            .checked_sub(start)
            .and_then(|n| n.checked_add(1))
            != Some(sectors)
        || number(&sysfs.join("size"))?.checked_mul(512) != Some(size)
        || number(&sysfs.join("diskseq"))? != sequence
    {
        return Err(denied());
    }
    let after = std::fs::symlink_metadata(&path)?;
    if (
        after.dev(),
        after.ino(),
        after.rdev(),
        after.uid(),
        after.mode(),
    ) != (
        metadata.dev(),
        metadata.ino(),
        metadata.rdev(),
        metadata.uid(),
        metadata.mode(),
    ) {
        return Err(denied());
    }
    parsed.id = path.to_string_lossy().into_owned();
    let basis = Basis {
        disk: parsed,
        system_volume: None,
        linux_btrfs_volume: None,
        firmware: firmware()?,
    };
    staging::plan::compute(
        &basis,
        Requirements {
            staging_esp_bytes: 512 * 1024 * 1024,
            staging_root_bytes: 4 * 1024 * 1024 * 1024,
            target_bytes: staging::MIN_TARGET_BYTES,
        },
    )
    .map_err(|error| io::Error::other(format!("{error:?}")))
}

#[test]
fn owned_native_staging_probe_reaps_after_missing_stdout() {
    // Genuine command, exact production cleanup owner, and a fallible stdout
    // extraction before any probe result exists. No source grant is fabricated.
    let mut command = Command::new("/usr/bin/sleep");
    command
        .arg("30")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .env_remove("LD_AUDIT");
    let child = command.spawn().unwrap();
    let pid = child.id();
    let mut owner = OwnedProbe {
        child,
        status: None,
    };
    assert!(owner.child.stdout.take().ok_or_else(denied).is_err());
    drop(owner);
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid as i32, &raw mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

//! The helper's machine: TUN devices by ioctl, `ip`, `nft` and `busctl` run
//! with argument vectors the [`super::plan`] built, forwarding through
//! `/proc/sys`, and the record on disk.

use std::fs;
use std::io::Write as _;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};

use super::Backend;
use super::plan::{Command, Program};
use super::record::Record;

/// Where the helper finds what it runs and keeps.
#[derive(Clone, Debug)]
pub struct Config {
    pub ip: PathBuf,
    pub nft: PathBuf,
    pub busctl: PathBuf,
    /// The record of what is set up, gone at reboot with everything it names.
    pub record: PathBuf,
    /// Present while `NetworkManager`'s global DNS points at the tunnel, which
    /// `NetworkManager` keeps across reboots.
    pub dns_marker: PathBuf,
    pub forwarding: PathBuf,
    pub sys_class_net: PathBuf,
    pub seat: seat_peer::Seat,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ip: program("ip"),
            nft: program("nft"),
            busctl: program("busctl"),
            record: PathBuf::from("/run/tilo-device-vpn/record.json"),
            dns_marker: PathBuf::from("/var/lib/tilo/device-vpn/global-dns"),
            forwarding: PathBuf::from("/proc/sys/net/ipv4/ip_forward"),
            sys_class_net: PathBuf::from("/sys/class/net"),
            seat: seat_peer::Seat::default(),
        }
    }
}

/// `/usr/bin/<name>`, or `/usr/sbin/<name>` where the two are not merged.
fn program(name: &str) -> PathBuf {
    let bin = Path::new("/usr/bin").join(name);
    if bin.exists() {
        bin
    } else {
        Path::new("/usr/sbin").join(name)
    }
}

pub struct LinuxBackend {
    config: Config,
}

impl LinuxBackend {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// The record an earlier helper left, or an empty one.
    pub fn load(&self) -> Record {
        fs::read(&self.config.record)
            .map_or_else(|_| Record::default(), |bytes| Record::parse(&bytes))
    }

    /// Whether `NetworkManager`'s DNS was left pointing at a tunnel.
    pub fn dns_marked(&self) -> bool {
        self.config.dns_marker.exists()
    }

    fn path(&self, program: Program) -> &Path {
        match program {
            Program::Ip => &self.config.ip,
            Program::Nft => &self.config.nft,
            Program::Busctl => &self.config.busctl,
        }
    }
}

impl Backend for LinuxBackend {
    fn create_tun(&mut self, name: &str) -> Result<OwnedFd, String> {
        create_tun(name).map_err(|error| format!("creating {name}: {error}"))
    }

    fn run(&mut self, command: &Command) -> Result<(), String> {
        let mut process = Process::new(self.path(command.program));
        process
            .args(&command.args)
            .env_clear()
            .env("PATH", "/usr/bin:/usr/sbin")
            .stdin(if command.input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = process
            .spawn()
            .map_err(|error| format!("{:?}: {error}", command.program))?;
        if let (Some(input), Some(mut stdin)) = (&command.input, child.stdin.take()) {
            stdin
                .write_all(input.as_bytes())
                .map_err(|error| format!("{:?}: {error}", command.program))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("{:?}: {error}", command.program))?;
        if output.status.success() || !command.required {
            return Ok(());
        }
        Err(format!(
            "{:?} {} failed: {}",
            command.program,
            command.args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }

    fn interface_exists(&self, name: &str) -> bool {
        self.config.sys_class_net.join(name).exists()
    }

    fn has_session(&self, uid: u32) -> bool {
        self.config.seat.has_session(uid)
    }

    fn forwarding(&self) -> Result<bool, String> {
        fs::read_to_string(&self.config.forwarding)
            .map(|text| text.trim() == "1")
            .map_err(|error| format!("reading forwarding: {error}"))
    }

    fn set_forwarding(&mut self, on: bool) -> Result<(), String> {
        fs::write(&self.config.forwarding, if on { "1\n" } else { "0\n" })
            .map_err(|error| format!("setting forwarding: {error}"))
    }

    fn save(&mut self, record: &Record) -> Result<(), String> {
        let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        write_atomic(&self.config.record, &bytes)?;
        if record.dns_set() {
            write_atomic(&self.config.dns_marker, b"")?;
        } else {
            match fs::remove_file(&self.config.dns_marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("removing the DNS marker: {error}")),
            }
        }
        Ok(())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("a record needs a directory")?;
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let staged = path.with_extension("staged");
    fs::write(&staged, bytes).map_err(|error| format!("{}: {error}", staged.display()))?;
    fs::rename(&staged, path).map_err(|error| format!("{}: {error}", path.display()))
}

/// Create the TUN `name` (IPv4/IPv6 packets with no extra header) and
/// return its descriptor. The device exists while a descriptor does.
pub fn create_tun(name: &str) -> std::io::Result<OwnedFd> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() >= libc::IFNAMSIZ || bytes.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not an interface name",
        ));
    }
    // SAFETY: a constant, NUL-terminated path and plain flags.
    let raw = unsafe { libc::open(c"/dev/net/tun".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `open` just returned this descriptor and nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: a zeroed ifreq is a valid empty request.
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request.ifr_name.iter_mut().zip(bytes) {
        *slot = libc::c_char::from_ne_bytes([*byte]);
    }
    request.ifr_ifru.ifru_flags =
        libc::c_short::try_from(libc::IFF_TUN | libc::IFF_NO_PI).map_err(std::io::Error::other)?;
    // SAFETY: TUNSETIFF reads and writes one ifreq, which lives on this
    // frame for the call.
    if unsafe { libc::ioctl(fd.as_raw_fd(), libc::TUNSETIFF, &raw mut request) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fd)
}

//! Real execution of disk operations — the app's **external side-effect
//! boundary**.
//!
//! Mount / unmount / format / eject / benchmark are privileged, per-machine
//! effects against real block devices (via udisks2 / polkit, or the CLI
//! fallback). They change the running system, never the document, so they
//! are effects ([`crate::resource::DisksEffect`]), not actions: the functions
//! here take no document and cannot touch it.
//!
//! Everything is *planned* as plain data first (a [`UdisksCall`] or an ordered
//! list of [`CommandSpec`] fallbacks) so the construction is unit-testable
//! without a system bus or root. Two execution layers back the plan:
//!
//!   1. **udisks2 over D-Bus** (feature `live`, via `zbus`) — the preferred
//!      path for mount / unmount / format; it carries polkit authorisation.
//!   2. **CLI fallback** — `udisksctl` first (still udisks2, still polkit),
//!      then the raw `mount` / `umount` / `mkfs.<fstype>` tools, shelled out
//!      through [`std::process::Command`].
//!
//! Benchmark performs a genuine sequential-read + random-seek measurement on
//! the block device.
//!
//! Only a live window over this machine runs them; a screenshot or the sample
//! machine in tests only ever builds the plans.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::Command;
use std::time::Duration;

/// The privileged operations the app can perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Mount,
    Unmount,
    Format,
    Benchmark,
    Eject,
}

use block_devices::udisks::{
    IFACE_BLOCK, IFACE_FILESYSTEM as IFACE_FS, object_path as udisks_object_path,
};

/// A fully-described udisks2 D-Bus method call. Kept as data so the object
/// path / interface / method are asserted in tests without a live bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdisksCall {
    /// Kernel device leaf name (`sdc1`) the call addresses.
    pub device: String,
    pub object_path: String,
    pub interface: &'static str,
    pub method: &'static str,
    /// Target filesystem type for `Block.Format`; empty for other calls.
    pub fstype: String,
}

/// One shell fallback command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    fn new(program: &str, args: &[&str]) -> Self {
        CommandSpec {
            program: program.to_string(),
            args: args.iter().map(std::string::ToString::to_string).collect(),
        }
    }
}

// ---- D-Bus call plans --------------------------------------------------

pub fn mount_call(dev_leaf: &str) -> UdisksCall {
    UdisksCall {
        device: dev_leaf.to_string(),
        object_path: udisks_object_path(dev_leaf),
        interface: IFACE_FS,
        method: "Mount",
        fstype: String::new(),
    }
}

pub fn unmount_call(dev_leaf: &str) -> UdisksCall {
    UdisksCall {
        device: dev_leaf.to_string(),
        object_path: udisks_object_path(dev_leaf),
        interface: IFACE_FS,
        method: "Unmount",
        fstype: String::new(),
    }
}

pub fn format_call(dev_leaf: &str, fstype: &str) -> UdisksCall {
    UdisksCall {
        device: dev_leaf.to_string(),
        object_path: udisks_object_path(dev_leaf),
        interface: IFACE_BLOCK,
        method: "Format",
        fstype: fstype.to_string(),
    }
}

// ---- CLI fallback plans (tried in order) -------------------------------

pub fn mount_cli(dev_leaf: &str) -> Vec<CommandSpec> {
    let dev = format!("/dev/{dev_leaf}");
    vec![
        CommandSpec::new("udisksctl", &["mount", "-b", &dev]),
        CommandSpec::new("mount", &[&dev]),
    ]
}

pub fn unmount_cli(dev_leaf: &str) -> Vec<CommandSpec> {
    let dev = format!("/dev/{dev_leaf}");
    vec![
        CommandSpec::new("udisksctl", &["unmount", "-b", &dev]),
        CommandSpec::new("umount", &[&dev]),
    ]
}

pub fn format_cli(dev_leaf: &str, fstype: &str) -> Vec<CommandSpec> {
    let dev = format!("/dev/{dev_leaf}");
    let mkfs = format!("mkfs.{fstype}");
    // `-F` forces past the "device in use / has a filesystem" guard on the
    // ext* + vfat mkfs variants, matching what udisks does internally.
    vec![CommandSpec::new(&mkfs, &["-F", &dev])]
}

pub fn eject_cli(dev_leaf: &str) -> Vec<CommandSpec> {
    let dev = format!("/dev/{dev_leaf}");
    vec![
        CommandSpec::new("udisksctl", &["power-off", "-b", &dev]),
        CommandSpec::new("eject", &[&dev]),
    ]
}

// ---- execution ---------------------------------------------------------

/// Run each command in turn, returning on the first success. A missing
/// program (or a non-zero exit) falls through to the next candidate.
///
/// # Errors
///
/// Returns an error if all commands fail.
pub fn run_command_chain(specs: &[CommandSpec]) -> Result<String, String> {
    let mut last = String::from("no command available");
    for spec in specs {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        match bounded_command::run_prepared(command, None, 1024 * 1024, Duration::from_secs(5 * 60))
        {
            Ok(out) if out.status.success() => {
                return Ok(format!("{} succeeded", spec.program));
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .filter(|character| !character.is_control() || *character == ' ')
                    .take(512)
                    .collect::<String>();
                last = format!("{} failed: {}", spec.program, err.trim());
            }
            Err(e) => {
                last = format!("{}: {e}", spec.program);
            }
        }
    }
    Err(last)
}

/// Invoke a udisks2 method over the system bus. Only compiled with `live`.
///
/// # Errors
///
/// Returns an error if the D-Bus call fails or the operation returns an error.
#[cfg(feature = "live")]
pub fn run_udisks(call: &UdisksCall) -> Result<String, String> {
    let udisks = block_devices::udisks::Udisks::system().map_err(|e| format!("system bus: {e}"))?;
    let device = call.device.as_str();
    // UDisks2 picks its defaults (auto mountpoint, default mkfs options),
    // while polkit handles authorisation.
    let reply = match call.method {
        "Mount" => udisks.mount(device).map(drop),
        "Unmount" => udisks.unmount(device),
        "Format" => udisks.format(device, &call.fstype),
        other => return Err(format!("udisks2 {other}: unsupported call")),
    };
    reply
        .map(|()| format!("udisks2 {} ok", call.method))
        .map_err(|e| format!("udisks2 {}: {e}", call.method))
}

/// A real sequential-read + random-seek benchmark of a block device. Reads up
/// to 64 MiB sequentially, then times 100 random 4 KiB reads. Needs read
/// permission on `/dev/<leaf>` (root for most block devices).
///
/// # Errors
///
/// Returns an error if the device cannot be opened or read.
pub fn benchmark(dev_leaf: &str) -> Result<String, String> {
    let path = format!("/dev/{dev_leaf}");
    let mut f = File::open(&path).map_err(|e| format!("open {path}: {e}"))?;
    benchmark_reader(&mut f, &path)
}

fn benchmark_reader(f: &mut (impl Read + Seek), path: &str) -> Result<String, String> {
    const BLOCK: usize = 1 << 20;
    const TARGET: usize = 64 << 20;
    const SEEKS: u32 = 100;

    let mut buf = vec![0u8; BLOCK];
    let start = ui::time::now();
    let mut total = 0usize;
    while total < TARGET {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) => return Err(format!("read {path}: {e}")),
        }
    }
    if total == 0 {
        return Err(format!("read {path}: device returned no data"));
    }
    let secs = start.elapsed().as_secs_f64().max(1e-9);
    let mbs = (total as f64 / 1_000_000.0) / secs;

    // Random-seek latency across the device.
    let size = f
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("seek {path}: {e}"))?;
    if size < 4096 {
        return Err(format!(
            "read {path}: device is smaller than one 4 KiB sample"
        ));
    }
    let mut small = vec![0u8; 4096];
    let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
    let seek_start = ui::time::now();
    for _ in 0..SEEKS {
        rng = rng
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let off = (rng % size.saturating_sub(4096).max(1)) & !0xFFF;
        f.seek(SeekFrom::Start(off))
            .map_err(|e| format!("seek {path}: {e}"))?;
        f.read_exact(&mut small)
            .map_err(|e| format!("read {path}: {e}"))?;
    }
    let avg_ms = seek_start.elapsed().as_secs_f64() * 1000.0 / f64::from(SEEKS);

    Ok(format!(
        "{path}: {mbs:.0} MB/s sequential read, {avg_ms:.2} ms average seek"
    ))
}

/// Execute an operation for real. Tries udisks2 (D-Bus, when built with
/// `live`) first, then the CLI chain. Returns a status message either way.
///
/// # Errors
///
/// Returns an error if the operation fails.
pub fn run(op: Op, dev_leaf: &str, fstype: Option<&str>) -> Result<String, String> {
    match op {
        Op::Benchmark => benchmark(dev_leaf),
        Op::Mount => {
            #[cfg(feature = "live")]
            if run_udisks(&mount_call(dev_leaf)).is_ok() {
                return Ok(format!("Mounted /dev/{dev_leaf}"));
            }
            run_command_chain(&mount_cli(dev_leaf)).map(|_| format!("Mounted /dev/{dev_leaf}"))
        }
        Op::Unmount => {
            #[cfg(feature = "live")]
            if run_udisks(&unmount_call(dev_leaf)).is_ok() {
                return Ok(format!("Unmounted /dev/{dev_leaf}"));
            }
            run_command_chain(&unmount_cli(dev_leaf)).map(|_| format!("Unmounted /dev/{dev_leaf}"))
        }
        Op::Format => {
            let fs = fstype.unwrap_or("ext4");
            #[cfg(feature = "live")]
            if run_udisks(&format_call(dev_leaf, fs)).is_ok() {
                return Ok(format!("Formatted /dev/{dev_leaf} as {fs}"));
            }
            run_command_chain(&format_cli(dev_leaf, fs))
                .map(|_| format!("Formatted /dev/{dev_leaf} as {fs}"))
        }
        Op::Eject => {
            run_command_chain(&eject_cli(dev_leaf)).map(|_| format!("Ejected /dev/{dev_leaf}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingRandomIo {
        data: std::io::Cursor<Vec<u8>>,
        fail_seek: bool,
        random_phase: bool,
    }

    impl Read for FailingRandomIo {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.random_phase {
                return Err(std::io::Error::other("random read failed"));
            }
            self.data.read(buf)
        }
    }

    impl Seek for FailingRandomIo {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.random_phase = true;
            if self.fail_seek {
                return Err(std::io::Error::other("device seek failed"));
            }
            self.data.seek(pos)
        }
    }

    #[test]
    fn benchmark_reports_random_io_failures_instead_of_latency() {
        for (fail_seek, expected) in [(true, "device seek failed"), (false, "random read failed")] {
            let mut reader = FailingRandomIo {
                data: std::io::Cursor::new(vec![0; 8192]),
                fail_seek,
                random_phase: false,
            };
            let error = benchmark_reader(&mut reader, "test-device").unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(error.contains("test-device"), "{error}");
        }
        let mut healthy = std::io::Cursor::new(vec![0; 8192]);
        assert!(benchmark_reader(&mut healthy, "test-device").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn benchmark_rejects_a_device_that_returns_no_data() {
        let error = benchmark("null").expect_err("an empty device cannot yield benchmark results");
        assert!(error.contains("/dev/null"), "{error}");
    }

    /// Mount/unmount/format target the right `UDisks2` D-Bus calls, fall back to udisksctl then raw tools, and a failed chain reports its last failure.
    #[test]
    fn device_commands_target_udisks_then_cli_fallbacks() {
        // --- phase: dbus_calls_target_the_right_interface_and_method ---
        {
            let m = mount_call("sdc1");
            assert_eq!(m.interface, IFACE_FS);
            assert_eq!(m.method, "Mount");
            assert!(m.object_path.ends_with("/sdc1"));

            let u = unmount_call("sdc1");
            assert_eq!(u.method, "Unmount");
            assert_eq!(u.interface, IFACE_FS);

            let f = format_call("sdc1", "ext4");
            assert_eq!(f.interface, IFACE_BLOCK);
            assert_eq!(f.method, "Format");
            assert_eq!(f.fstype, "ext4");
        }

        // --- phase: cli_fallbacks_prefer_udisksctl_then_raw_tools ---
        {
            let m = mount_cli("sdc1");
            assert_eq!(m[0].program, "udisksctl");
            assert_eq!(m[0].args, vec!["mount", "-b", "/dev/sdc1"]);
            assert_eq!(m[1].program, "mount");
            assert_eq!(m[1].args, vec!["/dev/sdc1"]);

            let u = unmount_cli("sdc1");
            assert_eq!(u[0].program, "udisksctl");
            assert_eq!(u[0].args, vec!["unmount", "-b", "/dev/sdc1"]);
            assert_eq!(u[1].program, "umount");

            let f = format_cli("sdc1", "vfat");
            assert_eq!(f[0].program, "mkfs.vfat");
            assert_eq!(f[0].args, vec!["-F", "/dev/sdc1"]);
        }

        // --- phase: command_chain_reports_the_last_failure_when_nothing_runs ---
        {
            // Two programs that certainly do not exist → an Err carrying the last.
            let specs = vec![
                CommandSpec::new("tilo-no-such-tool-a", &["x"]),
                CommandSpec::new("tilo-no-such-tool-b", &["y"]),
            ];
            let r = run_command_chain(&specs);
            assert!(r.is_err());
            assert!(r.unwrap_err().contains("tilo-no-such-tool-b"));
        }
    }
}

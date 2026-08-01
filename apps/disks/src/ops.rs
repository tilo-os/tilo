//! Real execution of disk operations — the app's **external side-effect
//! boundary**.
//!
//! Mount / unmount / format / eject / benchmark are privileged, per-machine
//! effects against real block devices (via udisks2 / polkit, or the CLI
//! fallback). They change the running system, never the shared Automerge
//! document, so by the qOS action-model recipe (`docs/action-model-recipe.md`)
//! **none of them is an `Action`** — none crosses the CRDT executor. The
//! resource + action-model finding is recorded in [`crate::actions`], and the
//! functions here deliberately take no `AutoCommit`: they cannot touch the doc.
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
//! None of the executing functions run unless the caller opts in at runtime
//! (`DISKS_LIVE`), so hermetic builds/tests and `--shots` stay inert — they
//! only ever build the plans.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::Command;
use std::time::Instant;

/// The privileged operations the app can perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Mount,
    Unmount,
    Format,
    Benchmark,
    Eject,
}

pub const IFACE_FS: &str = "org.freedesktop.UDisks2.Filesystem";
pub const IFACE_BLOCK: &str = "org.freedesktop.UDisks2.Block";
pub const IFACE_DRIVE: &str = "org.freedesktop.UDisks2.Drive";
pub const UDISKS_SERVICE: &str = "org.freedesktop.UDisks2";

/// A fully-described udisks2 D-Bus method call. Kept as data so the object
/// path / interface / method are asserted in tests without a live bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdisksCall {
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
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Encode a kernel device leaf name (`sda1`, `nvme0n1p2`, `dm-0`) into the
/// udisks2 `block_devices` object-path suffix. Every byte outside `[A-Za-z0-9]`
/// becomes `_<hex>` — the GDBus object-path escaping udisks uses (so `dm-0`
/// maps to `dm_2d0`).
pub fn udisks_object_path(dev_leaf: &str) -> String {
    let mut s = String::from("/org/freedesktop/UDisks2/block_devices/");
    for b in dev_leaf.bytes() {
        if b.is_ascii_alphanumeric() {
            s.push(b as char);
        } else {
            s.push('_');
            s.push_str(&format!("{b:02x}"));
        }
    }
    s
}

// ---- D-Bus call plans --------------------------------------------------

pub fn mount_call(dev_leaf: &str) -> UdisksCall {
    UdisksCall {
        object_path: udisks_object_path(dev_leaf),
        interface: IFACE_FS,
        method: "Mount",
        fstype: String::new(),
    }
}

pub fn unmount_call(dev_leaf: &str) -> UdisksCall {
    UdisksCall {
        object_path: udisks_object_path(dev_leaf),
        interface: IFACE_FS,
        method: "Unmount",
        fstype: String::new(),
    }
}

pub fn format_call(dev_leaf: &str, fstype: &str) -> UdisksCall {
    UdisksCall {
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
pub fn run_command_chain(specs: &[CommandSpec]) -> Result<String, String> {
    let mut last = String::from("no command available");
    for spec in specs {
        match Command::new(&spec.program).args(&spec.args).output() {
            Ok(out) if out.status.success() => {
                return Ok(format!("{} succeeded", spec.program));
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
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
#[cfg(feature = "live")]
pub fn run_udisks(call: &UdisksCall) -> Result<String, String> {
    use std::collections::HashMap;
    use zbus::blocking::Connection;
    use zbus::zvariant::Value;

    let conn = Connection::system().map_err(|e| format!("system bus: {e}"))?;
    let path = call.object_path.as_str();
    // Empty a{sv} options → udisks picks sensible defaults (auto mountpoint,
    // default mkfs options), while polkit handles authorisation.
    let opts: HashMap<&str, Value> = HashMap::new();

    let reply = if call.method == "Format" {
        conn.call_method(
            Some(UDISKS_SERVICE),
            path,
            Some(call.interface),
            call.method,
            &(call.fstype.as_str(), opts),
        )
    } else {
        conn.call_method(
            Some(UDISKS_SERVICE),
            path,
            Some(call.interface),
            call.method,
            &(opts,),
        )
    };
    reply
        .map(|_| format!("udisks2 {} ok", call.method))
        .map_err(|e| format!("udisks2 {}: {e}", call.method))
}

/// A real sequential-read + random-seek benchmark of a block device. Reads up
/// to 64 MiB sequentially, then times 100 random 4 KiB reads. Needs read
/// permission on `/dev/<leaf>` (root for most block devices).
pub fn benchmark(dev_leaf: &str) -> Result<String, String> {
    let path = format!("/dev/{dev_leaf}");
    let mut f = File::open(&path).map_err(|e| format!("open {path}: {e}"))?;

    const BLOCK: usize = 1 << 20;
    const TARGET: usize = 64 << 20;
    let mut buf = vec![0u8; BLOCK];
    let start = Instant::now();
    let mut total = 0usize;
    while total < TARGET {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) => return Err(format!("read {path}: {e}")),
        }
    }
    let secs = start.elapsed().as_secs_f64().max(1e-9);
    let mbs = (total as f64 / 1_000_000.0) / secs;

    // Random-seek latency across the device.
    let size = f.seek(SeekFrom::End(0)).unwrap_or(0).max(BLOCK as u64);
    let mut small = vec![0u8; 4096];
    let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
    const SEEKS: u32 = 100;
    let sstart = Instant::now();
    for _ in 0..SEEKS {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let off = (rng % size.saturating_sub(4096).max(1)) & !0xFFF;
        let _ = f.seek(SeekFrom::Start(off));
        let _ = f.read(&mut small);
    }
    let avg_ms = sstart.elapsed().as_secs_f64() * 1000.0 / SEEKS as f64;

    Ok(format!(
        "{}: {mbs:.0} MB/s sequential read · {avg_ms:.2} ms avg seek",
        path
    ))
}

/// Execute an operation for real. Tries udisks2 (D-Bus, when built with
/// `live`) first, then the CLI chain. Returns a status message either way.
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

    #[test]
    fn object_path_passes_through_plain_names() {
        assert_eq!(
            udisks_object_path("nvme0n1p2"),
            "/org/freedesktop/UDisks2/block_devices/nvme0n1p2"
        );
        assert_eq!(
            udisks_object_path("sda1"),
            "/org/freedesktop/UDisks2/block_devices/sda1"
        );
    }

    #[test]
    fn object_path_escapes_non_alnum_bytes() {
        // A dash is byte 0x2d → "_2d" (matches GDBus / udisks encoding).
        assert_eq!(
            udisks_object_path("dm-0"),
            "/org/freedesktop/UDisks2/block_devices/dm_2d0"
        );
    }

    #[test]
    fn dbus_calls_target_the_right_interface_and_method() {
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

    #[test]
    fn cli_fallbacks_prefer_udisksctl_then_raw_tools() {
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

    #[test]
    fn command_chain_reports_the_last_failure_when_nothing_runs() {
        // Two programs that certainly do not exist → an Err carrying the last.
        let specs = vec![
            CommandSpec::new("qos-no-such-tool-a", &["x"]),
            CommandSpec::new("qos-no-such-tool-b", &["y"]),
        ];
        let r = run_command_chain(&specs);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("qos-no-such-tool-b"));
    }
}

//! Real execution of disk operations — the app's **external side-effect
//! boundary**.
//!
//! Mount / unmount / format / eject are privileged, per-machine effects
//! against real block devices, made through udisks2 on the system bus, which
//! has polkit authorise each one. They change the running system, never the
//! document, so they are effects ([`crate::resource::DisksEffect`]), not
//! actions: the functions here take no document and cannot touch it. The
//! app starts no programs.
//!
//! Each is *planned* as plain data first (a [`UdisksCall`]) so the
//! construction is unit-testable without a system bus or root; the call is
//! made only in a build with the `live` feature.
//!
//! Benchmark performs a genuine sequential-read + random-seek measurement on
//! the block device.
//!
//! Only a live window over this machine runs them; a screenshot or the sample
//! machine in tests only ever builds the plans.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

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
    IFACE_BLOCK, IFACE_DRIVE, IFACE_FILESYSTEM as IFACE_FS, object_path as udisks_object_path,
};

/// A fully-described udisks2 D-Bus method call. Kept as data so the object
/// path / interface / method are asserted in tests without a live bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdisksCall {
    /// Kernel device leaf name (`sdc1`) the call addresses.
    pub device: String,
    /// The device's block object. A drive call reaches the drive through
    /// it (`Block.Drive`).
    pub object_path: String,
    pub interface: &'static str,
    pub method: &'static str,
    /// Target filesystem type for `Block.Format`; empty for other calls.
    pub fstype: String,
}

impl UdisksCall {
    fn new(dev_leaf: &str, interface: &'static str, method: &'static str) -> Self {
        UdisksCall {
            device: dev_leaf.to_string(),
            object_path: udisks_object_path(dev_leaf),
            interface,
            method,
            fstype: String::new(),
        }
    }
}

pub fn mount_call(dev_leaf: &str) -> UdisksCall {
    UdisksCall::new(dev_leaf, IFACE_FS, "Mount")
}

pub fn unmount_call(dev_leaf: &str) -> UdisksCall {
    UdisksCall::new(dev_leaf, IFACE_FS, "Unmount")
}

pub fn format_call(dev_leaf: &str, fstype: &str) -> UdisksCall {
    UdisksCall {
        fstype: fstype.to_string(),
        ..UdisksCall::new(dev_leaf, IFACE_BLOCK, "Format")
    }
}

/// Eject: power off the drive the device is, so it can be pulled out.
pub fn eject_call(dev_leaf: &str) -> UdisksCall {
    UdisksCall::new(dev_leaf, IFACE_DRIVE, "PowerOff")
}

/// Make a udisks2 call over the system bus; polkit authorises it.
#[cfg(feature = "live")]
pub fn run_udisks(call: &UdisksCall) -> Result<(), String> {
    let udisks = block_devices::udisks::Udisks::system().map_err(|e| format!("system bus: {e}"))?;
    let device = call.device.as_str();
    // UDisks2 picks its defaults (auto mountpoint, default mkfs options),
    // while polkit handles authorisation.
    let reply = match call.method {
        "Mount" => udisks.mount(device).map(drop),
        "Unmount" => udisks.unmount(device),
        "Format" => udisks.format(device, &call.fstype),
        "PowerOff" => udisks.power_off(device),
        other => return Err(format!("udisks2 {other}: unsupported call")),
    };
    reply.map_err(|e| format!("udisks2 {}: {e}", call.method))
}

/// A build without udisks2 changes no device.
///
/// # Errors
///
/// Always.
#[cfg(not(feature = "live"))]
pub fn run_udisks(call: &UdisksCall) -> Result<(), String> {
    Err(format!(
        "udisks2 {}: this build of Disks changes no devices",
        call.method
    ))
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
    let rate = ui::format::rate(total as f64 / secs);

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
        "{path}: {rate} sequential read, {avg_ms:.2} ms average seek"
    ))
}

/// Execute an operation for real, through udisks2. Returns a status
/// message either way.
pub fn run(op: Op, dev_leaf: &str, fstype: Option<&str>) -> Result<String, String> {
    let dev = format!("/dev/{dev_leaf}");
    let (call, done) = match op {
        Op::Benchmark => return benchmark(dev_leaf),
        Op::Mount => (mount_call(dev_leaf), format!("Mounted {dev}")),
        Op::Unmount => (unmount_call(dev_leaf), format!("Unmounted {dev}")),
        Op::Format => {
            let fs = fstype.unwrap_or("ext4");
            (
                format_call(dev_leaf, fs),
                format!("Formatted {dev} as {fs}"),
            )
        }
        Op::Eject => (eject_call(dev_leaf), format!("Ejected {dev}")),
    };
    run_udisks(&call).map(|()| done)
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

    /// Every device operation is a udisks2 call, which polkit authorises:
    /// the app starts no programs.
    #[test]
    fn device_operations_are_udisks_calls() {
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

        let e = eject_call("sdc");
        assert_eq!(e.interface, IFACE_DRIVE);
        assert_eq!(e.method, "PowerOff");
        assert!(e.object_path.ends_with("/block_devices/sdc"));
    }
}

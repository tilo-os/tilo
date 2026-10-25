//! What this machine offers remote capture: the trusted helper programs and
//! its video devices, probed on a thread of its own and probed again when
//! they change.
//!
//! Finding them stats files under `/usr` and lists and reads
//! `/sys/class/video4linux`, each a wait on the disk. The capture runtime
//! asks on the compositor thread, so it reads the last probe instead
//! ([`CaptureHost::current`]). Until the first probe lands, nothing is
//! available. A video device appearing or going (`/dev/video*`) and a
//! program installed or removed in `/usr/bin` start a new probe.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, Weak, mpsc};

use tilo_lock::RwLockRecover as _;

/// Where a trusted helper program may live, in order.
const TOOL_DIRECTORIES: [&str; 2] = ["/usr/bin", "/usr/libexec/tilo"];

/// Every helper program remote capture runs.
const TOOLS: [&str; 7] = [
    "v4l2-ctl",
    "pw-loopback",
    "pw-cat",
    "pw-dump",
    "gst-launch-1.0",
    "pw-record",
    "scanimage",
];

/// The label of the v4l2loopback device a remote camera is played into.
const REMOTE_CAMERA_LABEL: &str = "Tilo Remote Camera";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Probe {
    tools: BTreeMap<&'static str, PathBuf>,
    /// The first camera that is not the remote-camera loopback.
    physical_camera: Option<PathBuf>,
    /// The loopback device a remote camera is played into.
    remote_camera_loopback: Option<PathBuf>,
}

impl Probe {
    /// Look now. Waits on the disk: never on the compositor thread.
    fn now() -> Self {
        let tools = TOOLS
            .into_iter()
            .filter_map(|name| Some((name, trusted_command(name)?)))
            .collect();
        let mut physical_camera = None;
        let mut remote_camera_loopback = None;
        for (device, label) in video_devices() {
            if label.starts_with(REMOTE_CAMERA_LABEL) {
                remote_camera_loopback.get_or_insert(device);
            } else {
                physical_camera.get_or_insert(device);
            }
        }
        Self {
            tools,
            physical_camera,
            remote_camera_loopback,
        }
    }

    /// The trusted program `name`, when it is installed.
    pub(crate) fn tool(&self, name: &str) -> Option<&Path> {
        self.tools.get(name).map(PathBuf::as_path)
    }

    pub(crate) fn physical_camera(&self) -> Option<&Path> {
        self.physical_camera.as_deref()
    }

    pub(crate) fn remote_camera_loopback(&self) -> Option<&Path> {
        self.remote_camera_loopback.as_deref()
    }

    /// A machine with exactly these programs and devices, for tests.
    #[cfg(test)]
    pub(crate) fn of(
        tools: &[(&'static str, &str)],
        physical_camera: Option<&str>,
        remote_camera_loopback: Option<&str>,
    ) -> Self {
        Self {
            tools: tools
                .iter()
                .map(|(name, path)| (*name, PathBuf::from(path)))
                .collect(),
            physical_camera: physical_camera.map(PathBuf::from),
            remote_camera_loopback: remote_camera_loopback.map(PathBuf::from),
        }
    }
}

/// The last probe of the machine, shared by the capture adapter and source.
#[derive(Clone, Debug)]
pub(crate) struct CaptureHost {
    current: Arc<RwLock<Arc<Probe>>>,
}

impl CaptureHost {
    /// Probe this machine on a thread of its own, now and whenever its video
    /// devices or programs change, for as long as a copy of the returned
    /// value lives.
    pub(crate) fn watch() -> Self {
        let host = Self::fixed(Probe::default());
        let shared = Arc::downgrade(&host.current);
        if let Err(error) = std::thread::Builder::new()
            .name("remote-capture-probe".into())
            .spawn(move || follow(&shared))
        {
            tracing::warn!(%error, "remote capture: cannot probe this machine's devices");
        }
        host
    }

    /// A host that never changes from `probe`.
    pub(crate) fn fixed(probe: Probe) -> Self {
        Self {
            current: Arc::new(RwLock::new(Arc::new(probe))),
        }
    }

    /// The last probe. Never waits on the disk.
    pub(crate) fn current(&self) -> Arc<Probe> {
        Arc::clone(&self.current.read_recover())
    }
}

/// Probe into `shared` until nothing reads it any more: once now, and again
/// after each change of `/dev`'s video devices or `/usr/bin`.
fn follow(shared: &Weak<RwLock<Arc<Probe>>>) {
    let (notify, notices) = mpsc::channel::<()>();
    let watches: Vec<_> = [
        (Path::new("/dev"), is_video_device as fn(&Path) -> bool),
        (Path::new("/usr/bin"), is_tool as fn(&Path) -> bool),
    ]
    .into_iter()
    .filter_map(|(directory, names)| {
        let notify = notify.clone();
        // Which devices and tools exist, not what is written to them: every write to
        // /dev/null or /dev/fuse is a write to an entry of /dev.
        tilo_files::native::watch_presence(directory, names, move || {
            let _ = notify.send(());
        })
        .inspect_err(|error| {
            tracing::debug!(%error, directory = %directory.display(), "remote capture: not following changes");
        })
        .ok()
    })
    .collect();
    drop(notify);
    loop {
        let probe = Arc::new(Probe::now());
        let Some(current) = shared.upgrade() else {
            return;
        };
        *current.write_recover() = probe;
        drop(current);
        if notices.recv().is_err() {
            // Nothing watches: the probe stands.
            drop(watches);
            return;
        }
        // A device appearing comes as a burst of events; one probe serves it.
        while notices.try_recv().is_ok() {}
    }
}

fn is_video_device(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("video"))
}

fn is_tool(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| TOOLS.contains(&name))
}

fn trusted_command(name: &str) -> Option<PathBuf> {
    TOOL_DIRECTORIES
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|path| path.is_file())
}

/// Every video device and its label, in the kernel's order.
fn video_devices() -> Vec<(PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir("/sys/class/video4linux") else {
        return Vec::new();
    };
    let mut devices: Vec<(PathBuf, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let label = std::fs::read_to_string(entry.path().join("name")).ok()?;
            Some((
                Path::new("/dev").join(entry.file_name()),
                label.trim().to_owned(),
            ))
        })
        .collect();
    devices.sort();
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_host_answers_from_its_probe() {
        let host = CaptureHost::fixed(Probe::of(
            &[("pw-record", "/usr/bin/pw-record")],
            Some("/dev/video0"),
            None,
        ));
        let probe = host.current();
        assert_eq!(
            probe.tool("pw-record"),
            Some(Path::new("/usr/bin/pw-record"))
        );
        assert_eq!(probe.tool("scanimage"), None);
        assert_eq!(probe.physical_camera(), Some(Path::new("/dev/video0")));
        assert_eq!(probe.remote_camera_loopback(), None);
    }

    #[test]
    fn only_video_devices_and_capture_programs_start_a_probe() {
        assert!(is_video_device(Path::new("/dev/video2")));
        assert!(!is_video_device(Path::new("/dev/snd")));
        assert!(is_tool(Path::new("/usr/bin/pw-cat")));
        assert!(!is_tool(Path::new("/usr/bin/bash")));
    }
}

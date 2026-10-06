//! Where one user's legacy VM lives and how large it may grow.

use std::path::{Path, PathBuf};
use std::time::Duration;

use legacy_agent_protocol::GuestOs;
use legacy_agent_protocol::waypipe::host_port_for_uid;

use crate::memory::MemoryPolicy;

/// The files of one user's VM for one guest OS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paths {
    /// Durable: the disk, installed apps and grants.
    pub state: PathBuf,
    /// Per login: the supervisor's socket and lock, the virtio-fs sockets.
    pub runtime: PathBuf,
    /// Shared between guest OS VMs of this user: fetched base images.
    pub images: PathBuf,
}

impl Paths {
    /// `$XDG_DATA_HOME/tilo/legacy/<guest>`, `$XDG_RUNTIME_DIR/tilo-legacy/<guest>`
    /// and `$XDG_CACHE_HOME/tilo/legacy-images`.
    #[must_use]
    pub fn for_user(guest: GuestOs) -> Option<Self> {
        Some(Self::under(
            &tilo_paths::xdg::data_home()?,
            &tilo_paths::xdg::runtime_dir()?,
            &tilo_paths::xdg::cache_home()?,
            guest,
        ))
    }

    #[must_use]
    pub fn under(data: &Path, runtime: &Path, cache: &Path, guest: GuestOs) -> Self {
        Self {
            state: data.join("tilo/legacy").join(guest.as_str()),
            runtime: runtime.join("tilo-legacy").join(guest.as_str()),
            images: cache.join("tilo/legacy-images"),
        }
    }

    #[must_use]
    pub fn disk(&self) -> PathBuf {
        self.state.join("disk.qcow2")
    }

    #[must_use]
    pub fn grants(&self) -> PathBuf {
        self.state.join("grants.json")
    }

    #[must_use]
    pub fn apps(&self) -> PathBuf {
        self.state.join("apps.json")
    }

    #[must_use]
    pub fn supervisor_socket(&self) -> PathBuf {
        self.runtime.join("supervisor.sock")
    }

    #[must_use]
    pub fn supervisor_lock(&self) -> PathBuf {
        self.runtime.join("supervisor.lock")
    }

    #[must_use]
    pub fn supervisor_log(&self) -> PathBuf {
        self.runtime.join("supervisor.log")
    }

    /// The guest's serial console.
    #[must_use]
    pub fn console_log(&self) -> PathBuf {
        self.state.join("console.log")
    }

    /// QEMU's vhost-user socket to `vhost-device-vsock`.
    #[must_use]
    pub fn vsock_vhost_user(&self) -> PathBuf {
        self.runtime.join("vsock-vhu.sock")
    }

    /// `vhost-device-vsock`'s host side: connect here to reach guest ports.
    #[must_use]
    pub fn vsock_uds(&self) -> PathBuf {
        self.runtime.join("vsock.sock")
    }

    #[must_use]
    pub fn share_socket(&self, tag: &str) -> PathBuf {
        self.runtime.join(format!("{tag}.sock"))
    }
}

/// One user's VM for one guest OS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmConfig {
    pub guest: GuestOs,
    /// Unique within the user's own libvirt session.
    pub domain_name: String,
    /// The read-only base image the disk is an overlay of.
    pub base_image: PathBuf,
    pub paths: Paths,
    pub vcpus: u32,
    pub memory: MemoryPolicy,
    /// No running app for this long sends the VM to disk.
    pub idle_suspend: Duration,
    /// The vsock port this user's Wayland forwarder listens on.
    pub waypipe_port: u32,
    /// Guest uid/gid shared files appear as.
    pub guest_uid: u32,
    pub host_uid: u32,
    pub host_gid: u32,
}

impl VmConfig {
    #[must_use]
    pub fn new(guest: GuestOs, base_image: PathBuf, paths: Paths) -> Self {
        // SAFETY: getuid/getgid have no failure mode.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Self {
            guest,
            domain_name: format!("tilo-legacy-{}", guest.as_str()),
            base_image,
            paths,
            vcpus: available_vcpus().clamp(1, 4),
            memory: MemoryPolicy::default(),
            idle_suspend: Duration::from_secs(5 * 60),
            waypipe_port: host_port_for_uid(uid),
            guest_uid: 1000,
            host_uid: uid,
            host_gid: gid,
        }
    }
}

fn available_vcpus() -> u32 {
    std::thread::available_parallelism().map_or(2, |count| {
        u32::try_from(count.get().div_ceil(2)).unwrap_or(2)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_guest_os_gets_its_own_files_and_domain() {
        let linux = Paths::under(
            Path::new("/d"),
            Path::new("/run/user/1000"),
            Path::new("/c"),
            GuestOs::Linux,
        );
        assert_eq!(
            linux.disk(),
            PathBuf::from("/d/tilo/legacy/linux/disk.qcow2")
        );
        assert_eq!(
            linux.share_socket("fs-1"),
            PathBuf::from("/run/user/1000/tilo-legacy/linux/fs-1.sock")
        );
        assert_eq!(linux.images, PathBuf::from("/c/tilo/legacy-images"));
        let config = VmConfig::new(GuestOs::Linux, "/c/base.qcow2".into(), linux);
        assert_eq!(config.domain_name, "tilo-legacy-linux");
        assert!((1..=4).contains(&config.vcpus));
    }
}

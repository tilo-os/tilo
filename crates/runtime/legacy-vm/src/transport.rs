//! How this host gives a VM its vsock device.
//!
//! The kernel's `vhost-vsock` is the normal case. A host whose kernel lacks
//! it (nested and sandboxed hosts) can still run legacy apps through
//! QEMU's `vhost-user-vsock` served by the unprivileged `vhost-device-vsock`
//! daemon ([`AgentLink::Hybrid`]). QEMU cannot save a VM with a vhost-user
//! vsock device, so on such a host an idle VM is shut down instead of saved,
//! and the next launch boots it.

use std::path::Path;

use legacy_agent_protocol::link::{AgentLink, HYBRID_GUEST_CID};
use libvirt_domain::xml_text;

/// The libvirt namespace for raw QEMU arguments.
pub const QEMU_NAMESPACE: &str = "http://libvirt.org/schemas/domain/qemu/1.0";

/// The vhost-user vsock daemon.
pub const VHOST_DEVICE_VSOCK: &str = "vhost-device-vsock";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VsockBackend {
    Kernel,
    Hybrid,
}

impl VsockBackend {
    /// The kernel device when this user can open it, else the daemon when
    /// it is installed.
    #[must_use]
    pub fn detect() -> Option<Self> {
        let kernel = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/vhost-vsock")
            .is_ok();
        Self::choose(kernel, on_path(VHOST_DEVICE_VSOCK))
    }

    #[must_use]
    pub const fn choose(kernel: bool, daemon: bool) -> Option<Self> {
        match (kernel, daemon) {
            (true, _) => Some(Self::Kernel),
            (false, true) => Some(Self::Hybrid),
            (false, false) => None,
        }
    }

    /// Whether an idle VM can be saved to disk and restored.
    #[must_use]
    pub const fn snapshots(self) -> bool {
        matches!(self, Self::Kernel)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kernel => "kernel vhost-vsock",
            Self::Hybrid => "vhost-user vsock (no snapshots: idle VMs shut down)",
        }
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// `vhost-device-vsock --vm guest-cid=3,socket=<vhost-user>,uds-path=<uds>`.
#[must_use]
pub fn vhost_device_vsock_argv(vhost_user: &Path, uds: &Path) -> Vec<String> {
    vec![
        VHOST_DEVICE_VSOCK.to_string(),
        "--vm".to_string(),
        format!(
            "guest-cid={HYBRID_GUEST_CID},socket={},uds-path={}",
            vhost_user.display(),
            uds.display()
        ),
    ]
}

/// The QEMU arguments that add the vhost-user vsock device. libvirt has no
/// element for it.
#[must_use]
pub fn hybrid_commandline_xml(vhost_user: &Path) -> String {
    let chardev = xml_text(&format!(
        "socket,id=tilovsock,path={}",
        vhost_user.display()
    ));
    format!(
        "<qemu:commandline>\
<qemu:arg value='-chardev'/><qemu:arg value='{chardev}'/>\
<qemu:arg value='-device'/><qemu:arg value='vhost-user-vsock-pci,chardev=tilovsock'/>\
</qemu:commandline>"
    )
}

/// The link to the guest for a backend.
#[must_use]
pub fn hybrid_link(uds: &Path) -> AgentLink {
    AgentLink::Hybrid {
        socket: uds.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vsock_backend_falls_back_to_the_user_space_daemon() {
        // phase: the kernel device wins, the daemon is the fallback
        {
            assert_eq!(VsockBackend::choose(true, true), Some(VsockBackend::Kernel));
            assert_eq!(
                VsockBackend::choose(false, true),
                Some(VsockBackend::Hybrid)
            );
            assert_eq!(VsockBackend::choose(false, false), None);
            assert!(VsockBackend::Kernel.snapshots());
            assert!(!VsockBackend::Hybrid.snapshots());
        }
        // phase: the daemon and QEMU agree on one socket
        {
            let vhu = Path::new("/run/user/1000/tilo-legacy/linux/vsock-vhu.sock");
            let uds = Path::new("/run/user/1000/tilo-legacy/linux/vsock.sock");
            assert_eq!(
                vhost_device_vsock_argv(vhu, uds),
                [
                    "vhost-device-vsock",
                    "--vm",
                    "guest-cid=3,socket=/run/user/1000/tilo-legacy/linux/vsock-vhu.sock,uds-path=/run/user/1000/tilo-legacy/linux/vsock.sock"
                ]
            );
            let xml = hybrid_commandline_xml(vhu);
            assert!(xml.contains("path=/run/user/1000/tilo-legacy/linux/vsock-vhu.sock"));
            assert!(xml.contains("vhost-user-vsock-pci,chardev=tilovsock"));
        }
    }
}

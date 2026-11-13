//! libvirt vocabulary shared by Tilo's virtual machines: the Windows appliance
//! on `qemu:///system` and the per-user legacy VMs on `qemu:///session`. Both
//! drive libvirt through `virsh` with typed arguments, never a shell.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::Path;

mod virsh;

pub use virsh::{
    BoundedVirsh, Virsh, VirshError, VirshOutput, VirshRunner, managed_save_in_dominfo,
};

/// The `<metadata>` namespace that marks a domain as Tilo's.
pub const METADATA_NAMESPACE: &str = "https://tilo.sh/vm/1";

#[must_use]
pub fn metadata_xml(kind: &str, attributes: &[(&str, &str)]) -> String {
    let mut marker = format!("<metadata><tilo:{kind} xmlns:tilo='{METADATA_NAMESPACE}'");
    for (name, value) in attributes {
        let _ = write!(marker, " {name}='{}'", xml_text(value));
    }
    marker + "/></metadata>"
}

/// Whether a defined domain carries the marker [`metadata_xml`] wrote; a
/// same-named domain without it is someone else's and is never touched. libvirt
/// re-serialises metadata with double quotes, so either quote matches.
#[must_use]
pub fn is_ours(domain_xml: &str, kind: &str, attributes: &[(&str, &str)]) -> bool {
    let Some(marker) = domain_xml.split(&format!("<tilo:{kind}")).nth(1) else {
        return false;
    };
    let marker = marker.split('>').next().unwrap_or_default();
    attributes.iter().all(|(name, value)| {
        let value = xml_text(value);
        marker.contains(&format!("{name}='{value}'"))
            || marker.contains(&format!("{name}=\"{value}\""))
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Connection {
    /// The system daemon, for the owner-managed Windows appliance.
    System,
    /// The calling user's unprivileged daemon: a domain here runs as the user and
    /// is invisible to other users.
    Session,
}

impl Connection {
    #[must_use]
    pub const fn uri(self) -> &'static str {
        match self {
            Self::System => "qemu:///system",
            Self::Session => "qemu:///session",
        }
    }

    /// `virsh --connect <uri> <args…>`, never a shell line.
    #[must_use]
    pub fn virsh_args<I, S>(self, args: I) -> Vec<OsString>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut command = vec![OsString::from("--connect"), OsString::from(self.uri())];
        command.extend(args.into_iter().map(Into::into));
        command
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainState {
    Absent,
    ShutOff,
    Running,
    Paused,
    Suspended,
    Starting,
    ShuttingDown,
    Crashed,
    Other,
}

impl DomainState {
    /// Unknown words are [`DomainState::Other`], which callers treat as unsafe to
    /// act on.
    #[must_use]
    pub fn from_domstate(output: &str) -> Self {
        match output
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "running" | "idle" => Self::Running,
            "in shutdown" | "shutting down" => Self::ShuttingDown,
            "starting" => Self::Starting,
            "shut off" => Self::ShutOff,
            "crashed" => Self::Crashed,
            "paused" => Self::Paused,
            "pmsuspended" => Self::Suspended,
            _ => Self::Other,
        }
    }

    #[must_use]
    pub fn listed(list_output: &str, name: &str) -> bool {
        list_output.lines().any(|line| line.trim() == name)
    }
}

/// The virtio-vsock device; libvirt picks a free context id. Ids are
/// host-global, so a fixed one would collide with another user's VM.
#[must_use]
pub const fn vsock_device_xml() -> &'static str {
    "<vsock model='virtio'><cid auto='yes'/></vsock>"
}

/// The context id from a domain's live `virsh dumpxml`, where libvirt fills
/// `address` in once it assigned one.
#[must_use]
pub fn vsock_cid(domain_xml: &str) -> Option<u32> {
    let vsock = domain_xml.split("<vsock").nth(1)?;
    let cid = vsock.split("<cid").nth(1)?;
    let cid = cid.split("/>").next()?;
    let address = cid.split("address=").nth(1)?;
    let quote = address.chars().next()?;
    address
        .strip_prefix(quote)?
        .split(quote)
        .next()?
        .parse()
        .ok()
        .filter(|cid| *cid > 2)
}

/// A path as XML text, or `None` when it is not UTF-8 (libvirt XML is).
#[must_use]
pub fn xml_path(path: &Path) -> Option<String> {
    Some(xml_text(path.to_str()?))
}

/// Text escaped for an XML attribute or element.
#[must_use]
pub fn xml_text(text: &str) -> String {
    quick_xml::escape::escape(text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_vocabulary_reads_virsh_and_writes_devices() {
        {
            assert_eq!(
                DomainState::from_domstate("running\n\n"),
                DomainState::Running
            );
            assert_eq!(DomainState::from_domstate("shut off"), DomainState::ShutOff);
            assert_eq!(
                DomainState::from_domstate("pmsuspended"),
                DomainState::Suspended
            );
            assert_eq!(DomainState::from_domstate("blocked"), DomainState::Other);
            assert!(DomainState::listed(
                "a\n tilo-legacy-linux \n",
                "tilo-legacy-linux"
            ));
            assert!(!DomainState::listed(
                "tilo-legacy-linux-2\n",
                "tilo-legacy-linux"
            ));
        }
        {
            assert_eq!(
                Connection::Session.virsh_args(["start", "x"]),
                ["--connect", "qemu:///session", "start", "x"].map(OsString::from)
            );
            assert_eq!(Connection::System.uri(), "qemu:///system");
        }
        {
            let live = "<devices><vsock model='virtio'><cid auto='yes' address='17'/><alias name='vsock0'/></vsock></devices>";
            assert_eq!(vsock_cid(live), Some(17));
            assert_eq!(
                vsock_cid("<vsock model='virtio'><cid auto='no' address='9'/></vsock>"),
                Some(9)
            );
            assert_eq!(vsock_cid(vsock_device_xml()), None);
            assert_eq!(vsock_cid("<vsock><cid address='2'/></vsock>"), None);
            assert_eq!(xml_text("a'b<c"), "a&apos;b&lt;c");
        }
        {
            let marker = metadata_xml("legacy-vm", &[("guest", "linux")]);
            assert_eq!(
                marker,
                "<metadata><tilo:legacy-vm xmlns:tilo='https://tilo.sh/vm/1' guest='linux'/></metadata>"
            );
            assert!(is_ours(&marker, "legacy-vm", &[("guest", "linux")]));
            assert!(is_ours(
                "<tilo:legacy-vm xmlns:tilo=\"https://tilo.sh/vm/1\" guest=\"linux\"/>",
                "legacy-vm",
                &[("guest", "linux")]
            ));
            assert!(!is_ours(&marker, "legacy-vm", &[("guest", "windows")]));
            assert!(!is_ours(&marker, "windows-appliance", &[]));
            assert!(!is_ours(
                "<tilo:legacy-vm/><x guest='linux'/>",
                "legacy-vm",
                &[("guest", "linux")]
            ));
            assert!(is_ours(
                &metadata_xml("windows-appliance", &[]),
                "windows-appliance",
                &[]
            ));
        }
    }
}

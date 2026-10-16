//! libvirt vocabulary shared by Tilo's virtual machines.
//!
//! The Windows appliance (`windows-vm`, `qemu:///system`) and the per-user
//! legacy VMs (`legacy-vm`, `qemu:///session`) drive libvirt through `virsh`
//! with typed arguments and never through a shell. What they have in common
//! lives here: which daemon a command talks to, how `virsh domstate` is read,
//! and the XML of the devices both domains carry.

use std::ffi::OsString;
use std::path::Path;

/// Which libvirt daemon a domain belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Connection {
    /// The privileged system daemon, for the owner-managed Windows appliance.
    System,
    /// The calling user's own unprivileged daemon. A domain here runs as the
    /// user, can only reach what the user can, and is invisible to other users.
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

    /// `virsh --connect <uri> <args…>` as an argument vector, never a shell line.
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

/// A domain's run state as `virsh domstate` reports it.
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
    /// Reads the first line of `virsh domstate` output. Unknown words are
    /// [`DomainState::Other`], which every caller treats as unsafe to act on.
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

    /// Whether `name` appears in `virsh list --all --name` output.
    #[must_use]
    pub fn listed(list_output: &str, name: &str) -> bool {
        list_output.lines().any(|line| line.trim() == name)
    }
}

/// The virtio-vsock device. `None` lets libvirt pick a free context id, which
/// is what an unprivileged domain must do: context ids are host-global, and a
/// fixed one would collide with another user's VM.
#[must_use]
pub fn vsock_device_xml(cid: Option<u32>) -> String {
    match cid {
        Some(cid) => format!("<vsock model='virtio'><cid auto='no' address='{cid}'/></vsock>"),
        None => "<vsock model='virtio'><cid auto='yes'/></vsock>".to_string(),
    }
}

/// The context id of a running domain, read from its live `virsh dumpxml`.
/// libvirt fills `address` in once it assigned one.
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
        // phase: domstate words map to states, anything else is unsafe
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
        // phase: session and system commands never share a connection
        {
            assert_eq!(
                Connection::Session.virsh_args(["start", "x"]),
                ["--connect", "qemu:///session", "start", "x"].map(OsString::from)
            );
            assert_eq!(Connection::System.uri(), "qemu:///system");
        }
        // phase: the vsock cid round-trips through live XML
        {
            assert_eq!(
                vsock_device_xml(None),
                "<vsock model='virtio'><cid auto='yes'/></vsock>"
            );
            let live = "<devices><vsock model='virtio'><cid auto='yes' address='17'/><alias name='vsock0'/></vsock></devices>";
            assert_eq!(vsock_cid(live), Some(17));
            assert_eq!(vsock_cid(&vsock_device_xml(Some(9))), Some(9));
            assert_eq!(
                vsock_cid("<vsock model='virtio'><cid auto='yes'/></vsock>"),
                None
            );
            assert_eq!(vsock_cid("<vsock><cid address='2'/></vsock>"), None);
            assert_eq!(xml_text("a'b<c"), "a&apos;b&lt;c");
        }
    }
}

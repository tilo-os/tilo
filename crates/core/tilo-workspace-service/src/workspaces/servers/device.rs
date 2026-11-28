//! A device's own machine, as its entry says it: its hostname, which drives it
//! uses for what, and how its network interfaces are set up.
//!
//! The entry is the one place this lives; the device applies it through the
//! root helper (`os/usr/libexec/tilo-server-device-config`), which the
//! daemon starts for by writing a request at a fixed path and which answers
//! with a report the daemon writes back into the entry's status, so the person
//! reads what was applied or why not.
//!
//! The request is one fixed line per setting, fields separated by tabs and
//! already checked by the entry's actions ([`super::Servers`]); the helper
//! checks every one again and passes none of it to a shell. Disks are named by
//! an identity that survives reboots and re-plugging — a serial or a WWN — so
//! the entry never depends on kernel device names.
//!
//! A network change can cut the device off from the person's devices, so the
//! helper keeps the keyfiles it replaced and puts them back unless the daemon
//! says it is still reached within a short window ([`confirm`]). The entry's
//! desired configuration stays as the person set it; the status says it was
//! reverted and why.
//!
//! Disks are only checked and recorded here: encrypting a drive and opening it
//! at boot is the image's own chain, which this request does not duplicate. A
//! drive that already holds data is refused unless the entry's action was given
//! the drive's identity to confirm erasing it.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tilo_resource::{Collection, Model, Refusal};

use super::Server;

/// What a drive is used for: the one boots from, the data pool, or a
/// mirror of the data pool.
pub const SYSTEM: &str = "system";
pub const DATA: &str = "data";
pub const MIRROR: &str = "mirror";
/// The roles a drive is given ([`SYSTEM`], [`DATA`], [`MIRROR`]).
pub const DISK_ROLES: [&str; 3] = [SYSTEM, DATA, MIRROR];

/// How an interface is addressed: by DHCP, or with the addresses the entry
/// names.
pub const DHCP: &str = "dhcp";
pub const STATIC: &str = "static";
/// The ways an interface is addressed ([`DHCP`], [`STATIC`]).
pub const ADDRESSING: [&str; 2] = [DHCP, STATIC];

/// What the helper said it did ([`Status::state`]): it applied the
/// configuration, refused it whole, or put the previous network back.
pub const REPORT_STATES: [&str; 3] = ["applied", "refused", "reverted"];

/// How long [`confirm`] waits for the helper to say it applied.
const APPLY_WAIT: Duration = Duration::from_secs(20);
/// How often [`confirm`] looks for that note.
const POLL: Duration = Duration::from_millis(100);
const MAX_HOSTNAME: usize = 253;
const MAX_LABEL: usize = 63;
const MAX_INTERFACE: usize = 15;
const MAX_IDENTITY: usize = 64;
const MAX_DETAIL: usize = 400;

/// A device's own machine, as its entry says it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DeviceConfig {
    /// The machine's hostname; empty for the one its image came with.
    pub hostname: String,
    /// The drives it uses, by stable identity (serial or WWN).
    pub disks: Collection<Disk>,
    /// Its network interfaces, by name.
    pub network: Collection<Interface>,
    /// What its machine last said about applying this.
    pub status: Status,
}

/// One drive the device uses, keyed by its identity.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Disk {
    /// What it is used for ([`DISK_ROLES`]).
    pub role: String,
    /// The person confirmed erasing what the drive holds, naming its
    /// identity in the action.
    pub erase: bool,
}

/// One network interface, keyed by its name.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Interface {
    /// How it is addressed ([`ADDRESSING`]).
    pub addressing: String,
    /// Its addresses as `address/prefix`, comma separated; empty for DHCP.
    pub addresses: String,
    /// Its default gateways, at most one for each address family, comma
    /// separated; empty for none.
    pub gateway: String,
    /// Its DNS servers, comma separated; empty for none.
    pub dns: String,
    /// The VLAN its name tags it with (`<parent>.<vlan>`); 0 for untagged.
    pub vlan: u16,
    /// The bond it is a member of; empty for none.
    pub bond: String,
}

/// What a device's machine said about applying its configuration.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Status {
    /// The revision of the request it answered.
    pub revision: String,
    /// What it did ([`REPORT_STATES`]); empty until it said.
    pub state: String,
    /// What it applied or why not, for the person.
    pub detail: String,
}

/// Where a device writes its request, reads the helper's report, and marks the
/// network change it keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub request: PathBuf,
    pub report: PathBuf,
    pub applied_at: PathBuf,
    pub confirmed: PathBuf,
}

impl Paths {
    /// The paths beside each other in `dir`.
    #[must_use]
    pub fn under(dir: &Path) -> Self {
        Self {
            request: dir.join("device-config.request"),
            report: dir.join("device-config.report"),
            applied_at: dir.join("device-config.applied-at"),
            confirmed: dir.join("device-config.confirmed"),
        }
    }
}

/// The hostname `text` names, trimmed and lowercased, or why it is no
/// hostname: dot-separated labels of letters, digits and hyphens, none empty,
/// none starting or ending with a hyphen.
pub(crate) fn valid_hostname(text: &str) -> Result<String, Refusal> {
    let hostname = text.trim().to_ascii_lowercase();
    let wrong =
        || Refusal::new("a hostname is labels of letters, digits and hyphens, 253 bytes at most");
    if hostname.is_empty() || hostname.len() > MAX_HOSTNAME {
        return Err(wrong());
    }
    for label in hostname.split('.') {
        if label.is_empty()
            || label.len() > MAX_LABEL
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(wrong());
        }
    }
    Ok(hostname)
}

/// The identity `text` names a drive by, trimmed, or why it is none: letters,
/// digits and `-._:`, so it is one field of the request and names nothing
/// that is not a drive.
pub(crate) fn valid_identity(text: &str) -> Result<String, Refusal> {
    let wrong = || Refusal::new("a drive is named by its serial or WWN");
    let identity = text.trim();
    if identity.is_empty()
        || identity.len() > MAX_IDENTITY
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._:".contains(&byte))
    {
        return Err(wrong());
    }
    Ok(identity.to_owned())
}

/// Whether `role` is a drive role this build knows.
pub(crate) fn valid_role(role: &str) -> Result<(), Refusal> {
    if !DISK_ROLES.contains(&role) {
        return Err(Refusal::new("a drive is used as system, data or mirror"));
    }
    Ok(())
}

/// The interface `name` set up as the entry says, or why it cannot be. Its name
/// carries the VLAN it is tagged with (`<parent>.<vlan>`, so 0 for untagged),
/// which is how `NetworkManager` names a tagged interface too.
pub(crate) fn valid_interface(
    name: &str,
    addressing: &str,
    addresses: &str,
    gateway: &str,
    dns: &str,
    bond: &str,
) -> Result<Interface, Refusal> {
    if !valid_ifname(name) {
        return Err(Refusal::new(
            "an interface is named by 1 to 15 letters, digits, dots, hyphens or underscores",
        ));
    }
    if !ADDRESSING.contains(&addressing) {
        return Err(Refusal::new("an interface is addressed by dhcp or static"));
    }
    let numbers = parse_addresses(addresses)?;
    let gateway = gateway.trim().to_owned();
    let dns = parse_dns(dns)?;
    let bond = bond.trim().to_owned();
    let vlan = vlan_of(name)?;
    if !bond.is_empty() && !valid_ifname(&bond) {
        return Err(Refusal::new("a bond is named like an interface"));
    }
    if bond == name {
        return Err(Refusal::new("an interface is not its own bond"));
    }
    if !bond.is_empty() {
        if addressing != DHCP || !numbers.is_empty() || !gateway.is_empty() || !dns.is_empty() {
            return Err(Refusal::new(
                "a bond member takes its addressing from the bond",
            ));
        }
        if vlan > 0 {
            return Err(Refusal::new("a bond member is not tagged; tag the bond"));
        }
    } else if addressing == STATIC && numbers.is_empty() {
        return Err(Refusal::new(
            "a static interface names at least one address",
        ));
    } else if addressing == DHCP && (!numbers.is_empty() || !gateway.is_empty()) {
        return Err(Refusal::new(
            "a dhcp interface takes no addresses of its own",
        ));
    }
    let gateways = parse_gateways(&gateway, &numbers)?;
    if vlan > 0 {
        let Some((parent, _)) = name.rsplit_once('.') else {
            return Err(Refusal::new(
                "a VLAN interface is named after its parent as <parent>.<vlan>",
            ));
        };
        if !valid_ifname(parent) {
            return Err(Refusal::new(
                "a VLAN interface is named after its parent as <parent>.<vlan>",
            ));
        }
    }
    let addresses = numbers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    Ok(Interface {
        addressing: addressing.to_owned(),
        addresses,
        gateway: gateways,
        dns: dns
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","),
        vlan,
        bond,
    })
}

/// What a machine's report says, or why it cannot be one.
pub(crate) fn valid_status(revision: &str, state: &str, detail: &str) -> Result<Status, Refusal> {
    // A request the helper could not read far enough to name a revision is
    // reported as `none`, so the person reads why it refused anyway.
    let named = revision == "none"
        || (revision.len() == 32
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    if !named {
        return Err(Refusal::new("a report names a revision of the request"));
    }
    if !REPORT_STATES.contains(&state) {
        return Err(Refusal::new("a report says applied, refused or reverted"));
    }
    let detail = detail.trim();
    if detail.len() > MAX_DETAIL || detail.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(Refusal::new(
            "a report's reason is one line of 400 bytes at most",
        ));
    }
    Ok(Status {
        revision: revision.to_owned(),
        state: state.to_owned(),
        detail: detail.to_owned(),
    })
}

fn valid_ifname(name: &str) -> bool {
    let mut bytes = name.bytes();
    !name.is_empty()
        && name.len() <= MAX_INTERFACE
        && !name.starts_with('-')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
}

/// The VLAN id an interface's name carries: the number after its last dot, 0
/// for a name without one.
fn vlan_of(name: &str) -> Result<u16, Refusal> {
    let Some((_, tag)) = name.rsplit_once('.') else {
        return Ok(0);
    };
    if tag.is_empty() || !tag.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(0);
    }
    tag.parse::<u16>()
        .ok()
        .filter(|vlan| (1..=4094).contains(vlan))
        .ok_or_else(|| Refusal::new("a VLAN id is 1 to 4094"))
}

/// An address as `address/prefix`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Address {
    ip: IpAddr,
    prefix: u8,
}

impl Address {
    const fn is_ipv4(self) -> bool {
        self.ip.is_ipv4()
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.ip, self.prefix)
    }
}

fn parse_addresses(text: &str) -> Result<Vec<Address>, Refusal> {
    let mut addresses: Vec<Address> = Vec::new();
    for address in text.split(',').map(str::trim).filter(|a| !a.is_empty()) {
        let (ip, prefix) = address
            .split_once('/')
            .ok_or_else(|| Refusal::new("an address is written as <address>/<prefix>"))?;
        let ip = ip
            .parse::<IpAddr>()
            .map_err(|_| Refusal::new("an address's address is an IP address"))?;
        let prefix = prefix
            .parse::<u8>()
            .map_err(|_| Refusal::new("an address's prefix is a number"))?;
        let widest = if ip.is_ipv4() { 32 } else { 128 };
        if prefix == 0 || prefix > widest {
            return Err(Refusal::new("an address's prefix fits its family"));
        }
        if addresses.iter().any(|known| known.ip == ip) {
            return Err(Refusal::new("an interface's addresses differ"));
        }
        addresses.push(Address { ip, prefix });
    }
    Ok(addresses)
}

/// The gateways `text` names, comma separated and normalized: an IP address
/// each, at most one for each family, and only for a family one of the
/// interface's `addresses` has, so a dual-stack link gets its IPv4 and its
/// IPv6 default route at once.
fn parse_gateways(text: &str, addresses: &[Address]) -> Result<String, Refusal> {
    let mut gateways: Vec<IpAddr> = Vec::new();
    for gateway in text.split(',').map(str::trim).filter(|one| !one.is_empty()) {
        let address = gateway
            .parse::<IpAddr>()
            .map_err(|_| Refusal::new("a gateway is an IP address"))?;
        if gateways
            .iter()
            .any(|known| known.is_ipv4() == address.is_ipv4())
        {
            return Err(Refusal::new(
                "an interface has one gateway for each address family",
            ));
        }
        if !addresses
            .iter()
            .any(|held| held.is_ipv4() == address.is_ipv4())
        {
            return Err(Refusal::new(
                "a gateway has the family of one of the interface's addresses",
            ));
        }
        gateways.push(address);
    }
    Ok(gateways
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(","))
}

fn parse_dns(text: &str) -> Result<Vec<IpAddr>, Refusal> {
    text.split(',')
        .map(str::trim)
        .filter(|server| !server.is_empty())
        .map(|server| {
            server
                .parse::<IpAddr>()
                .map_err(|_| Refusal::new("a DNS server is an IP address"))
        })
        .collect()
}

/// The request `entry` asks for: its revision, and the document the helper
/// reads.
fn request(entry: &Server) -> Result<(String, String), String> {
    let mut body = String::new();
    if !entry.device.hostname.is_empty() {
        let _ = writeln!(body, "hostname|{}", entry.device.hostname);
    }
    for (identity, disk) in entry.device.disks.iter() {
        let _ = writeln!(
            body,
            "disk|{}|{}|{}",
            disk.role,
            identity.as_str(),
            u8::from(disk.erase)
        );
    }
    for (name, interface) in entry.device.network.iter() {
        let _ = writeln!(
            body,
            "interface|{}|{}|{}|{}|{}|{}|{}",
            name.as_str(),
            interface.addressing,
            interface.addresses,
            interface.gateway,
            interface.dns,
            interface.bond,
            interface.vlan
        );
    }
    let hash = blake3::hash(body.as_bytes()).to_hex();
    let revision = hash
        .get(..32)
        .ok_or_else(|| "a revision is 32 hex digits".to_owned())?
        .to_owned();
    let document = format!("revision|{revision}\n{body}");
    Ok((revision, document))
}

/// Write what `entry` says its machine is to `paths.request`, the fixed request
/// the root helper applies. Answers the revision it wrote, or `None` when
/// the request is the one already there.
///
/// # Errors
/// The entry names a configuration this build does not write, or the request
/// could not be written.
pub fn write(entry: &Server, paths: &Paths) -> Result<Option<String>, String> {
    let (revision, document) = request(entry)?;
    if std::fs::read(&paths.request).ok().as_deref() == Some(document.as_bytes()) {
        return Ok(None);
    }
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &paths.request,
        document.as_bytes(),
        0o600,
    )
    .map_err(|error| error.to_string())?;
    Ok(Some(revision))
}

/// What the helper last said it did, when it said it of a request at all.
#[must_use]
pub fn report(paths: &Paths) -> Option<Status> {
    let text = std::fs::read_to_string(&paths.report).ok()?;
    let mut status = Status::default();
    for line in text.lines() {
        let (key, value) = line.split_once('\t')?;
        match key {
            "revision" => value.clone_into(&mut status.revision),
            "state" => value.clone_into(&mut status.state),
            "detail" => value.clone_into(&mut status.detail),
            _ => return None,
        }
    }
    (!status.revision.is_empty() && !status.state.is_empty()).then_some(status)
}

/// Whether the request at `paths` asks for a network change, which the
/// root helper keeps only once the device is reached again ([`confirm`]).
#[must_use]
pub fn awaits_confirmation(paths: &Paths) -> bool {
    std::fs::read_to_string(&paths.request)
        .is_ok_and(|request| request.lines().any(|line| line.starts_with("interface|")))
}

/// Tell the root helper to keep the network configuration it applied for
/// `revision`: wait for its note that it applied it, then for this device to be
/// reached through a relay again, and only then write the confirmation. When
/// this gives up, the helper puts the previous configuration back and says so
/// in its report.
pub async fn confirm(paths: Paths, endpoint: iroh::Endpoint, revision: String) {
    let applied = tokio::time::timeout(APPLY_WAIT, async {
        while !names(&paths.applied_at, &revision) {
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    if applied.is_err() {
        tracing::info!(%revision, "the device configuration helper did not apply in time");
        return;
    }
    let address = tilo_endpoint_reachability::await_relay(&endpoint).await;
    if address.relay_urls().next().is_none() {
        tracing::warn!(
            %revision,
            "no relay is reached after the device configuration changed; it is reverted"
        );
        return;
    }
    let confirmation = format!("revision\t{revision}\n");
    if let Err(error) = tilo_files::native::atomic::write_creating_parent_with_mode(
        &paths.confirmed,
        confirmation.as_bytes(),
        0o600,
    )
    .map_err(|error| error.to_string())
    {
        tracing::warn!(%error, %revision, "the device configuration was not confirmed");
    }
}

/// Whether `path` is the note that `revision` was applied.
fn names(path: &Path, revision: &str) -> bool {
    std::fs::read_to_string(path).ok().as_deref()
        == Some(format!("revision\t{revision}\n").as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Id;

    fn server() -> Server {
        let mut entry = Server::default();
        entry.device.disks.push(
            Id::from_raw("SN123"),
            Disk {
                role: SYSTEM.into(),
                erase: false,
            },
        );
        entry.device.network.push(
            Id::from_raw("enp2"),
            Interface {
                addressing: STATIC.into(),
                addresses: "10.0.0.5/24".into(),
                gateway: "10.0.0.1".into(),
                dns: "1.1.1.1,9.9.9.9".into(),
                vlan: 0,
                bond: "bond0".into(),
            },
        );
        entry
    }

    /// The request is exactly the entry's values, one fixed line per setting,
    /// and asks for nothing when the entry names nothing.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the assertions are the test; its errors are the setup returning them"
    )]
    fn the_request_is_exactly_the_entrys_values() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = Paths::under(dir.path());
        let mut entry = server();
        entry.device.hostname = "nas".into();
        // A link addressed in both families keeps a default route for each.
        let dual = valid_interface(
            "enp3s0",
            STATIC,
            "65.108.204.148/26,2a01:4f9:1a:a083::2/64",
            "65.108.204.129,fe80::1",
            "185.12.64.1,185.12.64.2",
            "",
        )
        .expect("one gateway for each family");
        entry.device.network.push(Id::from_raw("enp3s0"), dual);
        let revision = write(&entry, &paths)
            .map_err(std::io::Error::other)?
            .expect("the first write changes the request");
        assert_eq!(
            std::fs::read_to_string(&paths.request)?,
            format!(
                "revision|{revision}\nhostname|nas\ndisk|system|SN123|0\n\
                 interface|enp2|static|10.0.0.5/24|10.0.0.1|1.1.1.1,9.9.9.9|bond0|0\n\
                 interface|enp3s0|static|65.108.204.148/26,2a01:4f9:1a:a083::2/64|\
                 65.108.204.129,fe80::1|185.12.64.1,185.12.64.2||0\n"
            )
        );
        assert_eq!(revision.len(), 32);
        assert!(awaits_confirmation(&paths), "a network change is confirmed");
        assert_eq!(
            write(&entry, &paths).map_err(std::io::Error::other)?,
            None,
            "the same request writes nothing"
        );
        entry.device.hostname = "attic".into();
        let moved = write(&entry, &paths)
            .map_err(std::io::Error::other)?
            .expect("a change writes");
        assert_ne!(moved, revision, "another configuration is another revision");
        assert!(
            std::fs::read_to_string(&paths.request)?.starts_with(&format!("revision|{moved}\n"))
        );
        let empty = Server::default();
        write(&empty, &paths)
            .map_err(std::io::Error::other)?
            .expect("an entry naming nothing writes an empty request");
        assert!(!awaits_confirmation(&paths), "nothing to confirm");
        assert_eq!(
            std::fs::read_to_string(&paths.request)?
                .lines()
                .skip(1)
                .count(),
            0
        );
        Ok(())
    }

    /// A report is read as the machine wrote it; a report naming anything else
    /// is not one.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the assertions are the test; its errors are the setup returning them"
    )]
    fn the_report_is_read_as_written() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = Paths::under(dir.path());
        assert!(report(&paths).is_none(), "nothing said yet");
        let revision = "0123456789abcdef0123456789abcdef";
        std::fs::write(
            &paths.report,
            format!(
                "revision\t{revision}\nstate\treverted\ndetail\tno device reached this machine\n"
            ),
        )?;
        let status = report(&paths).expect("a report");
        assert_eq!(
            status,
            Status {
                revision: revision.into(),
                state: "reverted".into(),
                detail: "no device reached this machine".into(),
            }
        );
        std::fs::write(&paths.report, "surprise\tvalue\n")?;
        assert!(report(&paths).is_none());
        Ok(())
    }

    /// A configuration the entry cannot mean is refused before anything is
    /// written.
    #[test]
    fn a_configuration_that_cannot_be_meant_is_refused() {
        assert!(valid_hostname("nas.attic").is_ok());
        assert_eq!(valid_hostname(" NAS ").unwrap(), "nas");
        assert!(valid_hostname("a.b.c").is_ok());
        for hostname in ["", "-nas", "nas-", "nas..attic", "na s"] {
            assert!(valid_hostname(hostname).is_err(), "{hostname}");
        }
        assert!(valid_hostname(&"a".repeat(MAX_LABEL + 1)).is_err());
        assert!(valid_identity("WD-WMC4N1234567").is_ok());
        assert!(valid_identity("wwn-0x5000c500a").is_ok());
        for identity in ["", "a b", "a\tb", &"a".repeat(MAX_IDENTITY + 1)] {
            assert!(valid_identity(identity).is_err(), "{identity}");
        }
        assert!(valid_role(DATA).is_ok());
        assert!(valid_role("spare").is_err());
    }

    /// An interface is refused an addressing it cannot have: a static one with
    /// no address, a DHCP one with addresses of its own, a bond member with
    /// addressing of its own, a gateway of another family, and a VLAN id
    /// outside the taggable range.
    #[test]
    fn an_interface_that_cannot_be_addressed_is_refused() {
        let ok = valid_interface(
            "enp2",
            STATIC,
            "10.0.0.5/24,2001:db8::5/64",
            "10.0.0.1",
            "1.1.1.1",
            "",
        );
        assert_eq!(
            ok.unwrap(),
            Interface {
                addressing: STATIC.into(),
                addresses: "10.0.0.5/24,2001:db8::5/64".into(),
                gateway: "10.0.0.1".into(),
                dns: "1.1.1.1".into(),
                vlan: 0,
                bond: String::new(),
            }
        );
        assert!(valid_interface("eno1", DHCP, "", "", "1.1.1.1", "").is_ok());
        let dual_stack = valid_interface(
            "enp3s0",
            STATIC,
            "65.108.204.148/26,2a01:4f9:1a:a083::2/64",
            "65.108.204.129,fe80::1",
            "185.12.64.1,185.12.64.2",
            "",
        );
        assert_eq!(
            dual_stack
                .expect("one gateway for each family at once")
                .gateway,
            "65.108.204.129,fe80::1"
        );
        let tagged = valid_interface("eno1.20", STATIC, "10.0.0.5/24", "", "", "");
        assert_eq!(tagged.unwrap().vlan, 20, "the name carries the VLAN");
        assert!(valid_interface("bond0", DHCP, "", "", "", "").is_ok());
        assert!(valid_interface("eno2", DHCP, "", "", "", "bond0").is_ok());
        for (name, addressing, addresses, gateway, dns, bond) in [
            ("enp2", STATIC, "", "", "", ""),
            ("enp2", DHCP, "10.0.0.5/24", "", "", ""),
            ("enp2", DHCP, "", "10.0.0.1", "", ""),
            ("enp2", STATIC, "10.0.0.5/24", "", "", "bond0"),
            ("enp2.20", DHCP, "", "", "", "bond0"),
            ("enp2", STATIC, "10.0.0.5/24", "2001:db8::1", "", ""),
            (
                "enp2",
                STATIC,
                "10.0.0.5/24,2001:db8::5/64",
                "10.0.0.1,10.0.0.2",
                "",
                "",
            ),
            (
                "enp2",
                STATIC,
                "10.0.0.5/24,2001:db8::5/64",
                "10.0.0.1,2001:db8::1,fe80::1",
                "",
                "",
            ),
            ("enp2", STATIC, "10.0.0.5", "", "", ""),
            ("enp2", STATIC, "10.0.0.5/0", "", "", ""),
            ("enp2.4095", DHCP, "", "", "", ""),
            ("bond0", DHCP, "", "", "", "bond0"),
            ("an-interface-name-that-is-too-long", DHCP, "", "", "", ""),
            ("enp2", "dhcpv6", "", "", "", ""),
            ("enp2", DHCP, "", "", "not-an-ip", ""),
        ] {
            assert!(
                valid_interface(name, addressing, addresses, gateway, dns, bond).is_err(),
                "{name} {addressing} {addresses} {gateway} {dns} {bond}"
            );
        }
    }

    /// A report says one of the things a machine can say, of a revision of a
    /// request, in one line for the person.
    #[test]
    fn a_report_says_only_what_a_machine_can() {
        let revision = "0123456789abcdef0123456789abcdef";
        assert!(valid_status(revision, "applied", "hostname, network, disks").is_ok());
        assert!(valid_status("abc", "applied", "").is_err());
        assert!(valid_status(revision, "done", "").is_err());
        assert!(valid_status(revision, "refused", "two\nlines").is_err());
    }

    /// The helper applies the network only for the revision it applied, so a
    /// confirmation of another request confirms nothing.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the assertions are the test; its errors are the setup returning them"
    )]
    fn a_confirmation_names_the_revision() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = Paths::under(dir.path());
        tilo_files::native::atomic::write_creating_parent_with_mode(
            &paths.applied_at,
            b"revision\tone\n",
            0o600,
        )
        .map_err(std::io::Error::other)?;
        assert!(names(&paths.applied_at, "one"));
        assert!(!names(&paths.applied_at, "two"));
        Ok(())
    }
}

//! Privileged capability declarations — the manifest's `[capabilities]`
//! block.
//!
//! A qOS app runs in a per-workspace sandbox with no route to the kernel
//! beyond what the user granted it as filesystem paths, devices or network
//! hosts. Some system apps need more than that: a firewall front-end must
//! change firewall rules, a disk tool must write a block device, a VPN app
//! must bring up a tunnel. Those are **privileged operations** — they touch
//! state outside any workspace, so they cannot be a bind mount the sandbox
//! carries, and they must never be performed by the sandboxed app itself.
//!
//! This block is the *declaration* half. It states, at install time, exactly
//! which privileged operations the package will ask the broker to perform on
//! its behalf. It is a **ceiling**, like the rest of `[sandbox]`: it does not
//! grant anything. At runtime the app sends a typed request to the trusted
//! [privileged broker](../../qos-privileged-broker); the broker refuses any
//! capability this block did not declare, then refuses again unless the user
//! has consented in the workspace. Deny-by-default at every layer.
//!
//! ## Why a typed enum, not free strings
//!
//! An unknown capability name fails TOML decode rather than being silently
//! ignored — a package cannot smuggle a privileged operation past the
//! validator by misspelling it, and a future capability the running system
//! does not understand refuses to install rather than installing as a no-op
//! that a later broker might honour. Deny-by-default begins at the parser.
//!
//! ## The bridge to the runtime capability model
//!
//! A declared capability maps to one [`collab_types::caps::Scope`] via
//! [`PrivilegedCapability::scope`]. That is the single point where the
//! packaging vocabulary meets the runtime vocabulary: the user's consent is a
//! `CapOverride` on that scope, stored on the workspace exactly like a camera
//! or network grant, shown in the same Permissions tab, and evaluated by the
//! same `decide_capability`. There is deliberately no second permission type
//! for privileged operations — a second vocabulary is how the declared grant
//! and the enforced grant drift apart.

use serde::{Deserialize, Serialize};

use collab_types::caps::{Scope, Verb};

/// The scope-name prefix that marks a [`Scope::Device`] as a privileged
/// capability rather than a hardware device class.
///
/// Reusing `Scope::Device` — the scope the portal already brokers for
/// camera / microphone / screencast — is deliberate: a privileged capability
/// is exactly "a system capability the broker brokers", it carries no role
/// base authority (so it always requires an explicit grant), and it binds no
/// path into the sandbox (the broker does the work, not a mount). The prefix
/// keeps the two device namespaces from ever colliding.
pub const PRIVILEGED_SCOPE_PREFIX: &str = "privileged:";

/// The verb a privileged operation requires, and that a consent grant
/// therefore confers.
///
/// Every privileged operation is governance-level: it mutates state outside
/// any workspace. [`Verb::Admin`] is the ceiling of the verb ladder, so a
/// grant phrased at any lower verb does not satisfy it — a user who shared a
/// document read-only has not thereby authorised a firewall change.
pub const PRIVILEGED_VERB: Verb = Verb::Admin;

/// A privileged system operation an app may perform only through the trusted
/// broker, never itself.
///
/// The wire form is a plain kebab-case string (`capability = "firewall-admin"`).
/// The variant set is closed on purpose: adding a capability is a deliberate,
/// reviewed change to the broker and this enum together, never a string a
/// package author invents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrivilegedCapability {
    /// Modify the host firewall (firewalld / nftables rules).
    FirewallAdmin,
    /// Read or partition a whole block device (mount, format, benchmark).
    BlockDevice,
    /// Write raw bytes to a block device node (flashing a disk image).
    RawDeviceWrite,
    /// Drive the container runtime (rootless podman / distrobox).
    ContainerRuntime,
    /// Create and configure a TUN device and its routes (a VPN tunnel).
    VpnTun,
    /// Capture packets off a network interface (a promiscuous socket).
    PacketCapture,
}

impl PrivilegedCapability {
    /// Every capability, for exhaustive iteration (the audit CLI, tests).
    pub const ALL: [PrivilegedCapability; 6] = [
        PrivilegedCapability::FirewallAdmin,
        PrivilegedCapability::BlockDevice,
        PrivilegedCapability::RawDeviceWrite,
        PrivilegedCapability::ContainerRuntime,
        PrivilegedCapability::VpnTun,
        PrivilegedCapability::PacketCapture,
    ];

    /// The kebab-case wire name, the same string the TOML carries.
    pub fn wire(self) -> &'static str {
        match self {
            PrivilegedCapability::FirewallAdmin => "firewall-admin",
            PrivilegedCapability::BlockDevice => "block-device",
            PrivilegedCapability::RawDeviceWrite => "raw-device-write",
            PrivilegedCapability::ContainerRuntime => "container-runtime",
            PrivilegedCapability::VpnTun => "vpn-tun",
            PrivilegedCapability::PacketCapture => "packet-capture",
        }
    }

    /// Parse a wire name back into a capability. `None` for anything not in
    /// the closed set.
    pub fn from_wire(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.wire() == s)
    }

    /// A short human phrase for the consent prompt, completing "Allow
    /// <App> to …?".
    pub fn consent_phrase(self) -> &'static str {
        match self {
            PrivilegedCapability::FirewallAdmin => "change firewall rules",
            PrivilegedCapability::BlockDevice => "manage disks and partitions",
            PrivilegedCapability::RawDeviceWrite => "write directly to a disk",
            PrivilegedCapability::ContainerRuntime => "run containers",
            PrivilegedCapability::VpnTun => "create a VPN tunnel",
            PrivilegedCapability::PacketCapture => "capture network traffic",
        }
    }

    /// The runtime [`Scope`] a consent grant for this capability names.
    ///
    /// This is the one bridge from the packaging vocabulary to the capability
    /// model: the compositor stores the user's consent as a `CapOverride` on
    /// exactly this scope, and the broker asks `decide_capability` about it.
    pub fn scope(self) -> Scope {
        Scope::Device(format!("{PRIVILEGED_SCOPE_PREFIX}{}", self.wire()))
    }

    /// Recover a capability from the scope it maps to, if that scope is a
    /// privileged-capability device. Inverse of [`Self::scope`].
    pub fn from_scope(scope: &Scope) -> Option<Self> {
        match scope {
            Scope::Device(name) => {
                Self::from_wire(name.strip_prefix(PRIVILEGED_SCOPE_PREFIX)?)
            }
            _ => None,
        }
    }
}

/// One `[[capabilities.privileged]]` entry: a declared capability, optionally
/// narrowed to a concrete target and annotated with a reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivilegedRequest {
    /// Which privileged operation the package needs.
    pub capability: PrivilegedCapability,
    /// An optional narrowing of the capability to a concrete target where
    /// the operation supports one — a device node (`/dev/sdb`) for
    /// `block-device` / `raw-device-write`, an interface for `packet-capture`
    /// or `vpn-tun`. `None` means the whole capability (the firewall, any
    /// disk). The broker treats a declared scope as a further ceiling; it is
    /// advisory today for capabilities whose appliers are staged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// A human explanation surfaced in the consent prompt and the audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PrivilegedRequest {
    /// A whole-capability request with no scope narrowing.
    pub fn new(capability: PrivilegedCapability) -> Self {
        Self {
            capability,
            scope: None,
            reason: None,
        }
    }
}

/// The `[capabilities]` block — the privileged operations a package declares.
///
/// Absent from a manifest means "declares nothing", which is the default and
/// the only shape an ordinary app has. A non-empty block is the loud,
/// reviewable signal that a package is a privileged system app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// `[[capabilities.privileged]]` repeated array.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub privileged: Vec<PrivilegedRequest>,
}

impl Capabilities {
    /// Whether the package declared no privileged capabilities — the only
    /// shape re-serialised, so a manifest that had no block keeps having none.
    pub fn is_empty(&self) -> bool {
        self.privileged.is_empty()
    }

    /// Does the package declare `capability`?
    ///
    /// The broker's ceiling check: a request for a capability this returns
    /// `false` for is denied before any consent is considered.
    pub fn declares(&self, capability: PrivilegedCapability) -> bool {
        self.privileged.iter().any(|r| r.capability == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        for cap in PrivilegedCapability::ALL {
            assert_eq!(PrivilegedCapability::from_wire(cap.wire()), Some(cap));
        }
        assert_eq!(PrivilegedCapability::from_wire("nonsense"), None);
    }

    #[test]
    fn scope_round_trips_and_is_a_privileged_device() {
        for cap in PrivilegedCapability::ALL {
            let scope = cap.scope();
            match &scope {
                Scope::Device(name) => {
                    assert!(name.starts_with(PRIVILEGED_SCOPE_PREFIX));
                }
                other => panic!("privileged scope must be a Device scope, got {other:?}"),
            }
            assert_eq!(PrivilegedCapability::from_scope(&scope), Some(cap));
        }
        // A plain hardware device is not a privileged capability.
        assert_eq!(
            PrivilegedCapability::from_scope(&Scope::Device("camera".into())),
            None
        );
        assert_eq!(
            PrivilegedCapability::from_scope(&Scope::FsPath("/etc".into())),
            None
        );
    }

    #[test]
    fn declares_answers_membership() {
        let caps = Capabilities {
            privileged: vec![PrivilegedRequest::new(PrivilegedCapability::FirewallAdmin)],
        };
        assert!(caps.declares(PrivilegedCapability::FirewallAdmin));
        assert!(!caps.declares(PrivilegedCapability::BlockDevice));
    }

    #[test]
    fn unknown_capability_string_fails_decode() {
        // Deny-by-default begins at the parser: a capability the system does
        // not know is a decode error, not a silently-ignored no-op.
        let src = r#"
[[privileged]]
capability = "root-shell"
"#;
        let decoded: Result<Capabilities, _> = toml::from_str(src);
        assert!(
            decoded.is_err(),
            "an unknown privileged capability must not decode"
        );
    }

    #[test]
    fn scope_and_reason_are_optional_and_round_trip() {
        let src = r#"
[[privileged]]
capability = "raw-device-write"
scope = "/dev/sdb"
reason = "flash a disk image"
"#;
        let caps: Capabilities = toml::from_str(src).unwrap();
        assert_eq!(caps.privileged.len(), 1);
        let req = &caps.privileged[0];
        assert_eq!(req.capability, PrivilegedCapability::RawDeviceWrite);
        assert_eq!(req.scope.as_deref(), Some("/dev/sdb"));
        assert_eq!(req.reason.as_deref(), Some("flash a disk image"));
    }
}

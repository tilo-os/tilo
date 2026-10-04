//! The capability descriptors an app asks for.

use serde::{Deserialize, Serialize};

use collab_types::caps::{CapOverride, Expiry, Scope, Verb};

/// Filesystem-grant access mode. Mirrors the read / read-write split
/// the portal D-Bus backend surfaces to apps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsMode {
    Read,
    ReadWrite,
}

/// How long a capability grant lives — [`collab_types::caps::Expiry`]
/// under the prompt UI's name. "Just once / Until I log out / Always"
/// are `AfterUse` / `EndOfSession` / `Never`, and the same three values
/// travel to the filesystem, the launcher sandbox and every peer.
pub type GrantScope = Expiry;

/// "Just once" — the prompt's one-shot button.
pub const GRANT_ONCE: Expiry = Expiry::AfterUse;
/// "Until I log out" — held for this shell session, never written to disk.
pub const GRANT_SESSION: Expiry = Expiry::EndOfSession;
/// "Always" — survives a restart.
pub const GRANT_ALWAYS: Expiry = Expiry::Never;

/// Per-app capability grant stored on a [`Workspace`](crate::workspace::Workspace). Each variant maps
/// to its own enforcement layer: Bubblewrap allowlist for filesystem,
/// Pipewire ACL for camera/mic, nftables for network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    /// Read or read-write access to a specific filesystem path.
    Filesystem {
        path: std::path::PathBuf,
        mode: FsMode,
        scope: GrantScope,
    },
    /// Camera (`/dev/video*`) access.
    Camera { scope: GrantScope },
    /// Microphone (Pipewire socket) access.
    Microphone { scope: GrantScope },
    /// Screen capture via portal `ScreenCast`.
    Screencast { scope: GrantScope },
    /// Remote control of the seat (pointer + keyboard injection) via
    /// portal `RemoteDesktop`. Separate from [`Capability::Screencast`]
    /// because it is strictly more invasive: "let them watch" and "let
    /// them type" stay separate grants.
    RemoteDesktop { scope: GrantScope },
    /// Submit a document to a printer via portal `Print`. Its own
    /// variant because CUPS enumerates destinations and manages other
    /// users' jobs, so "may print" and "may reach the print system" are
    /// not the same permission.
    ///
    /// Brokered, never bound: the launcher's grant-to-bind mapping
    /// (`launcher::capabilities`) has no print binding, so
    /// `/run/cups/cups.sock` never appears inside the sandbox however
    /// broadly this is granted; printing goes through the `Print` portal.
    Print { scope: GrantScope },
    /// Outbound network access. `hosts: None` = any host; `Some(list)` =
    /// only those.
    ///
    /// **Enforcement gap:** only the per-workspace network namespace
    /// isolation is wired up. The `hosts` allowlist is recorded but not
    /// enforced — no per-namespace nftables rules yet, so `Some([...])`
    /// and `None` behave identically.
    Network {
        hosts: Option<Vec<String>>,
        scope: GrantScope,
    },
    /// Terminal opening the device's own login shell, unconfined. Asked once
    /// by the shell runtime's Host target; answered into its policy, never
    /// bound into a sandbox.
    HostShell { scope: GrantScope },
}

impl Capability {
    /// The `(scope, verb)` this request descriptor asks for —
    /// [`Capability`] is what an app *asks* for, granting one writes
    /// [`CapOverride`]s. A host allowlist becomes one capability per
    /// host, so dropping one is a meaningful act.
    pub fn requested_scopes(&self) -> Vec<(Scope, Verb)> {
        match self {
            Capability::Filesystem { path, mode, .. } => {
                let verb = match mode {
                    FsMode::Read => Verb::Read,
                    FsMode::ReadWrite => Verb::Write,
                };
                vec![(Scope::FsPath(path.to_string_lossy().into_owned()), verb)]
            }
            Capability::Camera { .. } => vec![(Scope::Device("camera".into()), Verb::Read)],
            Capability::Microphone { .. } => {
                vec![(Scope::Device("microphone".into()), Verb::Read)]
            }
            Capability::Screencast { .. } => {
                vec![(Scope::Device("screencast".into()), Verb::Read)]
            }
            // Remote desktop is screencast plus input injection, so it
            // gets its own write-verb device scope — revoking one must
            // not revoke the other.
            Capability::RemoteDesktop { .. } => {
                vec![(Scope::Device("remote-desktop".into()), Verb::Write)]
            }
            // Printing is an *output* action, producing a physical
            // artifact, so it asks for the write verb, not the read
            // one a device probe would use.
            Capability::Print { .. } => vec![(Scope::Device("printer".into()), Verb::Write)],
            Capability::HostShell { .. } => {
                vec![(Scope::Device("host-shell".into()), Verb::Write)]
            }
            Capability::Network { hosts, .. } => match hosts {
                // An allowlist means exactly those hosts; `None` is the
                // only thing that produces a wildcard scope — behaving
                // as if an allowlist said `None` is the bug this shape
                // prevents.
                Some(list) => list
                    .iter()
                    .map(|h| (Scope::NetHost(h.clone()), Verb::Write))
                    .collect(),
                None => vec![(Scope::NetHost("*".into()), Verb::Write)],
            },
        }
    }

    pub fn requested_expiry(&self) -> Expiry {
        match self {
            Capability::Filesystem { scope, .. }
            | Capability::Camera { scope }
            | Capability::Microphone { scope }
            | Capability::Screencast { scope }
            | Capability::RemoteDesktop { scope }
            | Capability::Print { scope }
            | Capability::HostShell { scope }
            | Capability::Network { scope, .. } => *scope,
        }
    }

    /// Rebuild a descriptor from a stored capability, for the
    /// Permissions tab. `None` when the capability is not an app grant
    /// this UI knows how to render.
    pub fn from_capability(cap: &CapOverride) -> Option<Self> {
        let scope = cap.expiry;
        match &cap.scope {
            Scope::FsPath(p) => Some(Capability::Filesystem {
                path: std::path::PathBuf::from(p),
                mode: if cap.verb.implies(Verb::Write) {
                    FsMode::ReadWrite
                } else {
                    FsMode::Read
                },
                scope,
            }),
            Scope::Device(d) if d == "camera" => Some(Capability::Camera { scope }),
            Scope::Device(d) if d == "microphone" => Some(Capability::Microphone { scope }),
            Scope::Device(d) if d == "screencast" => Some(Capability::Screencast { scope }),
            // Without this arm a stored print grant wouldn't read back
            // as one: `GrantStoreHandle::lookup` would miss it and an
            // `Always` grant would re-prompt every job despite looking
            // granted in the Permissions tab.
            Scope::Device(d) if d == "printer" => Some(Capability::Print { scope }),
            Scope::Device(d) if d == "host-shell" => Some(Capability::HostShell { scope }),
            Scope::NetHost(h) => Some(Capability::Network {
                hosts: if h == "*" {
                    None
                } else {
                    Some(vec![h.clone()])
                },
                scope,
            }),
            _ => None,
        }
    }
}

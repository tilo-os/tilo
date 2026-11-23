//! The sandbox: what code may reach outside the documents it publishes —
//! filesystem paths, collaborative file records, devices and network hosts — as
//! `(who × scope × access)` capabilities.
//!
//! A capability names a [`Grantee`], a member or a role, and names an app when it
//! is that app's sandbox. Granted at install and through portals, and never what a
//! person grants a member: who may invoke which action is decided by
//! `tilo-action-policy` over the published tree.
//!
//! [`decide_capability`] is fail-closed and deny-biased: no matching grant
//! denies, and a matching `Restrict` caps access at its own [`Access`] even if
//! a `Grant` would allow more. `Read` is the floor: a `Restrict` makes a scope
//! read-only and never takes reading away. To keep a path, device, host or
//! record from an app, do not grant it.

use serde::{Deserialize, Serialize};
pub mod net;
pub mod sandbox;

pub use net::{Allowed, EgressPolicy, HostPattern, NetHostError, NetHostPattern, host_covers};
use tilo_action_policy::{Acting, Grantee, Lifetime, MemberId};

/// How far a sandbox capability reaches into its resource.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    tilo_resource::Model,
)]
#[serde(deny_unknown_fields)]
pub enum Access {
    /// Read, observe, capture. The floor: `Write` implies it, so no
    /// [`Effect::Restrict`] removes it. Only the absence of a grant does.
    Read,
    /// Change, send, drive. Implies `Read`.
    Write,
}

impl Access {
    /// Does holding `self` reach `other`?
    pub fn implies(self, other: Access) -> bool {
        self >= other
    }
}

/// What a sandbox capability ranges over.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub enum Scope {
    /// A filesystem path and everything below it.
    FsPath(String),
    /// A device of the machine the app is on.
    Device(DeviceCap),
    /// A network host pattern; see [`net`].
    NetHost(String),
    /// A stable collaborative-file record id, or `*` for every record.
    FsResource(String),
}

/// The scope pattern a [`Scope::NetHost`] or [`Scope::FsResource`] uses for
/// every one.
pub const EVERY: &str = "*";

/// A device of the machine an app is on, as a sandbox capability names it.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    tilo_resource::Model,
)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceCap {
    /// The cameras (`/dev/video*`).
    Camera,
    /// The microphones (the `PipeWire` socket and `/dev/snd`).
    Microphone,
    /// The picture of the screen: a screenshot or a screen share.
    Screen,
    /// The keyboard and pointer: injecting input, or capturing it.
    Input,
    /// The sound the machine plays.
    SystemAudio,
    /// Printing through the `Print` portal. Brokered, never bound: the CUPS
    /// socket never appears in a sandbox.
    Printer,
    /// The device's own login shell, unconfined, as a terminal's Host target.
    HostShell,
    /// How busy the device is, from the session's system information service.
    SystemInfo,
    /// The programs running on the device, and ending the person's own.
    Processes,
    /// The command lines of those programs, which can name files, hosts or
    /// tokens.
    ProcessCommands,
    /// The kernel's `io_uring` interface, which the sandbox's syscall filter
    /// otherwise refuses.
    IoUring,
    /// The GPU itself, rather than through the compositor.
    Gpu,
    /// The clipboard of the person's desktop, as a Windows app's remote
    /// session redirects it.
    Clipboard,
    /// The desktop's actions beyond the app's own workspace. Kept here until
    /// it becomes an action grant.
    DesktopControl,
}

impl DeviceCap {
    /// Every device, in the order a prompt lists them.
    pub const ALL: [Self; 14] = [
        Self::Camera,
        Self::Microphone,
        Self::Screen,
        Self::Input,
        Self::SystemAudio,
        Self::Printer,
        Self::HostShell,
        Self::SystemInfo,
        Self::Processes,
        Self::ProcessCommands,
        Self::IoUring,
        Self::Gpu,
        Self::Clipboard,
        Self::DesktopControl,
    ];

    /// The name a capability is stored and shown under.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::Microphone => "microphone",
            Self::Screen => "screen",
            Self::Input => "input",
            Self::SystemAudio => "system-audio",
            Self::Printer => "printer",
            Self::HostShell => "host-shell",
            Self::SystemInfo => "system-info",
            Self::Processes => "processes",
            Self::ProcessCommands => "process-commands",
            Self::IoUring => "io-uring",
            Self::Gpu => "gpu",
            Self::Clipboard => "clipboard",
            Self::DesktopControl => "desktop-control",
        }
    }

    /// The device named `name`, as [`Self::name`] spells it.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|device| device.name() == name)
    }

    /// Whether only the person at the device answers for it: the camera, the
    /// microphone, the screen and the input. A workspace may refuse these to
    /// the apps in it, but never allows them on a member's device: the
    /// person's own answer, kept for that device, does.
    #[must_use]
    pub const fn is_personal(self) -> bool {
        matches!(
            self,
            Self::Camera | Self::Microphone | Self::Screen | Self::Input
        )
    }

    /// The access a grant of this device gives: driving input, printing, the
    /// clipboard, a host shell and desktop control change things, the rest
    /// observe.
    #[must_use]
    pub const fn access(self) -> Access {
        match self {
            Self::Input
            | Self::Printer
            | Self::HostShell
            | Self::Clipboard
            | Self::DesktopControl => Access::Write,
            _ => Access::Read,
        }
    }

    /// The `[capabilities]` entry an app's manifest declares to be given this
    /// device, or `None` for one no manifest asks for: those are reached
    /// through a portal or the desktop, never bound into a sandbox.
    #[must_use]
    pub const fn declaration(self) -> Option<&'static str> {
        match self {
            Self::Camera => Some("camera"),
            Self::Microphone => Some("microphone"),
            Self::Screen => Some("screen-capture"),
            Self::Printer => Some("print"),
            Self::SystemInfo => Some("system-info"),
            Self::Processes | Self::ProcessCommands => Some("processes"),
            Self::IoUring => Some("io-uring"),
            Self::Gpu => Some("gpu"),
            Self::Clipboard => Some("clipboard"),
            _ => None,
        }
    }
}

impl Scope {
    /// Does this scope cover `resource`?
    pub fn matches_resource(&self, resource: &ResourceRef) -> bool {
        match (self, resource) {
            (Scope::FsPath(prefix), ResourceRef::FsPath(path)) => path_covers(prefix, path),
            (Scope::Device(d), ResourceRef::Device(e)) => d == e,
            (Scope::FsResource(d), ResourceRef::FsResource(e)) => d == EVERY || d == e,
            (Scope::NetHost(h), ResourceRef::NetHost(target)) => host_covers(h, target),
            (Scope::FsPath(_) | Scope::Device(_) | Scope::NetHost(_) | Scope::FsResource(_), _) => {
                false
            }
        }
    }
}

/// What a governor exports to its filesystem: the sandbox capabilities people
/// hold over shared files.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FsAuthority {
    pub capabilities: Vec<CapOverride>,
}

impl From<Vec<CapOverride>> for FsAuthority {
    fn from(capabilities: Vec<CapOverride>) -> Self {
        Self { capabilities }
    }
}

/// What a launch hands an app: whether the person launching it may change the
/// workspace through it, and the app's sandbox capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchGrants {
    /// Whether the app may change the workspace: its person may.
    #[serde(default)]
    pub author: bool,
    #[serde(default)]
    pub sandbox: Vec<CapOverride>,
}

impl LaunchGrants {
    /// What reaches the app `app` itself: only the capabilities naming it.
    /// Restrictions travel too: the launcher's decision needs them to narrow
    /// a grant.
    #[must_use]
    pub fn for_app(&self, app: &str) -> Self {
        Self {
            author: self.author,
            sandbox: self
                .sandbox
                .iter()
                .filter(|cap| cap.names_app(app))
                .cloned()
                .collect(),
        }
    }

    /// Nothing to hand over.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.author && self.sandbox.is_empty()
    }
}

/// Does the path-prefix `prefix` cover `path`, respecting component
/// boundaries?
///
/// `/w/docs` covers `/w/docs/a.txt` but not `/w/docsX` — a prefix that
/// stops mid-component is not a parent directory. Same rule
/// `fs::acl::prefix_covers` implements; the two must agree byte-for-byte
/// or a path could be readable through one and not the other.
pub fn path_covers(prefix: &str, path: &str) -> bool {
    if prefix == "/" || prefix.is_empty() {
        return true;
    }
    let prefix = prefix.strip_suffix('/').unwrap_or(prefix);
    if path == prefix {
        return true;
    }
    let mut with_sep = String::with_capacity(prefix.len().saturating_add(1));
    with_sep.push_str(prefix);
    with_sep.push('/');
    path.starts_with(&with_sep)
}

/// A sandboxed resource code might reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceRef {
    FsPath(String),
    Device(DeviceCap),
    NetHost(String),
    FsResource(String),
}

/// Whether a capability grants or restricts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub enum Effect {
    /// Extend the grantee's access within the scope.
    Grant,
    /// Cap the grantee's access within the scope at the capability's [`Access`]:
    /// `Restrict { Read }` means read-only. A restriction never denies
    /// [`Access::Read`].
    Restrict,
    /// Compiled access for a member of a governed workspace file tree.
    WorkspaceFileAccess,
    /// Marks a governed file tree so ordinary path capabilities cannot authorize its contents.
    WorkspaceFileBoundary,
}

/// One sandbox capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct CapOverride {
    pub grantee: Grantee,
    /// Only through this app, when the capability is that app's sandbox.
    #[serde(default)]
    pub app: Option<tilo_app_id::AppId>,
    pub scope: Scope,
    pub effect: Effect,
    pub access: Access,
    /// Stable id: revocation and the replicated map key on it.
    pub id: String,
    /// How long it stands; only a durable one is written into a workspace.
    pub lifetime: Lifetime,
    /// Who granted it, for audit and the share list.
    #[serde(default)]
    pub granted_by: Option<MemberId>,
}

impl CapOverride {
    pub fn new(grantee: Grantee, scope: Scope, effect: Effect, access: Access) -> Self {
        Self {
            grantee,
            app: None,
            scope,
            effect,
            access,
            id: String::new(),
            lifetime: Lifetime::UntilRevoked,
            granted_by: None,
        }
    }

    pub fn grant(grantee: Grantee, scope: Scope, access: Access) -> Self {
        Self::new(grantee, scope, Effect::Grant, access)
    }

    /// The same capability, reaching only through `app`.
    #[must_use]
    pub fn through(mut self, app: impl Into<tilo_app_id::AppId>) -> Self {
        self.app = Some(app.into());
        self
    }

    /// Whether this is `app`'s sandbox capability.
    #[must_use]
    pub fn names_app(&self, app: &str) -> bool {
        self.app.as_ref().is_some_and(|named| named.as_str() == app)
    }

    /// Whether this capability reaches `acting`: its grantee names a node of the acting tree, and
    /// one that names an app reaches it only through that app.
    ///
    /// A role entry reaches nobody here: the workspace's policy is what knows who holds a role, so
    /// the list a mount enforces is written with its members
    /// (`crates/core/tilo-workspace-service/src/files/access.rs`).
    #[must_use]
    pub fn reaches(&self, acting: &Acting) -> bool {
        if let Some(app) = &self.app
            && acting.app.as_ref() != Some(app)
        {
            return false;
        }
        acting
            .nodes()
            .into_iter()
            .any(|node| match node.by.member_id() {
                Some(member) => match &self.grantee {
                    Grantee::Member(named) => named == member,
                    Grantee::Role(_)
                    | Grantee::App(_)
                    | Grantee::CustomRole(_)
                    | Grantee::AnyKey => false,
                },
                None => false,
            })
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    #[must_use]
    pub fn lasting(mut self, lifetime: Lifetime) -> Self {
        self.lifetime = lifetime;
        self
    }

    #[must_use]
    pub fn granted_by(mut self, who: MemberId) -> Self {
        self.granted_by = Some(who);
        self
    }

    /// Whether it is live at `now_ms`, Unix milliseconds; see [`Lifetime::is_live_at`].
    pub fn is_live_at(&self, now_ms: Option<u64>) -> bool {
        self.lifetime.is_live_at(now_ms)
    }

    /// Whether it may be written down; see [`Lifetime::is_durable`].
    pub fn is_durable(&self) -> bool {
        self.lifetime.is_durable()
    }
}

/// The outcome of a sandbox check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapDecision {
    Allow,
    /// No live capability covers the resource.
    NotGranted,
    /// A restriction caps access below what was asked.
    DeniedByRestriction,
}

/// May `acting` reach `resource` with `access`?
///
/// A live matching grant whose access implies the request allows, unless a live
/// matching restriction caps it below the request. Within a governed file tree, the
/// workspace must allow access. `now` in Unix milliseconds; `None` skips expiry.
pub fn decide_capability(
    acting: &Acting,
    resource: &ResourceRef,
    access: Access,
    caps: &[CapOverride],
    now: Option<u64>,
) -> CapDecision {
    decide(|cap| cap.reaches(acting), resource, access, caps, now)
}

/// May the app `app` reach `resource` with `access`, judged by the list written
/// for its sandbox?
///
/// A launcher, a runtime and a portal ask on the app's behalf, over the list
/// that app's install was granted: naming the app is the whole statement of who
/// the capability is for, so the grantee is not asked about.
pub fn decide_app(
    app: &str,
    resource: &ResourceRef,
    access: Access,
    caps: &[CapOverride],
    now: Option<u64>,
) -> CapDecision {
    decide(|cap| cap.names_app(app), resource, access, caps, now)
}

fn decide(
    reaches: impl Fn(&CapOverride) -> bool,
    resource: &ResourceRef,
    access: Access,
    caps: &[CapOverride],
    now: Option<u64>,
) -> CapDecision {
    let mut governed_files = false;
    let mut workspace_granted = false;
    let mut granted = false;
    let mut restricted = false;
    for cap in caps {
        if !cap.scope.matches_resource(resource) {
            continue;
        }
        if cap.effect == Effect::WorkspaceFileBoundary && matches!(resource, ResourceRef::FsPath(_))
        {
            governed_files = true;
        }
        if !cap.is_live_at(now) || !reaches(cap) {
            continue;
        }
        match cap.effect {
            Effect::Grant if cap.access.implies(access) => granted = true,
            Effect::WorkspaceFileAccess if cap.access.implies(access) => workspace_granted = true,
            Effect::Restrict if !cap.access.implies(access) => restricted = true,
            _ => {}
        }
    }
    let granted = if governed_files {
        workspace_granted
    } else {
        granted
    };
    match (granted, restricted) {
        (true, false) => CapDecision::Allow,
        (_, true) if granted => CapDecision::DeniedByRestriction,
        _ => CapDecision::NotGranted,
    }
}

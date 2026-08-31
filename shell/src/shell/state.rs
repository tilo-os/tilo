use automerge::{AutoCommit, ChangeHash};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod shared_surfaces;
pub use shared_surfaces::SharedSurfaces;

pub use collab_types::caps::{
    Actor, CapDecision, CapOverride, Effect, Expiry, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability,
};
pub use collab_types::role::Role as CapRole;

/// Monotonic workspace identifier, newtype-wrapped over `u64`.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct WorkspaceId(pub u64);

impl WorkspaceId {
    /// Allocate a fresh, process-wide unique id.
    pub fn new() -> Self {
        Self(next_id())
    }

    pub fn raw(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod identity_fs_tests {
    use super::*;

    #[test]
    #[serial_test::serial(identity_env)]
    fn filesystem_export_maps_selected_person_through_verified_device_chain() {
        let temp = tempfile::tempdir().expect("temp");
        let xdg = temp.path().join("config");
        let system = temp.path().join("system");
        let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let old_root = std::env::var_os("QOS_IDENTITY_ROOT");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
            std::env::set_var("QOS_IDENTITY_ROOT", &system);
        }
        let identity_dir = xdg.join("qos/identity");
        let paths = qos_user_identity::IdentityPaths::under(&system);
        crate::firstboot::ensure_collaboration_identity(&identity_dir, &paths).expect("identity");
        let profile = crate::identity_registry::IdentityRegistryStore::at(&identity_dir)
            .load()
            .expect("registry")
            .selected_for_workspace("new-room")
            .expect("profile")
            .clone();

        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::test_default(WorkspaceId(1), "Files");
        workspace.capabilities.push(CapOverride::grant(
            Subject::person(profile.principal_id()),
            Scope::FsPath("/var/workspace/files".into()),
            Verb::Write,
        ));
        tree.workspaces.push(workspace);
        let output = temp.path().join("fs-caps.json");
        tree.fs_acl_path = Some(output.clone());
        tree.export_fs_capabilities();

        let exported: Vec<CapOverride> =
            serde_json::from_slice(&std::fs::read(&output).expect("export")).expect("json");
        let device = qos_user_identity::persist::read_device_key(&paths.device_privkey)
            .expect("device")
            .verifying_key()
            .to_bytes();
        assert!(exported.iter().any(|capability| {
            matches!(
                capability.subject,
                Subject::User(key) if key.as_bytes() == &device
            ) && capability.verb == Verb::Write
        }));

        crate::device_fleet::DeviceFleetStore::at(&identity_dir, paths)
            .revoke(
                &profile.id,
                device,
                qos_user_identity::RevocationReason::Retired,
                1,
            )
            .expect("revoke current device");
        tree.export_fs_capabilities();
        let after_revoke: Vec<CapOverride> =
            serde_json::from_slice(&std::fs::read(&output).expect("export")).expect("json");
        assert!(after_revoke.iter().all(|capability| {
            !matches!(
                capability.subject,
                Subject::User(key) if key.as_bytes() == &device
            )
        }));

        unsafe {
            match old_xdg {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match old_root {
                Some(value) => std::env::set_var("QOS_IDENTITY_ROOT", value),
                None => std::env::remove_var("QOS_IDENTITY_ROOT"),
            }
        }
    }
}

/// Durable, globally-unique identity of a workspace as a collaboration
/// room. Unlike [`WorkspaceId`] (a local counter) it is minted once as a
/// random v4 uuid and carried rather than recomputed, so machines that
/// have never met still agree on it. [`Self::nil`] is the unminted
/// sentinel a pre-room state file loads as.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct RoomId(uuid::Uuid);

impl RoomId {
    /// Mint a fresh room identity, once per workspace at creation.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// The "no identity yet" sentinel. Distinguishable from every
    /// minted id because `Uuid::new_v4` never produces the nil uuid.
    pub const fn nil() -> Self {
        Self(uuid::Uuid::nil())
    }

    /// Whether this is the unminted sentinel.
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    /// The underlying uuid, for the wire formats that speak `Uuid`.
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }

    /// Parse a room id off the wire (invite token, automerge doc,
    /// `QOS_WORKSPACE_ID`). `None` on anything that isn't a valid uuid.
    pub fn parse(s: &str) -> Option<Self> {
        uuid::Uuid::parse_str(s).ok().map(Self)
    }
}

impl std::fmt::Display for RoomId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<uuid::Uuid> for RoomId {
    fn from(v: uuid::Uuid) -> Self {
        Self(v)
    }
}

/// Whoever a message arrived from, named as the transport authenticated
/// it — not as the message claims. The only thing about a sender the
/// receiver did not take on faith, so the only thing an authority
/// decision may rest on. Namespaced (`iroh:` / `hub:`) and carried whole
/// rather than hashed: equality must not turn on a digest an attacker
/// can grind against.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PeerOrigin(String);

impl<'de> Deserialize<'de> for PeerOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if let Some(legacy) = value.strip_prefix("p2p:")
            && let Some(endpoint) = crate::network::behaviour::endpoint_from_legacy_peer_id(legacy)
        {
            return Ok(Self(format!("iroh:{endpoint}")));
        }
        Ok(Self(value))
    }
}

fn deserialize_room_peer_names<'de, D>(
    deserializer: D,
) -> Result<HashMap<RoomId, std::collections::BTreeMap<String, String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut rooms =
        HashMap::<RoomId, std::collections::BTreeMap<String, String>>::deserialize(deserializer)?;
    for names in rooms.values_mut() {
        let old = std::mem::take(names);
        *names = old
            .into_iter()
            .map(|(peer, name)| {
                let peer = crate::network::behaviour::endpoint_from_legacy_peer_id(&peer)
                    .map(|endpoint| endpoint.to_string())
                    .unwrap_or(peer);
                (peer, name)
            })
            .collect();
    }
    Ok(rooms)
}

impl PeerOrigin {
    /// A remote peer, by its authenticated iroh endpoint id.
    pub fn peer(peer: &iroh::EndpointId) -> Self {
        Self(format!("iroh:{peer}"))
    }

    /// A remote peer named as an invite token names it — endpoint text
    /// already validated by
    /// [`crate::network::invite::validate_invite_token`].
    pub fn peer_id(endpoint_id: &str) -> Self {
        Self(format!("iroh:{endpoint_id}"))
    }

    /// An in-process `crate::network::hub::Hub` subscriber.
    pub fn hub(id: u64) -> Self {
        Self(format!("hub:{id}"))
    }

    /// This machine, for a batch that did not come off a wire at all
    /// (a local document fork replayed into a room).
    pub fn local() -> Self {
        Self("local".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The handle presence frames identify this origin's peer by, or
    /// `None` when this isn't a remote iroh peer — bridges presence and
    /// room replication onto the same row per peer. One-way only, never
    /// for authority: a collision merges a display row, not a permission.
    pub fn liveness_handle(&self) -> Option<u64> {
        let id = self.0.strip_prefix("iroh:")?;
        let peer = id.parse::<iroh::EndpointId>().ok()?;
        Some(crate::network::behaviour::peer_liveness_handle(&peer))
    }
}

impl std::fmt::Display for PeerOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who may write a room's governance — its owner and capability list —
/// as this machine knows it. Automerge merges a map key
/// last-writer-wins with no per-key ACL, so without this record
/// governance would be decided by which actor id sorts higher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomAuthority {
    /// This machine minted the room, so its cache is the governance.
    Local,
    /// A named peer, cross-checked against the invite token's signature.
    Peer(PeerOrigin),
    /// Joined without an invite naming an administrator: the first peer
    /// to supply governance becomes it, and is pinned.
    Unpinned,
}

impl From<u64> for WorkspaceId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<WorkspaceId> for u64 {
    fn from(v: WorkspaceId) -> Self {
        v.0
    }
}

/// Monotonic strip identifier, newtype-wrapped like [`WorkspaceId`].
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct StripId(pub u64);

impl StripId {
    /// Allocate a fresh, process-wide unique id.
    pub fn new() -> Self {
        Self(next_id())
    }

    pub fn raw(&self) -> u64 {
        self.0
    }
}

impl From<u64> for StripId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<StripId> for u64 {
    fn from(v: StripId) -> Self {
        v.0
    }
}

pub type GroupId = u64;
pub type TabId = u64;
pub type FileId = u64;

/// Snapshot of the focused tab, cloned out of the [`WorkspaceTree`] read
/// guard so callers can drop the guard before using the values.
#[derive(Clone, Debug)]
pub struct TabRef {
    pub id: TabId,
    pub title: String,
}

use std::sync::atomic::{AtomicU16, AtomicU64, Ordering as AtomicOrdering};

/// Process-wide monotonic counter behind [`WorkspaceId::new`],
/// [`StripId::new`] and `next_id()`.
/// [`WorkspaceTree::seed_next_id_from_self`] advances it past the highest
/// persisted id at load, or a fresh process restarts at `1` and collides
/// with an already-loaded entry. Only the low 48 bits come from here; the
/// high 16 carry [`NEXT_ID_PEER_SALT`] so two peers minting in parallel
/// never collide — otherwise `apply_changes` folds two distinct
/// workspaces with the same id into one.
pub(crate) static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// High-16-bit peer salt for every id [`next_id`] mints. Default `0`
/// keeps tests on `0x0001, 0x0002, …`; the hosted entry point overwrites
/// it at startup with a hash of the local Iroh key. `Relaxed`: written
/// once at boot, nothing piggy-backs on visibility.
pub(crate) static NEXT_ID_PEER_SALT: AtomicU16 = AtomicU16::new(0);

/// Mask that selects the low-48-bit counter portion of a salted id.
pub(crate) const PEER_ID_COUNTER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Shift amount that lifts the 16-bit peer salt into the high bits.
pub(crate) const PEER_ID_SALT_SHIFT: u32 = 48;

pub(crate) fn next_id() -> u64 {
    let counter = NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed);
    let counter = counter & PEER_ID_COUNTER_MASK;
    let salt = u64::from(NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed));
    (salt << PEER_ID_SALT_SHIFT) | counter
}

/// Extract the 16-bit peer salt encoded in the high bits of a salted id.
pub(crate) fn peer_salt_of(id: u64) -> u16 {
    (id >> PEER_ID_SALT_SHIFT) as u16
}

/// Extract the low-48-bit counter component of a salted id.
pub(crate) fn counter_of(id: u64) -> u64 {
    id & PEER_ID_COUNTER_MASK
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Other(String),
}

impl AppKind {
    pub fn as_str(&self) -> &str {
        match self {
            AppKind::Terminal => "Terminal",
            AppKind::FileExplorer => "Files",
            AppKind::Browser => "Browser",
            AppKind::Settings => "Settings",
            AppKind::Other(s) => s,
        }
    }

    /// The key into the per-workspace [`Workspace::grants`] map — distinct
    /// from [`Self::as_str`]'s display label. Foreign clients carry their
    /// real `xdg_toplevel::set_app_id` (`org.gimp.GIMP`, …) straight in;
    /// built-ins get namespaced under the compositor's reverse-DNS prefix
    /// so every grant key lives in one namespace.
    pub fn app_id(&self) -> AppId {
        match self {
            AppKind::Terminal => format!("{APP_ID_PREFIX}.Terminal"),
            AppKind::FileExplorer => format!("{APP_ID_PREFIX}.Files"),
            AppKind::Browser => format!("{APP_ID_PREFIX}.Browser"),
            AppKind::Settings => format!("{APP_ID_PREFIX}.Settings"),
            AppKind::Other(s) if s.contains('.') => s.clone(),
            AppKind::Other(s) => format!("{APP_ID_PREFIX}.{s}"),
        }
    }
}

/// Reverse-DNS prefix the compositor stamps on built-in apps' ids,
/// matching its own portal backend name
/// (`org.freedesktop.impl.portal.desktop.nirios`).
pub const APP_ID_PREFIX: &str = "org.nirios";

/// Reference width the legacy pixel model was scaled against, so a
/// migrated `Custom(600)` preserves the *proportion* the user saw rather
/// than snapping to whichever tier is now "Normal".
const LEGACY_PIXEL_REFERENCE_WIDTH: u32 = 1200;

/// Persisted width of a [`WindowGroup`], as a fraction of the strip's
/// usable width — a stored pixel count would mean a different screen
/// share on every monitor. Kept separate from [`ui::widget::WindowWidth`]
/// (serde/CRDT shape vs. layout shape).
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum WindowWidth {
    Narrow,
    Normal,
    Wide,
    /// An arbitrary fraction of the strip's usable width, in per-mille.
    /// What a manual resize persists.
    Fraction(u16),
}

impl WindowWidth {
    /// The fraction this width represents, in per-mille of the strip's
    /// usable width. Delegates to the toolkit so the shell's persisted
    /// tiers and the laid-out ones cannot drift apart.
    pub fn permille(&self) -> u16 {
        self.to_ui().permille()
    }

    /// Resolve to logical pixels against a strip of `strip_usable_px`.
    pub fn resolve(&self, strip_usable_px: f64) -> f64 {
        self.to_ui().resolve(strip_usable_px)
    }

    /// The toolkit width this persisted width maps to.
    pub fn to_ui(self) -> ui::widget::WindowWidth {
        match self {
            WindowWidth::Narrow => ui::widget::WindowWidth::Narrow,
            WindowWidth::Normal => ui::widget::WindowWidth::Normal,
            WindowWidth::Wide => ui::widget::WindowWidth::Wide,
            WindowWidth::Fraction(p) => ui::widget::WindowWidth::Fraction(p),
        }
    }

    /// The persisted width for a toolkit width.
    pub fn from_ui(w: ui::widget::WindowWidth) -> Self {
        match w {
            ui::widget::WindowWidth::Narrow => WindowWidth::Narrow,
            ui::widget::WindowWidth::Normal => WindowWidth::Normal,
            ui::widget::WindowWidth::Wide => WindowWidth::Wide,
            ui::widget::WindowWidth::Fraction(p) => WindowWidth::Fraction(p),
        }
    }

    /// Migrate a width persisted by the pixel model — see
    /// [`LEGACY_PIXEL_REFERENCE_WIDTH`].
    pub fn from_legacy_pixels(px: u32) -> Self {
        WindowWidth::Fraction(ui::widget::permille_for_px(
            f64::from(px),
            f64::from(LEGACY_PIXEL_REFERENCE_WIDTH),
        ))
    }
}

impl<'de> Deserialize<'de> for WindowWidth {
    /// Accepts both the current shape and the legacy pixel shape, so an
    /// existing `~/.local/state` workspace still loads.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        enum Raw {
            Narrow,
            Normal,
            Wide,
            Fraction(u16),
            /// Legacy: an absolute pixel count.
            Custom(u32),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Narrow => WindowWidth::Narrow,
            Raw::Normal => WindowWidth::Normal,
            Raw::Wide => WindowWidth::Wide,
            Raw::Fraction(p) => WindowWidth::Fraction(p.clamp(
                ui::widget::WIDTH_MIN_PERMILLE,
                ui::widget::WIDTH_MAX_PERMILLE,
            )),
            Raw::Custom(px) => WindowWidth::from_legacy_pixels(px),
        })
    }
}

/// What a tile holds. Persistence only carries `Stub` and a symbolic
/// `AppPlaceholder` for native apps, whose concrete widgets are
/// constructed at shell boot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TileContent {
    AppPlaceholder {
        kind: AppKind,
        label: String,
    },
    Stub {
        kind: AppKind,
        label: String,
    },
    /// Foreign Wayland client adopted by the compositor. `surface_id` is
    /// the `wl_surface` protocol id the compositor maps back to a smithay
    /// `WlSurface`; renders as a Stub until the frame loop blits it.
    Foreign {
        app_id: String,
        surface_id: u64,
    },
    /// A window a peer has open, before this machine has bound a local
    /// instance to the tab. The peer's `surface_id` only resolves on
    /// their compositor so it is dropped, keeping only the app id. A
    /// waypoint: `shell::peer_windows` binds a local instance and the
    /// tile becomes `Foreign`. Never projected into a room document,
    /// since a `PeerWindow` is by construction somebody else's.
    PeerWindow {
        /// The wayland app-id the peer's window announces.
        app_id: String,
        /// The peer's own resolved tab title.
        label: String,
        /// What this machine is doing about it, for the card. Empty means
        /// nobody has looked yet; writing the note is how
        /// `shell::peer_windows` records that the decision was made, so a
        /// refusal or launch only ever starts once.
        #[serde(default)]
        note: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub tile: TileContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowGroup {
    pub id: GroupId,
    pub tabs: Vec<Tab>,
    pub active_tab: TabId,
    pub width: WindowWidth,
    /// Mirrors the widget's maximized flag, stored so it survives a
    /// restart and replicates. `#[serde(default)]` so pre-existing JSON
    /// still loads, un-maximized.
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Strip {
    pub id: StripId,
    pub name: String,
    pub window_groups: Vec<WindowGroup>,
}

impl Strip {
    pub fn new(id: StripId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            window_groups: Vec::new(),
        }
    }
}

/// One tab as a room's replication document carries it. Deliberately not
/// a [`Tab`]: no field here for anything a person's own session decided.
#[derive(Clone, Debug)]
struct ReplicatedTab {
    id: TabId,
    order: u64,
    title: String,
    /// `None` when unparseable — hydrated as a stub rather than dropped,
    /// since a window in the room should still show up as one here.
    tile: Option<TileContent>,
}

/// One window group as a room's replication document carries it.
/// `width`/`maximized` are `Option` because a peer may create the entry
/// just to hold its own tab without owning the group — `None` means
/// "owner hasn't said", not "default". **No `active_tab`** — see
/// [`WorkspaceTree::project_owned_contents`].
#[derive(Clone, Debug)]
struct ReplicatedGroup {
    id: GroupId,
    order: u64,
    width: Option<WindowWidth>,
    maximized: Option<bool>,
    tabs: Vec<ReplicatedTab>,
}

/// One strip as a room's replication document carries it.
#[derive(Clone, Debug)]
struct ReplicatedStrip {
    id: u64,
    order: u64,
    name: Option<String>,
    groups: Vec<ReplicatedGroup>,
}

/// Write `value` at `key` only when the document does not already say
/// it, so a document accumulates a change per real change rather than
/// one per export — otherwise each sync round manufactures ops out of
/// untouched state and two peers exporting to each other never fall
/// quiet.
fn put_str_if_changed(doc: &mut AutoCommit, obj: &automerge::ObjId, key: &str, value: &str) {
    use automerge::{ReadDoc, ScalarValue, Value, transaction::Transactable};
    let current = match doc.get(obj, key).ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    };
    if current.as_deref() != Some(value) {
        let _ = doc.put(obj, key, value.to_string());
    }
}

/// [`put_str_if_changed`] for an unsigned integer.
fn put_uint_if_changed(doc: &mut AutoCommit, obj: &automerge::ObjId, key: &str, value: u64) {
    use automerge::{ReadDoc, ScalarValue, Value, transaction::Transactable};
    let current = match doc.get(obj, key).ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Uint(v) => Some(*v),
            _ => None,
        },
        _ => None,
    };
    if current != Some(value) {
        let _ = doc.put(obj, key, ScalarValue::Uint(value));
    }
}

/// [`put_str_if_changed`] for a boolean.
fn put_bool_if_changed(doc: &mut AutoCommit, obj: &automerge::ObjId, key: &str, value: bool) {
    use automerge::{ReadDoc, ScalarValue, Value, transaction::Transactable};
    let current = match doc.get(obj, key).ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Boolean(v) => Some(*v),
            _ => None,
        },
        _ => None,
    };
    if current != Some(value) {
        let _ = doc.put(obj, key, ScalarValue::Boolean(value));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    /// Tab id the collaborator is focused on, driving per-tab presence
    /// dots. `None` means present in the workspace but not on content.
    #[serde(default)]
    pub active_tab: Option<u64>,
    /// Optional `#rrggbb`/`#rrggbbaa` override for the palette
    /// [`ui::presence::user_color`] would otherwise pick.
    #[serde(default)]
    pub cursor_color: Option<String>,
    /// Optional avatar URL or initials fragment; not yet wired to a
    /// bitmap loader (the runtime renders initials today).
    #[serde(default)]
    pub avatar: Option<String>,
    /// Display-only role chip. Authority comes from
    /// [`Workspace::role_of`], never from this field.
    #[serde(default)]
    pub role_id: Option<String>,
    /// Named by the room but has not taken part yet.
    #[serde(default)]
    pub invited: bool,
    /// The publishing peer's Iroh handle, so a departure (which names
    /// only a peer id) finds this row again after it has been renamed
    /// from the handle to the person. `None` for a persisted row.
    #[serde(default)]
    pub session: Option<u64>,
}

impl Collaborator {
    /// Build a collaborator with no tab attribution.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            active_tab: None,
            cursor_color: None,
            avatar: None,
            role_id: None,
            invited: false,
            session: None,
        }
    }

    /// Record the live session this person is here through.
    #[must_use]
    pub fn with_session(mut self, peer: u64) -> Self {
        self.session = Some(peer);
        self
    }

    /// Like [`Collaborator::new`] but attributed to a specific tab.
    #[must_use]
    pub fn with_active_tab(id: impl Into<String>, name: impl Into<String>, tab: u64) -> Self {
        let mut c = Self::new(id, name);
        c.active_tab = Some(tab);
        c
    }

    /// Attach an explicit `#rrggbb` / `#rrggbbaa` override for this peer.
    #[must_use]
    pub fn with_cursor_color(mut self, hex: impl Into<String>) -> Self {
        self.cursor_color = Some(hex.into());
        self
    }

    /// Attach an avatar reference (URL or initials fragment).
    #[must_use]
    pub fn with_avatar(mut self, avatar: impl Into<String>) -> Self {
        self.avatar = Some(avatar.into());
        self
    }

    /// Attach a role id pointing into the workspace's role table.
    #[must_use]
    pub fn with_role(mut self, role_id: impl Into<String>) -> Self {
        self.role_id = Some(role_id.into());
        self
    }

    /// Mark this collaborator as invited rather than arrived.
    #[must_use]
    pub fn as_invited(mut self) -> Self {
        self.invited = true;
        self
    }
}

/// Identifier for an installed app — opaque to the state layer; the
/// portal layer fills it in with the caller's wayland app-id.
pub type AppId = String;

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

/// Per-app capability grant stored on a [`Workspace`]. Each variant maps
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
    /// **Not yet enforced:** `SandboxProfile::apply_grants` has no arm
    /// for it, so `/run/cups/cups.sock` never appears inside the sandbox
    /// however broadly this is granted.
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Durable identity of this workspace as a collaboration room — the
    /// value two hosts must agree on, unlike the local-only [`Self::id`].
    /// `#[serde(default)]` yields [`RoomId::nil`] for pre-room state
    /// files until `adopt_roomless_workspaces` mints a real one at boot.
    #[serde(default)]
    pub room: RoomId,
    pub name: String,
    pub strips: Vec<Strip>,
    /// Who is in this workspace *right now*, asserted present by a
    /// verified invite or presence frame — not durable membership, which
    /// is [`Workspace::owner`] / [`Workspace::capabilities`].
    /// `skip_deserializing` rather than `skip`: reading is the direction
    /// that can fabricate a stale person, but `Serialize` still feeds the
    /// control socket's state dump.
    #[serde(skip_deserializing)]
    pub collaborators: Vec<Collaborator>,
    /// Who owns this room — the one principal whose authority is not
    /// itself a capability. `None` means a pre-ownership state file,
    /// adopted by the local user on first load.
    #[serde(default)]
    pub owner: Option<UserId>,
    /// **Every** permission in this room, as capabilities — one list, one
    /// vocabulary (room membership, file shares, portal grants), so it
    /// can't drift the way a second per-app map would.
    #[serde(default)]
    pub capabilities: Vec<CapOverride>,
    /// Default-application policy belongs to the workspace, while each
    /// person's ordinary defaults remain in their personal settings. All
    /// mutation goes through the authority-checked methods below.
    #[serde(default)]
    pub default_apps: desktop_policy::WorkspaceDefaults,
    /// Surfaces from this workspace being streamed to peers. Keys are
    /// wayland `wl_surface` ids; values are the target Iroh peers
    /// (hashed to `u64`) — a `HashSet` for multi-recipient streaming.
    #[serde(default)]
    pub shared_surfaces: SharedSurfaces,
}

/// Stable identifier for a person. The compositor's collaborator table
/// keys on this; a peer's Ed25519 key hex is a valid value, and so is
/// the local user's account name.
pub type UserId = String;

/// What a role is called on screen. One mapping shared by every surface
/// that shows a role, so it can't be spelled two different ways.
pub fn role_label(role: CapRole) -> &'static str {
    match role {
        CapRole::Owner => "owner",
        CapRole::Editor => "editor",
        CapRole::Commenter => "commenter",
        CapRole::Viewer => "viewer",
        // Not "no role" — a blocked member is still in the room's
        // directory, and a row that said nothing would read as ordinary.
        CapRole::Denied => "blocked",
    }
}

/// The id of the person sitting at this machine. Derived from the device
/// identity when provisioned, falling back to the OS account name —
/// stable across restarts, or an owner would lose their own workspaces.
pub fn local_user_id() -> UserId {
    if let Ok(v) = std::env::var("QOS_USER_ID")
        && !v.is_empty()
    {
        return v;
    }
    if let Ok(bytes) = std::fs::read("/var/lib/qos/user-identity/pubkey.bin")
        && bytes.len() == 32
    {
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        if let Principal::Person(id) =
            Principal::person_from_key(collab_types::crypto::Ed25519PublicKey::from_bytes(key))
        {
            return id;
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "local".to_string())
}

/// Identity acting in a specific durable room.
///
/// Registry errors fail back to the session identity for compatibility, but
/// a valid explicit binding always wins. Callers that launch an app use the
/// stricter path resolver and fail closed if its private record is unavailable.
pub fn local_user_id_for_room(room: &RoomId) -> UserId {
    crate::identity_registry::selected_principal_id(&room.to_string())
        .ok()
        .flatten()
        .unwrap_or_else(local_user_id)
}

/// The name a session goes by before it says who is sitting at it.
/// Derived from the Iroh peer handle, so it must match the fallback
/// label [`crate::shell::peer_presence::fold_inbound`] gives an
/// unnamed publisher, or one peer shows up as two.
pub fn session_handle(peer: u64) -> String {
    format!("p{peer:016x}")
}

/// Seconds since the Unix epoch, for capability expiry. An unset clock
/// reads as 0, making every `AtUnix` capability look live — safe for a
/// *grant*, unsafe for a *restriction*, so write restrictions without
/// an expiry.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkspaceDefaultError {
    #[error("only a workspace administrator may change workspace policy")]
    NotAdministrator,
    #[error("a person may only change their own workspace default")]
    WrongIdentity,
    #[error("invalid default application: {0}")]
    InvalidSelection(desktop_policy::DefaultAppError),
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

impl Workspace {
    /// Set the administrator layer of default-app policy. The shell supplies
    /// the authenticated local identity; a caller cannot widen policy by
    /// merely painting an enabled control.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceDefaultError::NotAdministrator`] if `actor` does
    /// not hold administrator authority in this workspace, or
    /// [`WorkspaceDefaultError::InvalidSelection`] if `policy` names an app
    /// package the resolver rejects for `resource`.
    pub fn set_default_app_policy_as(
        &mut self,
        actor: &UserId,
        resource: desktop_policy::ResourceKind,
        policy: desktop_policy::WorkspaceDefaultPolicy,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
    ) -> Result<(), WorkspaceDefaultError> {
        if !self.may_administer(actor) {
            return Err(WorkspaceDefaultError::NotAdministrator);
        }
        if let desktop_policy::WorkspaceDefaultPolicy::App(package) = &policy {
            resolver
                .validate_app(&resource, package)
                .map_err(WorkspaceDefaultError::InvalidSelection)?;
        }
        self.default_apps.policy.insert(resource, policy);
        Ok(())
    }

    /// Set a person's own workspace-level choice. Even a workspace owner may
    /// not silently rewrite another person's preference; administrative
    /// policy is the separate method above and remains visibly attributable.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceDefaultError::WrongIdentity`] if `actor` differs
    /// from `subject` — a person may only set their own choice — or
    /// [`WorkspaceDefaultError::InvalidSelection`] if `choice` names an app
    /// package the resolver rejects for `resource`.
    pub fn set_default_app_choice_as(
        &mut self,
        actor: &UserId,
        subject: &UserId,
        resource: desktop_policy::ResourceKind,
        choice: desktop_policy::UserWorkspaceChoice,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
    ) -> Result<(), WorkspaceDefaultError> {
        if actor != subject {
            return Err(WorkspaceDefaultError::WrongIdentity);
        }
        if let desktop_policy::UserWorkspaceChoice::App(package) = &choice {
            resolver
                .validate_app(&resource, package)
                .map_err(WorkspaceDefaultError::InvalidSelection)?;
        }
        self.default_apps
            .user_choices
            .entry(subject.clone())
            .or_default()
            .insert(resource, choice);
        Ok(())
    }

    // App grants — the portal's view of the one capability list. Not a
    // second store: they read/write `self.capabilities`, filtered to
    // subjects that are apps.

    /// Record an app's grant. Returns the ids of the capabilities
    /// written (one per requested scope).
    pub fn grant_app(&mut self, app_id: &str, cap: &Capability) -> Vec<String> {
        let expiry = cap.requested_expiry();
        cap.requested_scopes()
            .into_iter()
            .map(|(scope, verb)| {
                self.grant_capability(
                    CapOverride::grant(Subject::app(app_id), scope, verb).expiring(expiry),
                )
            })
            .collect()
    }

    pub fn app_capabilities(&self, app_id: &str) -> Vec<&CapOverride> {
        let want = Subject::app(app_id);
        let now = Some(now_unix());
        self.capabilities
            .iter()
            .filter(|c| c.subject == want && c.effect == Effect::Grant && c.is_live_at(now))
            .collect()
    }

    /// A **snapshot** of app grants in the portal's `Capability` shape.
    /// Rebuilt from [`Self::capabilities`] on every call — mutating the
    /// returned map edits a temporary and stores nothing. To grant, use
    /// [`Self::grant_app`] or [`Self::grant_as`].
    pub fn app_grants_snapshot(&self) -> HashMap<AppId, Vec<Capability>> {
        let now = Some(now_unix());
        let mut out: HashMap<AppId, Vec<Capability>> = HashMap::new();
        for c in &self.capabilities {
            if c.effect != Effect::Grant || !c.is_live_at(now) {
                continue;
            }
            let Subject::Principal(Principal::App(app)) = &c.subject else {
                continue;
            };
            if let Some(desc) = Capability::from_capability(c) {
                out.entry(app.clone()).or_default().push(desc);
            }
        }
        out
    }

    /// Does any app hold a grant in this room? Drives the Permissions
    /// tab's visibility.
    pub fn has_app_grants(&self) -> bool {
        let now = Some(now_unix());
        self.capabilities.iter().any(|c| {
            c.effect == Effect::Grant
                && c.is_live_at(now)
                && matches!(&c.subject, Subject::Principal(Principal::App(_)))
                && Capability::from_capability(c).is_some()
        })
    }

    /// Ids of the capabilities backing app `app_id`'s grants, in the
    /// same order [`Self::grants`] renders them, so a Revoke button
    /// that knows a row index can name the capability it removes.
    pub fn app_grant_ids(&self, app_id: &str) -> Vec<String> {
        let now = Some(now_unix());
        let want = Subject::app(app_id);
        self.capabilities
            .iter()
            .filter(|c| {
                c.subject == want
                    && c.effect == Effect::Grant
                    && c.is_live_at(now)
                    && Capability::from_capability(c).is_some()
            })
            .map(|c| c.id.clone())
            .collect()
    }

    // The capability store — every permission question bottoms out in
    // `decide` / `allows` below.

    /// Decide whether `actor` may apply `verb` to `resource` in this
    /// room. Fail-closed: denies unless something explicitly permits.
    pub fn decide(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> CapDecision {
        decide_capability(actor, resource, verb, &self.capabilities, Some(now_unix()))
    }

    /// [`Self::decide`] reduced to a bool, for call sites that only
    /// gate an action rather than explaining a denial.
    pub fn allows(&self, actor: &Actor, resource: &ResourceRef, verb: Verb) -> bool {
        self.decide(actor, resource, verb) == CapDecision::Allow
    }

    /// Record a capability, minting an id if the caller didn't supply
    /// one. Authority to *make* the grant is checked by the caller
    /// ([`Self::grant_as`]), not here.
    pub fn grant_capability(&mut self, mut cap: CapOverride) -> String {
        if cap.id.is_empty() {
            cap.id = format!("cap-{}", next_id());
        }
        // Merges rather than stacks a re-grant of the same (subject,
        // scope, effect, verb), keeping the longer-lived expiry — a
        // grant is monotonic; reducing authority is revoke's job.
        if let Some(slot) = self.capabilities.iter_mut().find(|c| {
            c.subject == cap.subject
                && c.scope == cap.scope
                && c.effect == cap.effect
                && c.verb == cap.verb
        }) {
            slot.expiry = Expiry::longest(slot.expiry, cap.expiry);
            if !cap.id.is_empty() {
                slot.id = cap.id;
            }
            if cap.granted_by.is_some() {
                slot.granted_by = cap.granted_by;
            }
            return slot.id.clone();
        }
        if cap.id.is_empty() {
            cap.id = format!("cap-{}", next_id());
        }
        let id = cap.id.clone();
        self.capabilities.push(cap);
        id
    }

    /// Grant a capability **on behalf of** `grantor`, enforcing that
    /// the grantor actually holds authority to delegate it — a peer
    /// without `Admin` cannot widen anyone's access, including their
    /// own. `None` when the grantor lacks that authority.
    pub fn grant_as(&mut self, grantor: &UserId, cap: CapOverride) -> Option<String> {
        if !self.may_administer(grantor) {
            return None;
        }
        let cap = cap.granted_by(Principal::Person(grantor.clone()));
        Some(self.grant_capability(cap))
    }

    /// Drop the capability with this id. Returns whether anything was
    /// removed.
    pub fn revoke_capability(&mut self, id: &str) -> bool {
        let before = self.capabilities.len();
        self.capabilities.retain(|c| c.id != id);
        self.capabilities.len() != before
    }

    /// Revoke on behalf of `revoker`, enforcing the same authority as
    /// [`Self::grant_as`].
    pub fn revoke_as(&mut self, revoker: &UserId, id: &str) -> bool {
        self.may_administer(revoker) && self.revoke_capability(id)
    }

    /// Drop every capability matching `pred`. Returns the ids removed,
    /// so the caller can mirror each removal into the replicated doc.
    pub fn revoke_matching(&mut self, pred: impl Fn(&CapOverride) -> bool) -> Vec<String> {
        let mut removed = Vec::new();
        self.capabilities.retain(|c| {
            if pred(c) {
                removed.push(c.id.clone());
                false
            } else {
                true
            }
        });
        removed
    }

    // Ownership and membership.

    /// Is `user` the owner, or a delegated co-owner (`Admin` over the
    /// room)? An ownerless workspace has no administrators, not
    /// everyone-is-an-administrator.
    pub fn may_administer(&self, user: &UserId) -> bool {
        if self.owner.as_deref() == Some(user.as_str()) {
            return true;
        }
        self.allows(
            &self.actor_for_person(user),
            &ResourceRef::Workspace,
            Verb::Admin,
        )
    }

    /// Install `user` as owner, and record the membership capability
    /// that makes them `Admin` — `owner` survives an empty capability
    /// list, and the capability is what replicates and what `decide` reads.
    pub fn set_owner(&mut self, user: impl Into<UserId>) {
        let user = user.into();
        self.owner = Some(user.clone());
        self.grant_capability(
            CapOverride::membership(
                Subject::Principal(Principal::Person(user)),
                self.room.to_string(),
                Verb::Admin,
            )
            .with_id(format!("owner-{}", self.id.raw())),
        );
    }

    /// Give up administration because this workspace joined a room it
    /// did not mint — drops both the recorded owner and every admin
    /// capability, or [`Self::may_administer`] would still answer yes
    /// for a room this machine doesn't own.
    pub fn surrender_governance(&mut self) -> bool {
        let had_owner = self.owner.take().is_some();
        let dropped = self.revoke_matching(|c| {
            let is_person = matches!(c.subject, Subject::Principal(Principal::Person(_)));
            // Capability vectors are already owned by one workspace. Accept
            // legacy local-id and current durable-room spellings alike so a
            // replicated membership cannot disappear on a peer whose local
            // workspace handle differs.
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            is_person && covers_room && c.verb.implies(Verb::Admin)
        });
        had_owner || !dropped.is_empty()
    }

    /// The role `user` holds. Not stored separately — it *is* the verb
    /// on their membership capability, so "in the room" and "allowed
    /// to do something" can't drift apart. `None` means not a member.
    pub fn role_of(&self, user: &UserId) -> Option<CapRole> {
        if self.owner.as_deref() == Some(user.as_str()) {
            return Some(CapRole::Owner);
        }
        let subject_is_user = |c: &CapOverride| match &c.subject {
            Subject::Principal(Principal::Person(p)) => p == user,
            _ => false,
        };
        let now = Some(now_unix());
        let mut best: Option<Verb> = None;
        let mut blocked = false;
        for c in &self.capabilities {
            if !subject_is_user(c) || !c.is_live_at(now) {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            match c.effect {
                Effect::Grant => {
                    best = Some(match best {
                        Some(prev) if prev.implies(c.verb) => prev,
                        _ => c.verb,
                    });
                }
                // A room-wide Restrict below Read is how a member is
                // blocked without being erased from history.
                Effect::Restrict if !c.verb.implies(Verb::Read) => blocked = true,
                Effect::Restrict => {}
            }
        }
        if blocked {
            return Some(CapRole::Denied);
        }
        best.map(|v| match v {
            Verb::Admin => CapRole::Owner,
            Verb::Write => CapRole::Editor,
            Verb::Comment => CapRole::Commenter,
            Verb::Read | Verb::Invite => CapRole::Viewer,
        })
    }

    /// Build the [`Actor`] for a person acting in this room.
    pub fn actor_for_person(&self, user: &UserId) -> Actor {
        Actor::person(self.role_of(user), user.clone()).in_room(self.room.to_string())
    }

    /// Build the [`Actor`] for an app running in this room. Carries the
    /// room as principal but **no role** — an app has no base authority,
    /// only what's granted to it by name.
    pub fn actor_for_app(&self, app_id: &str) -> Actor {
        Actor::app(app_id).in_room(self.room.to_string())
    }

    /// Build the [`Actor`] for an agent acting for the user. Same
    /// authority shape as an app: scoped, revocable, no role.
    pub fn actor_for_agent(&self, agent_id: &str) -> Actor {
        Actor::agent(agent_id).in_room(self.room.to_string())
    }

    /// The room roster: the people who are *members*. Built from
    /// room-scoped capabilities only — someone with a capability on one
    /// file and nothing else is **not** here. See [`Self::shares_of`].
    pub fn roster(&self) -> Vec<(UserId, CapRole)> {
        let now = Some(now_unix());
        let mut seen: Vec<UserId> = Vec::new();
        for c in &self.capabilities {
            if !c.is_live_at(now) || c.effect != Effect::Grant {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            if let Subject::Principal(Principal::Person(p)) = &c.subject
                && !seen.contains(p)
            {
                seen.push(p.clone());
            }
        }
        if let Some(owner) = &self.owner
            && !seen.contains(owner)
        {
            seen.insert(0, owner.clone());
        }
        seen.into_iter()
            .filter_map(|u| self.role_of(&u).map(|r| (u, r)))
            .collect()
    }

    /// The role an *agent* holds — same derivation as [`Self::role_of`]
    /// minus the owner short-circuit, since an agent is never the owner.
    pub fn role_of_agent(&self, agent: &str) -> Option<CapRole> {
        let now = Some(now_unix());
        let mut best: Option<Verb> = None;
        let mut blocked = false;
        for c in &self.capabilities {
            let subject_matches = matches!(
                &c.subject,
                Subject::Principal(Principal::Agent(a)) if a == agent
            );
            if !subject_matches || !c.is_live_at(now) {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            match c.effect {
                Effect::Grant => {
                    best = Some(match best {
                        Some(prev) if prev.implies(c.verb) => prev,
                        _ => c.verb,
                    });
                }
                Effect::Restrict if !c.verb.implies(Verb::Read) => blocked = true,
                Effect::Restrict => {}
            }
        }
        if blocked {
            return Some(CapRole::Denied);
        }
        best.map(|v| match v {
            Verb::Admin => CapRole::Owner,
            Verb::Write => CapRole::Editor,
            Verb::Comment => CapRole::Commenter,
            Verb::Read | Verb::Invite => CapRole::Viewer,
        })
    }

    /// The agents that hold membership in this room — a *second* list
    /// from [`Self::roster`], since an agent is not a person and
    /// rendering them undifferentiated would misrepresent who's present.
    pub fn agent_members(&self) -> Vec<(String, CapRole)> {
        let now = Some(now_unix());
        let mut seen: Vec<String> = Vec::new();
        for c in &self.capabilities {
            if !c.is_live_at(now) || c.effect != Effect::Grant {
                continue;
            }
            let covers_room = matches!(c.scope, Scope::Room(_) | Scope::Workspace);
            if !covers_room {
                continue;
            }
            if let Subject::Principal(Principal::Agent(a)) = &c.subject
                && !seen.contains(a)
            {
                seen.push(a.clone());
            }
        }
        seen.into_iter()
            .filter_map(|a| self.role_of_agent(&a).map(|r| (a, r)))
            .collect()
    }

    /// The share list for one resource. Room-scoped capabilities are
    /// excluded (those belong in [`Self::roster`]) so the UI can never
    /// present "shared one file with Bob" as "Bob is a collaborator".
    pub fn shares_of(&self, resource: &ResourceRef) -> Vec<&CapOverride> {
        let now = Some(now_unix());
        self.capabilities
            .iter()
            .filter(|c| {
                c.is_live_at(now)
                    && c.effect == Effect::Grant
                    && c.subject_is_person()
                    && !c.is_room_membership()
                    && c.scope.matches_resource(resource)
            })
            .collect()
    }

    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self {
            id,
            // A workspace that exists is a room that exists: minted
            // here rather than lazily.
            room: RoomId::new(),
            name: name.into(),
            strips: Vec::new(),
            collaborators: Vec::new(),
            owner: None,
            capabilities: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            shared_surfaces: SharedSurfaces::default(),
        }
    }

    /// What fixtures want instead of a hand-written `Workspace { … }`
    /// literal (which breaks on every new field); each fixture also gets
    /// its own room rather than sharing the nil one.
    pub fn test_default(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self::new(id, name)
    }

    /// This workspace's collaboration cluster id — [`Self::room`], not
    /// the local [`Self::id`]. Unminted returns the nil uuid, which
    /// `CrdtRoute::delivers_to` refuses to deliver to.
    pub fn collab_cluster_id(&self) -> uuid::Uuid {
        self.room.as_uuid()
    }

    /// Mark `surface_id` as being streamed to `peer_id`. Additive — two
    /// calls record both peers in the per-surface set.
    pub fn share_surface(&mut self, surface_id: u64, peer_id: u64) {
        self.shared_surfaces.share(surface_id, peer_id);
    }

    /// Drop the share entry for `surface_id` (no-op if unshared).
    /// Returns the peers it was shared with, so callers can fire
    /// `StopShare` for each without a second lookup.
    pub fn unshare_surface(&mut self, surface_id: u64) -> Option<std::collections::HashSet<u64>> {
        self.shared_surfaces.unshare(surface_id)
    }

    /// Stop streaming `surface_id` to one peer, leaving others intact.
    /// Drops the surface key entirely once the last peer is removed, so
    /// [`Self::is_surface_shared`] reports `false` again.
    pub fn unshare_surface_from_peer(&mut self, surface_id: u64, peer_id: u64) -> bool {
        self.shared_surfaces.unshare_peer(surface_id, peer_id)
    }

    /// Whether `surface_id` is currently streamed somewhere. Used by
    /// the compositor's commit hook to short-circuit the readback for
    /// unshared surfaces (~all of them in steady state).
    pub fn is_surface_shared(&self, surface_id: u64) -> bool {
        self.shared_surfaces.contains(surface_id)
    }
}

impl Default for Workspace {
    /// Room is minted, not left at [`RoomId::nil`] — two workspaces
    /// built this way must not silently become the same room. Nil is
    /// reachable only via deserializing a pre-rooms state file.
    fn default() -> Self {
        Self {
            id: WorkspaceId::default(),
            room: RoomId::new(),
            name: String::new(),
            strips: Vec::new(),
            collaborators: Vec::new(),
            owner: None,
            capabilities: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            shared_surfaces: SharedSurfaces::default(),
        }
    }
}

/// The shape of `state.json` this build writes and understands. Bump
/// only when a change makes a file unreadable to an older build in a way
/// that loses meaning (field removed/repurposed, id space renumbered) —
/// a new `#[serde(default)]` field does not qualify. Guards the one
/// direction with no safe default: older reading newer.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// A file with no `schema_version` key is version 1 — the key was added
/// alongside the check, not a format change, and defaulting to "unknown"
/// would strand every existing session on upgrade.
fn state_schema_version_of_a_file_without_one() -> u32 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceTree {
    /// Which version of this struct's on-disk shape the file describes.
    /// Read back to decide whether this build may touch the file at all.
    #[serde(default = "state_schema_version_of_a_file_without_one")]
    pub schema_version: u32,
    /// Counts rounds in which a peer's changes landed. `#[serde(skip)]`
    /// — a restored value could match a live reader's cache and
    /// suppress the first resync.
    #[serde(skip)]
    pub room_epoch: u64,
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub active: WorkspaceId,
    pub active_strip: HashMap<WorkspaceId, StripId>,
    /// Per-strip "active group" pointer, one level deeper than
    /// [`Self::active_strip`]. Missing means "fall back to the first
    /// non-closing group" (see [`Self::resolve_active_group`]).
    #[serde(default)]
    pub active_group: HashMap<StripId, GroupId>,
    /// Tree-level roster, fallback for chrome when the active workspace
    /// has nobody of its own. `#[serde(skip)]` — reloading it would show
    /// people who are not there.
    #[serde(skip_deserializing)]
    pub collaborators: Vec<Collaborator>,
    /// Automerge backing doc for peer-sync. The cache (other fields) is
    /// the source of truth for reads; this doc for peer change export.
    /// `#[serde(skip)]` — repopulates as peer-sync runs. Public so tests
    /// can synthesize change-blobs, but real consumers must go through
    /// [`Self::current_heads`] / [`Self::export_changes_since`] /
    /// [`Self::apply_changes`]: direct access bypasses the cache rebuild.
    #[serde(skip, default = "AutoCommit::new")]
    pub doc: AutoCommit,
    /// One replication document per room — [`Self::doc`] describes
    /// **every** workspace, so handing it to a peer would leak every
    /// other room's state. Maintained by [`Self::sync_room_doc`];
    /// re-projected from the cache after a restart.
    #[serde(skip, default)]
    pub room_docs: HashMap<RoomId, AutoCommit>,
    /// Who may write each room's governance — see [`RoomAuthority`]. A
    /// room absent from this map is one this machine minted and governs.
    /// Persisted, unlike [`Self::room_docs`] — can't be re-derived.
    #[serde(default)]
    pub room_authority: HashMap<RoomId, RoomAuthority>,
    /// What each peer is **called**, as Iroh `EndpointId` →
    /// display name. Written only by the inviter (non-spoofable);
    /// [`Self::apply_room_changes`] refuses it from anyone else.
    #[serde(default, deserialize_with = "deserialize_room_peer_names")]
    pub room_peer_names: HashMap<RoomId, std::collections::BTreeMap<String, String>>,
    /// Mirror of each room's record index (record id → kind), pushed by
    /// [`Self::set_room_records`] — a plain-data copy since the real
    /// index lives behind an `Rc<RefCell<_>>` peer-sync's
    /// `Arc<RwLock<_>>` can't reach. Not persisted.
    #[serde(skip, default)]
    pub room_records_held: HashMap<RoomId, std::collections::BTreeMap<String, String>>,
    /// References that arrived from a peer and have not been folded into
    /// the local index yet. Drained by the shell — see
    /// [`Self::take_inbound_room_records`].
    #[serde(skip, default)]
    pub room_records_inbound: Vec<(RoomId, String, String)>,
    /// For a joined room, the [`NEXT_ID`] value at the moment of
    /// joining — windows minted before that aren't projected in, or
    /// accepting an invite would publish every window already open. A
    /// counter, not a timestamp, so the line survives a restart.
    #[serde(default)]
    pub room_share_floor: HashMap<RoomId, u64>,
    /// This tree's own peer salt, overriding the process-wide one — the
    /// static is wrong when one process holds **two** trees (e.g. two
    /// simulated peers). [`Self::local_salt`] prefers this field;
    /// production never sets it.
    #[serde(skip, default)]
    pub peer_salt_override: Option<u16>,
    /// Where to write this tree when a capability changes. `None`
    /// disables it (tests, offscreen renderer). Set at boot by the
    /// compositor — what makes an `Always` grant survive a restart.
    #[serde(skip)]
    pub persist_path: Option<std::path::PathBuf>,
    /// Where to export the capability list the FUSE mount enforces.
    /// `None` disables the export; set at boot to the same path passed
    /// to `qos-fs --acl`, so both sides decide from one list.
    #[serde(skip)]
    pub fs_acl_path: Option<std::path::PathBuf>,
}

impl Default for WorkspaceTree {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            room_epoch: 0,
            workspaces: Vec::new(),
            active: WorkspaceId::default(),
            active_strip: HashMap::new(),
            active_group: HashMap::new(),
            collaborators: Vec::new(),
            doc: AutoCommit::new(),
            room_docs: HashMap::new(),
            room_authority: HashMap::new(),
            room_peer_names: HashMap::new(),
            room_records_held: HashMap::new(),
            room_records_inbound: Vec::new(),
            room_share_floor: HashMap::new(),
            peer_salt_override: None,
            persist_path: None,
            fs_acl_path: None,
        }
    }
}

#[cfg(test)]
mod iroh_identity_migration_tests {
    use super::*;

    fn legacy_peer(seed: [u8; 32]) -> (String, iroh::EndpointId) {
        let endpoint = iroh::SecretKey::from_bytes(&seed).public();
        let mut bytes = vec![0x00, 0x24, 0x08, 0x01, 0x12, 0x20];
        bytes.extend_from_slice(endpoint.as_bytes());
        (bs58::encode(bytes).into_string(), endpoint)
    }

    #[test]
    fn persisted_libp2p_authority_and_names_migrate_to_iroh() {
        let room = RoomId::from(uuid::Uuid::new_v4());
        let (legacy, endpoint) = legacy_peer([31; 32]);
        let mut tree = WorkspaceTree::default();
        tree.room_authority.insert(
            room,
            RoomAuthority::Peer(PeerOrigin(format!("p2p:{legacy}"))),
        );
        tree.room_peer_names
            .entry(room)
            .or_default()
            .insert(legacy, "Alice".into());

        let json = serde_json::to_vec(&tree).unwrap();
        let migrated: WorkspaceTree = serde_json::from_slice(&json).unwrap();
        assert_eq!(
            migrated.room_authority(room),
            RoomAuthority::Peer(PeerOrigin::peer(&endpoint))
        );
        assert_eq!(
            migrated.room_peer_names[&room].get(&endpoint.to_string()),
            Some(&"Alice".to_string())
        );
    }
}

/// Shared handle to a single [`WorkspaceTree`], cloned by the
/// compositor, portal and peer-sync so every mutation lands on the same
/// tree. `parking_lot::RwLock` rather than a `Mutex`: readers vastly
/// outnumber writers at ~60 Hz repaint, and an uncontended read lock is
/// ~10 ns.
pub type WorkspaceTreeHandle = std::sync::Arc<parking_lot::RwLock<WorkspaceTree>>;

/// Constructors for [`WorkspaceTreeHandle`] — Rust won't let us add
/// inherent methods to a type alias, so they live on this trait instead.
pub trait WorkspaceTreeHandleExt: Sized {
    /// A handle wrapped around a default (empty) tree.
    fn test_default() -> Self;
}

impl WorkspaceTreeHandleExt for WorkspaceTreeHandle {
    fn test_default() -> Self {
        std::sync::Arc::new(parking_lot::RwLock::new(WorkspaceTree::default()))
    }
}

impl WorkspaceTree {
    /// Wrap this tree in a [`WorkspaceTreeHandle`]. Consumes the tree
    /// (there should only ever be one instance) and returns the shared
    /// handle every subsystem clones from.
    pub fn shared(self) -> WorkspaceTreeHandle {
        std::sync::Arc::new(parking_lot::RwLock::new(self))
    }
}

/// Owner, capability blobs, and peer-name claims read from a room
/// document's governance keys. See [`WorkspaceTree::governance_of`].
type RoomGovernance = (
    Option<String>,
    std::collections::BTreeMap<String, Vec<u8>>,
    std::collections::BTreeMap<String, String>,
);

impl WorkspaceTree {
    /// Set the process-wide peer salt [`next_id`] stamps into the high
    /// 16 bits of every freshly-allocated id. Default `0` keeps
    /// `WorkspaceId(1)`-style test literals working; two peers under
    /// distinct salts can never collide. Takes `&mut self` even though
    /// the salt is process-global, so a per-tree-salt refactor stays a
    /// one-line change.
    pub fn set_peer_salt(&mut self, salt: u16) {
        Self::set_process_peer_salt(salt);
    }

    /// [`Self::set_peer_salt`] without a tree to call it on. Must run
    /// **before** the tree is loaded: ids minted before the salt exists
    /// land in the shared 0-salt range, identical on every first boot,
    /// and the room document keys a window group by id — so two such
    /// peers fold two distinct windows into one. Re-seeding the counter
    /// afterward does not repair already-minted ids.
    pub fn set_process_peer_salt(salt: u16) {
        NEXT_ID_PEER_SALT.store(salt, AtomicOrdering::Relaxed);
    }

    /// Read the current process-wide peer salt. Mostly used by tests to
    /// assert on the salt without poking the static directly.
    pub fn peer_salt(&self) -> u16 {
        NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed)
    }

    /// The salt replication treats as this tree's own — the id range
    /// this tree may write and sweep. [`Self::peer_salt_override`]
    /// first, the process-wide salt otherwise.
    pub fn local_salt(&self) -> u16 {
        self.peer_salt_override
            .unwrap_or_else(|| NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed))
    }

    /// Give this tree a salt without touching the process-wide one, for
    /// holding two trees at once. The caller must mint matching ids
    /// itself; [`next_id`] reads the static.
    pub fn set_salt_override(&mut self, salt: u16) {
        self.peer_salt_override = Some(salt);
    }

    /// Walk every id and advance [`NEXT_ID`] past the highest counter
    /// stamped by the local peer salt — idempotent, called after
    /// deserializing. The counter restarts at `1` each process, so
    /// without this it collides with persisted ids. Only local-salt ids
    /// count; a remote peer's must not advance our counter.
    pub fn seed_next_id_from_self(&self) {
        let local_salt = NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed);
        let mut max_counter: u64 = 0;
        let mut consider = |id: u64| {
            if peer_salt_of(id) == local_salt {
                max_counter = max_counter.max(counter_of(id));
            }
        };
        for ws in &self.workspaces {
            consider(ws.id.raw());
            for strip in &ws.strips {
                consider(strip.id.raw());
                for group in &strip.window_groups {
                    consider(group.id);
                    for tab in &group.tabs {
                        consider(tab.id);
                    }
                }
            }
        }
        // Advance the counter past `max_counter`. `fetch_max` keeps the
        // higher of the two so a parallel allocator that already
        // pushed past max_counter isn't reset backwards.
        NEXT_ID.fetch_max(max_counter.saturating_add(1), AtomicOrdering::Relaxed);
    }

    /// Close the room document's open transaction, stamping when it
    /// happened — otherwise `AutoCommit`'s implicit close leaves the
    /// advisory timestamp at zero and no activity view can date-filter
    /// the change graph. Also keeps each mirrored mutation its own
    /// change rather than letting a run coalesce. Cheap when nothing is
    /// pending, so the compositor also calls it once a frame.
    pub fn commit_room_change(&mut self) {
        let _ = qdoc::history::commit_stamped(&mut self.doc);
    }

    /// Snapshot of the current automerge change-graph heads, the
    /// baseline peer-sync computes what to send against. Takes `&mut
    /// self` because `AutoCommit` flushes any pending transaction
    /// before returning heads.
    pub fn current_heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// Export every change applied since `since` as opaque change-blobs.
    /// **Not a peer-facing export**: [`Self::doc`] holds every workspace
    /// on this machine, so serving it would leak every other room. Peers
    /// get [`Self::export_room_changes_since`], scoped to one room.
    pub fn export_changes_since(&mut self, since: &[ChangeHash]) -> Vec<Vec<u8>> {
        self.doc
            .get_changes(since)
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect()
    }

    /// Bring the cache back in line with the room document after a
    /// *local* rewrite (a history restore doesn't go through
    /// [`Self::apply_changes`], which does this for peer changes).
    pub fn resync_cache_from_doc(&mut self) {
        self.rebuild_cache_from_doc();
    }

    /// Apply a batch of opaque change-blobs received from a peer. After
    /// applying, the cache fields are rebuilt via
    /// [`Self::rebuild_cache_from_doc`] so subsequent reads observe the
    /// merged state.
    ///
    /// # Errors
    ///
    /// Returns the underlying `automerge::AutomergeError` if any blob
    /// fails to decode as a `Change`, or if applying the (otherwise
    /// well-formed) batch to `self.doc` fails.
    pub fn apply_changes(&mut self, changes: &[Vec<u8>]) -> Result<(), automerge::AutomergeError> {
        let parsed: Result<Vec<automerge::Change>, _> = changes
            .iter()
            .map(|b| automerge::Change::try_from(b.as_slice()))
            .collect();
        self.doc.apply_changes(parsed?)?;
        self.rebuild_cache_from_doc();
        Ok(())
    }

    // Per-room replication: an invite must not hand over a directory of
    // every other room, and `doc`'s change graph cannot be served in
    // part (changes form a hash chain, so filtering leaves dependencies
    // the receiver never sees and automerge parks them unapplied
    // forever). So the replication unit is a document per room,
    // projected from the cache.

    /// Every room this machine is in, deduplicated, in workspace order.
    /// A workspace whose room is still the unminted sentinel is skipped.
    pub fn rooms(&self) -> Vec<RoomId> {
        let mut seen = std::collections::HashSet::new();
        self.workspaces
            .iter()
            .map(|w| w.room)
            .filter(|r| !r.is_nil() && seen.insert(*r))
            .collect()
    }

    /// Who may write `room`'s governance. No recorded authority means
    /// this machine minted the room and governs it; joining another's
    /// room ([`Self::set_room`]) records [`RoomAuthority::Unpinned`],
    /// pinned to the named owner on invite accept
    /// ([`Self::pin_room_authority`]).
    pub fn room_authority(&self, room: RoomId) -> RoomAuthority {
        self.room_authority
            .get(&room)
            .cloned()
            .unwrap_or(RoomAuthority::Local)
    }

    /// Does this machine hold `room`'s governance?
    pub fn governs_room(&self, room: RoomId) -> bool {
        self.room_authority(room) == RoomAuthority::Local
    }

    /// Name the peer `peer_id` (iroh `EndpointId`) `name` in
    /// `room`, as `actor`. Deliberately the **inviter's** act, not the
    /// invitee's — a peer naming itself could claim any name. Refused
    /// unless `actor` may administer `room`. An empty `name` un-names
    /// the peer.
    pub fn name_peer(&mut self, room: RoomId, actor: &UserId, peer_id: &str, name: &str) -> bool {
        let peer_id = peer_id.trim();
        if room.is_nil() || peer_id.is_empty() {
            return false;
        }
        // Storing an unparseable peer id would leave a key no presence
        // frame can ever match — reads later as "didn't replicate"
        // rather than "was never addressed to anyone".
        if peer_id.parse::<iroh::EndpointId>().is_err() {
            return false;
        }
        if !self.governs_room(room) {
            return false;
        }
        let Some(ws) = self.workspace_in_room(room) else {
            return false;
        };
        if !ws.may_administer(actor) {
            return false;
        }
        let name = name.trim();
        let entry = self.room_peer_names.entry(room).or_default();
        let changed = if name.is_empty() {
            entry.remove(peer_id).is_some()
        } else {
            entry
                .insert(peer_id.to_string(), name.to_string())
                .as_deref()
                != Some(name)
        };
        if entry.is_empty() {
            self.room_peer_names.remove(&room);
        }
        if changed {
            self.sync_room_doc(room);
            self.flush_to_disk();
        }
        true
    }

    /// The names `room` carries, keyed by the peer **handle** the
    /// presence path speaks in
    /// ([`crate::network::behaviour::peer_liveness_handle`]), not the
    /// id — the document stores real identity, this bridges the gap. A
    /// peer with no entry is labelled with its handle.
    pub fn room_peer_labels(&self, room: RoomId) -> std::collections::BTreeMap<u64, String> {
        let mut out = std::collections::BTreeMap::new();
        // Room documents aren't persisted, so a just-restarted machine
        // holds none until something exports. On a machine that governs
        // the room, falling back to the cache avoids showing invitees
        // as numbers for the first few seconds; a guest gets no such
        // fallback.
        let named = if self.governs_room(room) {
            self.room_peer_names.get(&room).cloned().unwrap_or_default()
        } else {
            self.room_peer_names_in_doc(room)
        };
        for (peer, name) in named {
            let Ok(parsed) = peer.parse::<iroh::EndpointId>() else {
                continue;
            };
            out.insert(
                crate::network::behaviour::peer_liveness_handle(&parsed),
                name,
            );
        }
        out
    }

    /// Record that `room`'s governance is written by `peer` and nobody
    /// else. The value comes from a validated invite token (room id and
    /// owner peer id both inside the ed25519 signature), so a peer
    /// can't name itself authority. Refuses to overwrite an authority
    /// already pinned to a different peer.
    pub fn pin_room_authority(&mut self, room: RoomId, peer: PeerOrigin) -> bool {
        if room.is_nil() {
            return false;
        }
        if let Some(RoomAuthority::Peer(existing)) = self.room_authority.get(&room)
            && *existing != peer
        {
            tracing::warn!(
                target: "shell::state",
                room = %room,
                pinned = %existing,
                offered = %peer,
                "room authority already pinned to another peer; keeping it",
            );
            return false;
        }
        self.room_authority.insert(room, RoomAuthority::Peer(peer));
        self.flush_to_disk();
        true
    }

    /// Put `who` on the roster of the workspace that addresses `room`.
    /// An **upsert**, not a push — a later frame with no explicit
    /// colour must not erase an existing one. Returns whether the
    /// roster changed.
    pub fn admit_collaborator_in_room(&mut self, room: RoomId, who: Collaborator) -> bool {
        if room.is_nil() {
            return false;
        }
        let Some(ws) = self.workspaces.iter_mut().find(|w| w.room == room) else {
            return false;
        };
        Self::admit_into(&mut ws.collaborators, who)
    }

    /// [`Self::admit_collaborator_in_room`] addressed by this machine's
    /// local workspace handle instead of the room's durable identity.
    pub fn admit_collaborator(&mut self, workspace_id: WorkspaceId, who: Collaborator) -> bool {
        let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) else {
            return false;
        };
        Self::admit_into(&mut ws.collaborators, who)
    }

    /// Take `who` off the roster. Needed for the roster to mean
    /// anything — a list that only ever grows lies just as much as one
    /// seeded with people who never arrived.
    pub fn retire_collaborator_in_room(&mut self, room: RoomId, id: &str) -> bool {
        if room.is_nil() {
            return false;
        }
        let Some(ws) = self.workspaces.iter_mut().find(|w| w.room == room) else {
            return false;
        };
        let before = ws.collaborators.len();
        ws.collaborators.retain(|c| c.id != id);
        before != ws.collaborators.len()
    }

    /// Learn that the session `peer` is in `room` and that `user` is the
    /// person sitting at it. A session announces itself (via the
    /// accepted invite) before it can say who is at it, so this
    /// replaces the session-handle row when the name arrives rather
    /// than adding a second avatar. Unnamed stays under its handle.
    pub fn note_peer_present(&mut self, room: RoomId, peer: u64, user: &str) -> bool {
        if room.is_nil() {
            return false;
        }
        let handle = session_handle(peer);
        let named = !user.is_empty() && user != handle;
        let Some(ws) = self.workspaces.iter_mut().find(|w| w.room == room) else {
            return false;
        };
        let mut changed = false;
        if named {
            // The same session, previously known only by its handle.
            // Carry its colour across so a peer does not change hue at
            // the moment it acquires a name.
            if let Some(i) = ws.collaborators.iter().position(|c| c.id == handle) {
                let old = ws.collaborators.remove(i);
                changed = true;
                let mut upgraded = Collaborator::new(user, user).with_session(peer);
                upgraded.cursor_color = old.cursor_color;
                upgraded.role_id = old.role_id;
                let _ = Self::admit_into(&mut ws.collaborators, upgraded);
                return changed;
            }
        }
        let entry = if named {
            Collaborator::new(user, user).with_session(peer)
        } else {
            Collaborator::new(handle.clone(), handle).with_session(peer)
        };
        changed |= Self::admit_into(&mut ws.collaborators, entry);
        changed
    }

    /// Take every person who was here through `peer`'s session off every
    /// roster. Sweeps every workspace, not one: a session can be in
    /// several rooms at once, and it left all of them.
    pub fn retire_session(&mut self, peer: u64) -> bool {
        let mut changed = false;
        for ws in &mut self.workspaces {
            let before = ws.collaborators.len();
            ws.collaborators.retain(|c| c.session != Some(peer));
            changed |= before != ws.collaborators.len();
        }
        changed
    }

    /// Upsert one roster row. Split out so the room-addressed and
    /// handle-addressed entry points cannot drift apart.
    fn admit_into(roster: &mut Vec<Collaborator>, who: Collaborator) -> bool {
        if let Some(existing) = roster.iter_mut().find(|c| c.id == who.id) {
            let mut changed = false;
            if !who.name.is_empty() && existing.name != who.name {
                existing.name = who.name;
                changed = true;
            }
            if who.cursor_color.is_some() && existing.cursor_color != who.cursor_color {
                existing.cursor_color = who.cursor_color;
                changed = true;
            }
            if who.role_id.is_some() && existing.role_id != who.role_id {
                existing.role_id = who.role_id;
                changed = true;
            }
            // A row without a session (read off disk, or a fixture) is
            // invisible to `retire_session`; the arriving row is the one
            // that knows, so it wins.
            if who.session.is_some() && existing.session != who.session {
                existing.session = who.session;
                changed = true;
            }
            return changed;
        }
        roster.push(who);
        true
    }

    /// The cached workspace that addresses `room`, if this machine is in
    /// it. First match wins, in case of a room joined twice locally.
    fn workspace_in_room(&self, room: RoomId) -> Option<&Workspace> {
        if room.is_nil() {
            return None;
        }
        self.workspaces.iter().find(|w| w.room == room)
    }

    /// Project the cached state of `room`'s workspace into that room's
    /// replication document, writing only what differs; no-op when this
    /// machine is not in `room`.
    ///
    /// Every value is a map key, not a list element, so a re-projection
    /// under a fresh actor after a restart merges last-writer-wins
    /// instead of duplicating. A revoked capability is deleted from the
    /// map so the revocation reaches a peer already holding the grant —
    /// but the room document is not persisted, so that deletion has to
    /// be re-issued after a restart.
    pub fn sync_room_doc(&mut self, room: RoomId) {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

        let Some(ws) = self.workspace_in_room(room) else {
            return;
        };
        let name = ws.name.clone();
        let owner = ws.owner.clone();
        let caps: Vec<CapOverride> = ws.capabilities.clone();
        let peer_names: std::collections::BTreeMap<String, String> =
            self.room_peer_names.get(&room).cloned().unwrap_or_default();
        let held: std::collections::BTreeMap<String, String> = self
            .room_records_held
            .get(&room)
            .cloned()
            .unwrap_or_default();
        let strips: Vec<Strip> = ws.strips.clone();
        let room_str = room.to_string();
        let local_salt = self.local_salt();
        let share_floor = self.room_share_floor.get(&room).copied();
        // Governance is projected only by the machine that holds it —
        // a guest claiming ownership would put a competing claim on
        // the wire that `apply_room_changes` would only refuse anyway.
        let governs = self.governs_room(room);

        let doc = self.room_docs.entry(room).or_default();

        // Identity first: the receiver checks this field against the
        // room it asked for, so an unlabelled document could be applied
        // to the wrong one.
        let current_room = match doc.get(&ROOT, "room").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        };
        if current_room.as_deref() != Some(room_str.as_str()) {
            let _ = doc.put(&ROOT, "room", room_str.clone());
        }

        let current_name = match doc.get(&ROOT, "name").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        };
        if current_name.as_deref() != Some(name.as_str()) {
            let _ = doc.put(&ROOT, "name", name);
        }

        let current_owner = match doc.get(&ROOT, "owner").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        };
        match &owner {
            Some(o) if governs && current_owner.as_deref() != Some(o.as_str()) => {
                let _ = doc.put(&ROOT, "owner", o.clone());
            }
            // Never unset in a governed room — `adopt_ownerless_workspaces`
            // only ever fills the field in.
            _ => {}
        }

        // Projected by **every** member, unlike governance below: a room
        // a guest can't bring a record — or a window — into isn't shared.
        Self::project_room_records(doc, &held);
        Self::project_owned_contents(doc, &strips, local_salt, share_floor);

        if !governs {
            let _ = qdoc::history::commit_stamped(doc);
            doc.reset_diff_cursor();
            return;
        }

        let caps_id = match doc.get(&ROOT, "capabilities").ok().flatten() {
            Some((_, id)) => id,
            None => match doc.put_object(&ROOT, "capabilities", ObjType::Map) {
                Ok(id) => id,
                Err(err) => {
                    tracing::error!(?err, room = %room_str, "room doc: capabilities map");
                    return;
                }
            },
        };
        let live: std::collections::HashSet<&str> = caps.iter().map(|c| c.id.as_str()).collect();
        let stale: Vec<String> = doc
            .keys(&caps_id)
            .filter(|k| !live.contains(k.as_str()))
            .collect();
        for key in stale {
            let _ = doc.delete(&caps_id, key.as_str());
        }
        for cap in &caps {
            let Ok(blob) = serde_json::to_vec(cap) else {
                continue;
            };
            let unchanged = matches!(
                doc.get(&caps_id, &cap.id).ok().flatten(),
                Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Bytes(b) if b == &blob)
            );
            if !unchanged {
                let _ = doc.put(&caps_id, &cap.id, ScalarValue::Bytes(blob));
            }
        }
        Self::project_peer_names(doc, &peer_names);
        let _ = qdoc::history::commit_stamped(doc);
        // Nothing reads this doc through patches, so the log is pure
        // overhead — worse, `PatchLog::migrate_actors` panics when a
        // peer's actor can't be spliced into it in sort order.
        doc.reset_diff_cursor();
    }

    /// Key the room document stores its peers' names under.
    const ROOM_PEERS_KEY: &'static str = "peers";

    /// Project "who the peers in this room are", as Iroh `EndpointId`
    /// -> display name. Called only from the governing branch of
    /// [`Self::sync_room_doc`] — one writer, so no guest can rename
    /// itself or anybody else.
    fn project_peer_names(
        doc: &mut AutoCommit,
        names: &std::collections::BTreeMap<String, String>,
    ) {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

        let peers_id = if let Some((_, id)) = doc.get(&ROOT, Self::ROOM_PEERS_KEY).ok().flatten() {
            id
        } else {
            // Don't mint an empty map — a room nobody has named
            // anyone in produces no change at all.
            if names.is_empty() {
                return;
            }
            match doc.put_object(&ROOT, Self::ROOM_PEERS_KEY, ObjType::Map) {
                Ok(id) => id,
                Err(err) => {
                    tracing::error!(?err, "room doc: peers map");
                    return;
                }
            }
        };
        let stale: Vec<String> = doc
            .keys(&peers_id)
            .filter(|k| !names.contains_key(k.as_str()))
            .collect();
        for key in stale {
            let _ = doc.delete(&peers_id, key.as_str());
        }
        for (peer, name) in names {
            let unchanged = matches!(
                doc.get(&peers_id, peer).ok().flatten(),
                Some((Value::Scalar(s), _))
                    if matches!(s.as_ref(), ScalarValue::Str(v) if v.as_str() == name)
            );
            if !unchanged {
                let _ = doc.put(&peers_id, peer, name.clone());
            }
        }
    }

    /// Every name the room document carries. Read from the **document**,
    /// not [`Self::room_peer_names`], so a guest and the host answer this
    /// the same way — one source, and it's the authenticated one.
    pub fn room_peer_names_in_doc(
        &self,
        room: RoomId,
    ) -> std::collections::BTreeMap<String, String> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let mut out = std::collections::BTreeMap::new();
        let Some(doc) = self.room_docs.get(&room) else {
            return out;
        };
        let Some((_, peers_id)) = doc.get(&ROOT, Self::ROOM_PEERS_KEY).ok().flatten() else {
            return out;
        };
        let keys: Vec<String> = doc.keys(&peers_id).collect();
        for key in keys {
            if let Some((Value::Scalar(s), _)) = doc.get(&peers_id, &key).ok().flatten()
                && let ScalarValue::Str(v) = s.as_ref()
                && !v.as_str().is_empty()
            {
                out.insert(key, v.to_string());
            }
        }
        out
    }

    /// Key the room document stores the room's window contents under.
    const ROOM_CONTENTS_KEY: &'static str = "contents";

    /// A strip / group / tab id as a `contents` map key: fixed-width hex,
    /// so the key parses back into the id it names and two peers spell
    /// one id the same way.
    fn contents_key(id: u64) -> String {
        format!("{id:016x}")
    }

    /// The id a `contents` key names, or `None` for a key this build did
    /// not write.
    fn contents_key_id(key: &str) -> Option<u64> {
        u64::from_str_radix(key, 16).ok()
    }

    /// Project the strips, window groups and tabs this machine owns into
    /// the room document's `contents` map. The `active_*` pointers are
    /// excluded — projecting them would drag a colleague off the page
    /// they were on whenever somebody else changed tabs.
    ///
    /// Every entry is written only by the peer whose salt its id carries
    /// ([`peer_salt_of`]) — a single-writer rule
    /// [`Self::apply_room_changes`] enforces, so last-writer-wins has
    /// nothing to arbitrate. Entries carry their index; readers sort by
    /// `(order, id)` so peers appending at once still agree, and the
    /// sweep at each level deletes only entries this machine owns. Salt
    /// `0` has no id space, so nothing is projected or merged there.
    fn project_owned_contents(
        doc: &mut AutoCommit,
        strips: &[Strip],
        local_salt: u16,
        share_floor: Option<u64>,
    ) {
        use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
        use std::collections::HashSet;

        if local_salt == 0 {
            return;
        }
        // "Mine to publish": minted by this peer, and since it joined
        // (not before) if this is a room it joined rather than minted.
        let mine = |id: u64| -> bool {
            peer_salt_of(id) == local_salt
                && share_floor.is_none_or(|floor| counter_of(id) >= floor)
        };

        let contents_id = match doc.get(&ROOT, Self::ROOM_CONTENTS_KEY).ok().flatten() {
            Some((_, id)) => id,
            None => match doc.put_object(&ROOT, Self::ROOM_CONTENTS_KEY, ObjType::Map) {
                Ok(id) => id,
                Err(err) => {
                    tracing::error!(?err, "room doc: contents map");
                    return;
                }
            },
        };

        let mut live_strips: HashSet<String> = HashSet::new();
        for (si, strip) in strips.iter().enumerate() {
            // An empty strip holds no state; an empty row arriving on
            // someone else's desktop is noise, not news.
            if strip.window_groups.is_empty() {
                continue;
            }
            let owns_strip = mine(strip.id.raw());
            // A peer's strip is still walked: a window we opened into it
            // is ours to publish even though the row isn't.
            let has_owned = strip
                .window_groups
                .iter()
                .any(|g| mine(g.id) || g.tabs.iter().any(|t| mine(t.id)));
            if !owns_strip && !has_owned {
                continue;
            }

            let skey = Self::contents_key(strip.id.raw());
            let strip_entry = match doc.get(&contents_id, skey.as_str()).ok().flatten() {
                Some((_, id)) => id,
                None => match doc.put_object(&contents_id, skey.as_str(), ObjType::Map) {
                    Ok(id) => id,
                    Err(err) => {
                        tracing::error!(?err, "room doc: strip entry");
                        continue;
                    }
                },
            };
            if owns_strip {
                live_strips.insert(skey.clone());
                put_str_if_changed(doc, &strip_entry, "name", &strip.name);
                put_uint_if_changed(doc, &strip_entry, "order", si as u64);
            }

            Self::project_owned_groups(doc, &strip_entry, strip, &mine);
        }
        let stale: Vec<String> = doc
            .keys(&contents_id)
            .filter(|k| {
                !live_strips.contains(k.as_str()) && Self::contents_key_id(k).is_some_and(&mine)
            })
            .collect();
        for key in stale {
            let _ = doc.delete(&contents_id, key.as_str());
        }
    }

    /// [`Self::project_owned_contents`]'s per-strip pass: get-or-create
    /// `strip_entry`'s `groups` map, project every group this machine owns
    /// (or owns a tab within), then sweep group keys this machine owns
    /// that no longer appear among `strip`'s groups.
    fn project_owned_groups(
        doc: &mut AutoCommit,
        strip_entry: &automerge::ObjId,
        strip: &Strip,
        mine: &impl Fn(u64) -> bool,
    ) {
        use automerge::{ObjType, ReadDoc, transaction::Transactable};
        use std::collections::HashSet;

        let groups_id = match doc.get(strip_entry, "groups").ok().flatten() {
            Some((_, id)) => id,
            None => match doc.put_object(strip_entry, "groups", ObjType::Map) {
                Ok(id) => id,
                Err(err) => {
                    tracing::error!(?err, "room doc: groups map");
                    return;
                }
            },
        };
        let mut live_groups: HashSet<String> = HashSet::new();
        for (gi, group) in strip.window_groups.iter().enumerate() {
            let owns_group = mine(group.id);
            let has_owned_tabs = group.tabs.iter().any(|t| mine(t.id));
            if !owns_group && !has_owned_tabs {
                continue;
            }
            let gkey = Self::contents_key(group.id);
            let group_entry = match doc.get(&groups_id, gkey.as_str()).ok().flatten() {
                Some((_, id)) => id,
                None => match doc.put_object(&groups_id, gkey.as_str(), ObjType::Map) {
                    Ok(id) => id,
                    Err(err) => {
                        tracing::error!(?err, "room doc: group entry");
                        continue;
                    }
                },
            };
            if owns_group {
                live_groups.insert(gkey.clone());
                put_uint_if_changed(doc, &group_entry, "order", gi as u64);
                put_uint_if_changed(
                    doc,
                    &group_entry,
                    "width_permille",
                    u64::from(group.width.permille()),
                );
                put_bool_if_changed(doc, &group_entry, "maximized", group.maximized);
                // `active_tab` is not written here. It is the one
                // field of this struct that belongs to the person
                // rather than to the room.
            }

            Self::project_owned_tabs(doc, &group_entry, group, mine);
        }
        let stale: Vec<String> = doc
            .keys(&groups_id)
            .filter(|k| {
                !live_groups.contains(k.as_str()) && Self::contents_key_id(k).is_some_and(mine)
            })
            .collect();
        for key in stale {
            let _ = doc.delete(&groups_id, key.as_str());
        }
    }

    /// [`Self::project_owned_groups`]'s per-group pass: get-or-create
    /// `group_entry`'s `tabs` map, project every tab this machine owns,
    /// then sweep tab keys this machine owns that no longer appear among
    /// `group`'s tabs.
    fn project_owned_tabs(
        doc: &mut AutoCommit,
        group_entry: &automerge::ObjId,
        group: &WindowGroup,
        mine: &impl Fn(u64) -> bool,
    ) {
        use automerge::{ObjType, ReadDoc, ScalarValue, transaction::Transactable};
        use std::collections::HashSet;

        let tabs_id = match doc.get(group_entry, "tabs").ok().flatten() {
            Some((_, id)) => id,
            None => match doc.put_object(group_entry, "tabs", ObjType::Map) {
                Ok(id) => id,
                Err(err) => {
                    tracing::error!(?err, "room doc: tabs map");
                    return;
                }
            },
        };
        let mut live_tabs: HashSet<String> = HashSet::new();
        for (ti, tab) in group.tabs.iter().enumerate() {
            if !mine(tab.id) {
                continue;
            }
            let tkey = Self::contents_key(tab.id);
            live_tabs.insert(tkey.clone());
            let tab_entry = match doc.get(&tabs_id, tkey.as_str()).ok().flatten() {
                Some((_, id)) => id,
                None => match doc.put_object(&tabs_id, tkey.as_str(), ObjType::Map) {
                    Ok(id) => id,
                    Err(err) => {
                        tracing::error!(?err, "room doc: tab entry");
                        continue;
                    }
                },
            };
            put_uint_if_changed(doc, &tab_entry, "order", ti as u64);
            put_str_if_changed(doc, &tab_entry, "title", &tab.title);
            if let Ok(blob) = serde_json::to_vec(&tab.tile) {
                let unchanged = matches!(
                    doc.get(&tab_entry, "tile_json").ok().flatten(),
                    Some((automerge::Value::Scalar(v), _))
                        if matches!(v.as_ref(), ScalarValue::Bytes(b) if b == &blob)
                );
                if !unchanged {
                    let _ = doc.put(&tab_entry, "tile_json", ScalarValue::Bytes(blob));
                }
            }
        }
        let stale: Vec<String> = doc
            .keys(&tabs_id)
            .filter(|k| {
                !live_tabs.contains(k.as_str()) && Self::contents_key_id(k).is_some_and(mine)
            })
            .collect();
        for key in stale {
            let _ = doc.delete(&tabs_id, key.as_str());
        }
    }

    /// Read a room document's `contents` back into plain values. Every
    /// level sorts by `(order, id)`; an entry with no `order` sorts
    /// last so it can't displace one whose owner did state a position.
    fn contents_of(doc: &AutoCommit) -> Vec<ReplicatedStrip> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};

        let uint = |obj: &automerge::ObjId, key: &str| -> Option<u64> {
            match doc.get(obj, key).ok().flatten() {
                Some((Value::Scalar(v), _)) => match v.as_ref() {
                    ScalarValue::Uint(n) => Some(*n),
                    // A negative `Int` has no sane `order`/id reading, so
                    // treat it as absent rather than wrapping it into a
                    // huge `u64` via a bit-reinterpreting cast.
                    ScalarValue::Int(n) => u64::try_from(*n).ok(),
                    _ => None,
                },
                _ => None,
            }
        };
        let text = |obj: &automerge::ObjId, key: &str| -> Option<String> {
            match doc.get(obj, key).ok().flatten() {
                Some((Value::Scalar(v), _)) => match v.as_ref() {
                    ScalarValue::Str(t) => Some(t.to_string()),
                    _ => None,
                },
                _ => None,
            }
        };

        let Some((_, contents_id)) = doc.get(&ROOT, Self::ROOM_CONTENTS_KEY).ok().flatten() else {
            return Vec::new();
        };
        let mut strips: Vec<ReplicatedStrip> = Vec::new();
        for skey in doc.keys(&contents_id) {
            let Some(sid) = Self::contents_key_id(&skey) else {
                continue;
            };
            let Some((_, strip_entry)) = doc.get(&contents_id, skey.as_str()).ok().flatten() else {
                continue;
            };
            let mut groups: Vec<ReplicatedGroup> = Vec::new();
            if let Some((_, groups_id)) = doc.get(&strip_entry, "groups").ok().flatten() {
                for gkey in doc.keys(&groups_id) {
                    let Some(gid) = Self::contents_key_id(&gkey) else {
                        continue;
                    };
                    let Some((_, group_entry)) = doc.get(&groups_id, gkey.as_str()).ok().flatten()
                    else {
                        continue;
                    };
                    let mut tabs: Vec<ReplicatedTab> = Vec::new();
                    if let Some((_, tabs_id)) = doc.get(&group_entry, "tabs").ok().flatten() {
                        for tkey in doc.keys(&tabs_id) {
                            let Some(tid) = Self::contents_key_id(&tkey) else {
                                continue;
                            };
                            let Some((_, tab_entry)) =
                                doc.get(&tabs_id, tkey.as_str()).ok().flatten()
                            else {
                                continue;
                            };
                            let tile = match doc.get(&tab_entry, "tile_json").ok().flatten() {
                                Some((Value::Scalar(v), _)) => match v.as_ref() {
                                    ScalarValue::Bytes(b) => serde_json::from_slice(b).ok(),
                                    _ => None,
                                },
                                _ => None,
                            };
                            tabs.push(ReplicatedTab {
                                id: tid,
                                order: uint(&tab_entry, "order").unwrap_or(u64::MAX),
                                title: text(&tab_entry, "title").unwrap_or_default(),
                                tile,
                            });
                        }
                    }
                    tabs.sort_by_key(|t| (t.order, t.id));
                    groups.push(ReplicatedGroup {
                        id: gid,
                        order: uint(&group_entry, "order").unwrap_or(u64::MAX),
                        width: uint(&group_entry, "width_permille").map(|p| {
                            // Clamped into u16's range on the line
                            // above, so this never actually truncates.
                            WindowWidth::Fraction(
                                u16::try_from(p.min(u64::from(u16::MAX))).unwrap_or(u16::MAX),
                            )
                        }),
                        maximized: match doc.get(&group_entry, "maximized").ok().flatten() {
                            Some((Value::Scalar(v), _)) => match v.as_ref() {
                                ScalarValue::Boolean(b) => Some(*b),
                                _ => None,
                            },
                            _ => None,
                        },
                        tabs,
                    });
                }
            }
            groups.sort_by_key(|g| (g.order, g.id));
            strips.push(ReplicatedStrip {
                id: sid,
                order: uint(&strip_entry, "order").unwrap_or(u64::MAX),
                name: text(&strip_entry, "name"),
                groups,
            });
        }
        strips.sort_by_key(|s| (s.order, s.id));
        strips
    }

    /// Every value in `doc`'s `contents` that this machine owns, as a
    /// comparable digest. [`Self::apply_room_changes`] diffs this
    /// before/after a peer's batch and refuses it whole if any owned
    /// entry changed — enforcing "one writer per entry".
    fn owned_contents_digest(doc: Option<&AutoCommit>, local_salt: u16) -> Vec<(u64, String)> {
        let Some(doc) = doc else {
            return Vec::new();
        };
        if local_salt == 0 {
            return Vec::new();
        }
        let mut out: Vec<(u64, String)> = Vec::new();
        for strip in Self::contents_of(doc) {
            if peer_salt_of(strip.id) == local_salt {
                out.push((
                    strip.id,
                    format!(
                        "s|{}|{}",
                        strip.order,
                        strip.name.clone().unwrap_or_default()
                    ),
                ));
            }
            for group in &strip.groups {
                if peer_salt_of(group.id) == local_salt {
                    out.push((
                        group.id,
                        format!("g|{}|{:?}|{:?}", group.order, group.width, group.maximized),
                    ));
                }
                for tab in &group.tabs {
                    if peer_salt_of(tab.id) == local_salt {
                        out.push((
                            tab.id,
                            format!("t|{}|{}|{:?}", tab.order, tab.title, tab.tile),
                        ));
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// Fold the room document's `contents` into the cached workspace
    /// addressing `room`, reporting whether anything changed. Entries
    /// this machine owns are never overwritten or dropped — the document
    /// is downstream of the cache, so reading one back could only revert
    /// an unprojected local change; a peer's entries are taken from the
    /// document and dropped when absent.
    ///
    /// `active_tab` is never read from the document: it stays put unless
    /// genuinely gone, and a group arriving from a peer opens on its
    /// first tab. Vanishing strips take their `active_strip`/
    /// `active_group` with them so resolvers fall back rather than point
    /// at nothing.
    fn merge_room_contents(&mut self, room: RoomId) -> bool {
        let local_salt = self.local_salt();
        if local_salt == 0 {
            return false;
        }
        let replicated = {
            let Some(doc) = self.room_docs.get(&room) else {
                return false;
            };
            Self::contents_of(doc)
        };
        let is_local = |id: u64| peer_salt_of(id) == local_salt;

        // Read before the workspace is borrowed mutably: both live on
        // the tree rather than on the workspace.
        let host_strip = self
            .workspaces
            .iter()
            .find(|w| w.room == room)
            .and_then(|w| {
                let active = self.active_strip.get(&w.id).copied();
                active
                    .filter(|id| w.strips.iter().any(|s| s.id == *id && is_local(id.raw())))
                    .or_else(|| w.strips.iter().find(|s| is_local(s.id.raw())).map(|s| s.id))
            });

        let Some(ws) = self.workspaces.iter_mut().find(|w| w.room == room) else {
            return false;
        };
        let mut changed = false;
        let mut by_strip: std::collections::BTreeMap<u64, ReplicatedStrip> =
            replicated.into_iter().map(|s| (s.id, s)).collect();

        // **Which strip a peer's windows land in is this machine's
        // business** — taking a peer's strip id literally would mint a
        // second strip that stacks below ours; instead their groups
        // merge into the strip we're using, keeping their own ids.
        let foreign: Vec<u64> = by_strip
            .keys()
            .copied()
            .filter(|id| !is_local(*id))
            .collect();
        let mut peer_groups: Vec<ReplicatedGroup> = Vec::new();
        for sid in foreign {
            // `sid` came from `by_strip.keys()` just above and `BTreeMap`
            // keys don't repeat, so this always finds an entry — but
            // `continue` rather than `.expect()` costs nothing and can't
            // panic if that invariant ever slips.
            let Some(rep) = by_strip.remove(&sid) else {
                continue;
            };
            peer_groups.extend(rep.groups);
        }
        // No strip of ours to put them in, so one is made.
        if !peer_groups.is_empty() && host_strip.is_none() {
            ws.strips.push(Strip::new(StripId::from(next_id()), "Main"));
            changed = true;
        }
        let host_strip = host_strip.or_else(|| ws.strips.last().map(|s| s.id));

        // `(order, id, strip)` so local-only strips keep a position too.
        let mut out: Vec<(u64, u64, Strip)> = Vec::new();
        // Old strip ids that don't make it into `out` below — collected
        // as the loop drops them rather than diffed afterward against a
        // snapshot, since `ws.strips` is about to be overwritten with the
        // merged result.
        let mut vanished: Vec<StripId> = Vec::new();
        let mut peer_groups = Some(peer_groups);
        for (i, strip) in std::mem::take(&mut ws.strips).into_iter().enumerate() {
            let sid = strip.id.raw();
            // Everybody else's windows join this one.
            let adopting = (Some(strip.id) == host_strip)
                .then(|| peer_groups.take())
                .flatten()
                .filter(|g| !g.is_empty());
            let published = by_strip.remove(&sid);
            if published.is_none() && adopting.is_none() {
                if is_local(sid) {
                    // Ours and simply not published; the document has
                    // no say over it.
                    out.push((i as u64, sid, strip));
                } else {
                    // A foreign strip no longer being merged in — drop it.
                    changed = true;
                    vanished.push(StripId::from(sid));
                }
                continue;
            }
            let mut rep = published.unwrap_or_else(|| ReplicatedStrip {
                id: sid,
                order: i as u64,
                name: None,
                groups: Vec::new(),
            });
            // Ours keeps our cache's position — taking it from the
            // document would undo a local reorder until re-projected.
            let order = if is_local(sid) { i as u64 } else { rep.order };
            if let Some(extra) = adopting {
                rep.groups.extend(extra);
            }
            out.push((
                order,
                sid,
                Self::merge_strip(strip, rep, &is_local, &mut changed),
            ));
        }
        for (sid, rep) in by_strip {
            out.push((rep.order, sid, Self::hydrate_strip(rep)));
            changed = true;
        }
        out.sort_by_key(|(order, id, _)| (*order, *id));
        ws.strips = out.into_iter().map(|(_, _, s)| s).collect();

        let ws_id = ws.id;
        let live: Vec<StripId> = ws.strips.iter().map(|s| s.id).collect();

        // Removed rather than repointed: resolvers already fall back to
        // the first strip/group with no explicit selection.
        if self
            .active_strip
            .get(&ws_id)
            .is_some_and(|active| !live.contains(active))
        {
            self.active_strip.remove(&ws_id);
        }
        for strip in vanished {
            self.active_group.remove(&strip);
        }
        changed
    }

    /// Reconcile one strip against what the room document says about it.
    fn merge_strip(
        mut local: Strip,
        rep: ReplicatedStrip,
        is_local: &dyn Fn(u64) -> bool,
        changed: &mut bool,
    ) -> Strip {
        // A name is only taken from the document for a strip we do not
        // own; ours is ours to write.
        if !is_local(local.id.raw())
            && let Some(name) = rep.name
            && local.name != name
        {
            local.name = name;
            *changed = true;
        }
        let mut by_group: std::collections::BTreeMap<u64, ReplicatedGroup> =
            rep.groups.into_iter().map(|g| (g.id, g)).collect();

        let mut out: Vec<(u64, u64, WindowGroup)> = Vec::new();
        for (i, group) in std::mem::take(&mut local.window_groups)
            .into_iter()
            .enumerate()
        {
            let gid = group.id;
            match by_group.remove(&gid) {
                Some(rep) => {
                    let order = if is_local(gid) { i as u64 } else { rep.order };
                    out.push((order, gid, Self::merge_group(group, rep, is_local, changed)));
                }
                None if is_local(gid) => out.push((i as u64, gid, group)),
                None => *changed = true,
            }
        }
        for (gid, rep) in by_group {
            out.push((rep.order, gid, Self::hydrate_group(rep)));
            *changed = true;
        }
        out.sort_by_key(|(order, id, _)| (*order, *id));
        local.window_groups = out.into_iter().map(|(_, _, g)| g).collect();
        local
    }

    /// Reconcile one window group. **This is where view state is kept.**
    fn merge_group(
        mut local: WindowGroup,
        rep: ReplicatedGroup,
        is_local: &dyn Fn(u64) -> bool,
        changed: &mut bool,
    ) -> WindowGroup {
        // Geometry travels only from the peer that owns the group — for
        // one of ours the document is an echo of what we last projected,
        // and taking it back would revert an unpublished resize.
        if !is_local(local.id) {
            if let Some(width) = rep.width
                && local.width.permille() != width.permille()
            {
                local.width = width;
                *changed = true;
            }
            if let Some(maximized) = rep.maximized
                && local.maximized != maximized
            {
                local.maximized = maximized;
                *changed = true;
            }
        }

        let mut by_tab: std::collections::BTreeMap<u64, ReplicatedTab> =
            rep.tabs.into_iter().map(|t| (t.id, t)).collect();
        let mut out: Vec<(u64, u64, Tab)> = Vec::new();
        for (i, tab) in std::mem::take(&mut local.tabs).into_iter().enumerate() {
            let tid = tab.id;
            match by_tab.remove(&tid) {
                Some(rep) => {
                    let order = if is_local(tid) { i as u64 } else { rep.order };
                    let mut tab = tab;
                    if !is_local(tid) {
                        if tab.title != rep.title {
                            tab.title.clone_from(&rep.title);
                            *changed = true;
                        }
                        if let Some(tile) = rep.tile.clone() {
                            let tile = Self::localise_tile(tile, Some(&tab.tile), &rep.title);
                            if serde_json::to_vec(&tab.tile).ok() != serde_json::to_vec(&tile).ok()
                            {
                                tab.tile = tile;
                                *changed = true;
                            }
                        }
                    }
                    out.push((order, tid, tab));
                }
                None if is_local(tid) => out.push((i as u64, tid, tab)),
                None => *changed = true,
            }
        }
        for (tid, rep) in by_tab {
            out.push((rep.order, tid, Self::hydrate_tab(rep)));
            *changed = true;
        }
        out.sort_by_key(|(order, id, _)| (*order, *id));
        local.tabs = out.into_iter().map(|(_, _, t)| t).collect();

        // `active_tab` is not taken from `rep` — the document doesn't
        // carry one. Moves only when the tab is genuinely gone.
        if !local.tabs.iter().any(|t| t.id == local.active_tab)
            && let Some(first) = local.tabs.first()
        {
            local.active_tab = first.id;
        }
        local
    }

    /// A strip that arrived from a peer.
    fn hydrate_strip(rep: ReplicatedStrip) -> Strip {
        Strip {
            id: StripId::from(rep.id),
            name: rep.name.unwrap_or_default(),
            window_groups: rep.groups.into_iter().map(Self::hydrate_group).collect(),
        }
    }

    /// A window group that arrived from a peer. Its `active_tab` is
    /// chosen **here** — a local decision about a local view, not the
    /// opening peer's to make.
    fn hydrate_group(rep: ReplicatedGroup) -> WindowGroup {
        let tabs: Vec<Tab> = rep.tabs.into_iter().map(Self::hydrate_tab).collect();
        let active_tab = tabs.first().map(|t| t.id).unwrap_or_default();
        WindowGroup {
            id: rep.id,
            tabs,
            active_tab,
            width: rep.width.unwrap_or(WindowWidth::Normal),
            maximized: rep.maximized.unwrap_or(false),
        }
    }

    /// A tab that arrived from a peer.
    fn hydrate_tab(rep: ReplicatedTab) -> Tab {
        let title = rep.title;
        Tab {
            tile: rep.tile.map_or(
                TileContent::Stub {
                    kind: AppKind::Other(String::new()),
                    label: String::new(),
                },
                |tile| Self::localise_tile(tile, None, &title),
            ),
            id: rep.id,
            title,
        }
    }

    /// Make a peer's tile mean something on this machine: a
    /// [`TileContent::Foreign`] `wl_surface` id only resolves on the
    /// compositor hosting that client, so the surface is dropped and the
    /// app id survives as [`TileContent::PeerWindow`]. `current` is
    /// preferred once a local instance is bound, or the peer's
    /// re-projection would take the window away again every round.
    fn localise_tile(
        tile: TileContent,
        current: Option<&TileContent>,
        tab_title: &str,
    ) -> TileContent {
        match tile {
            TileContent::Foreign { app_id, .. } => {
                // Already resolved here, to our own instance or the same
                // waypoint — the remote tile has nothing to add.
                match current {
                    Some(
                        matched @ (TileContent::Foreign { app_id: local, .. }
                        | TileContent::PeerWindow { app_id: local, .. }),
                    ) if local.eq_ignore_ascii_case(&app_id) => {
                        return matched.clone();
                    }
                    _ => {}
                }
                let label = if tab_title.trim().is_empty() {
                    app_id.clone()
                } else {
                    tab_title.to_string()
                };
                TileContent::PeerWindow {
                    app_id,
                    label,
                    note: String::new(),
                }
            }
            other => other,
        }
    }

    /// Key the room document stores a room's record index under, mirroring
    /// [`ui::record::room`]'s shape (id -> `kind`) in the document that
    /// crosses the wire.
    const ROOM_RECORDS_KEY: &'static str = "records";

    /// Project a room's record index into that room's replication
    /// document. A room holds a *reference* (id and type), never the
    /// record itself — documents travel per-record over the broker.
    ///
    /// Additive only: a key this machine doesn't hold is left alone,
    /// not swept, since sweeping would race a peer's record that hasn't
    /// reached our index yet. Cost: an un-reference doesn't propagate
    /// until removal becomes an explicit tombstone.
    fn project_room_records(
        doc: &mut AutoCommit,
        held: &std::collections::BTreeMap<String, String>,
    ) {
        use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};

        if held.is_empty() {
            return;
        }
        let records_id = match doc.get(&ROOT, Self::ROOM_RECORDS_KEY).ok().flatten() {
            Some((_, id)) => id,
            None => match doc.put_object(&ROOT, Self::ROOM_RECORDS_KEY, ObjType::Map) {
                Ok(id) => id,
                Err(err) => {
                    tracing::error!(?err, "room doc: records map");
                    return;
                }
            },
        };
        for (id, kind) in held {
            let entry = match doc.get(&records_id, id.as_str()).ok().flatten() {
                Some((_, obj)) => obj,
                None => match doc.put_object(&records_id, id.as_str(), ObjType::Map) {
                    Ok(obj) => obj,
                    Err(err) => {
                        tracing::error!(?err, record = %id, "room doc: record entry");
                        continue;
                    }
                },
            };
            // Compared before writing, like every other value here — a
            // re-projection of unchanged state must not manufacture one.
            let current = match doc.get(&entry, "kind").ok().flatten() {
                Some((automerge::Value::Scalar(v), _)) => match v.as_ref() {
                    automerge::ScalarValue::Str(k) => Some(k.to_string()),
                    _ => None,
                },
                _ => None,
            };
            if current.as_deref() != Some(kind.as_str()) {
                let _ = doc.put(&entry, "kind", kind.clone());
            }
        }
    }

    /// Read a room document's record index back as `(record id, kind)`
    /// pairs, sorted by id so two peers read one document the same way.
    fn room_records_of(doc: &AutoCommit) -> Vec<(String, String)> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};

        let Some((_, records_id)) = doc.get(&ROOT, Self::ROOM_RECORDS_KEY).ok().flatten() else {
            return Vec::new();
        };
        let mut out: Vec<(String, String)> = Vec::new();
        for key in doc.keys(&records_id) {
            let Some((_, entry)) = doc.get(&records_id, key.as_str()).ok().flatten() else {
                continue;
            };
            let kind = match doc.get(&entry, "kind").ok().flatten() {
                Some((Value::Scalar(v), _)) => match v.as_ref() {
                    ScalarValue::Str(k) => k.to_string(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            out.push((key, kind));
        }
        out.sort();
        out
    }

    /// Tell the tree which records `room`'s index holds. Returns whether
    /// this was news, so the caller can skip re-asking when nothing moved.
    pub fn set_room_records(
        &mut self,
        room: RoomId,
        held: std::collections::BTreeMap<String, String>,
    ) -> bool {
        match self.room_records_held.get(&room) {
            Some(existing) if *existing == held => false,
            _ => {
                self.room_records_held.insert(room, held);
                true
            }
        }
    }

    /// Take the references peers have added since this was last called.
    /// Drained rather than read so a queue that only grows isn't a leak.
    pub fn take_inbound_room_records(&mut self) -> Vec<(RoomId, String, String)> {
        std::mem::take(&mut self.room_records_inbound)
    }

    /// The governance a room document records: owner and capability
    /// list — a peer able to write either controls the whole security
    /// policy. The room's **name** is deliberately not governed. The
    /// **id** is stricter still: a mismatched payload is refused outright.
    fn governance_of(doc: Option<&AutoCommit>) -> RoomGovernance {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let Some(doc) = doc else {
            return (
                None,
                std::collections::BTreeMap::new(),
                std::collections::BTreeMap::new(),
            );
        };
        let owner = match doc.get(&ROOT, "owner").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        };
        let mut caps = std::collections::BTreeMap::new();
        if let Some((_, caps_id)) = doc.get(&ROOT, "capabilities").ok().flatten() {
            let keys: Vec<String> = doc.keys(&caps_id).collect();
            for key in keys {
                if let Some((Value::Scalar(s), _)) = doc.get(&caps_id, &key).ok().flatten()
                    && let ScalarValue::Bytes(blob) = s.as_ref()
                {
                    caps.insert(key, blob.clone());
                }
            }
        }
        // Peer names fold in beside the capability list, the same kind
        // of claim — left out, it'd be the one governance key a guest
        // could write, and an impostor's first target.
        let mut peers = std::collections::BTreeMap::new();
        if let Some((_, peers_id)) = doc.get(&ROOT, Self::ROOM_PEERS_KEY).ok().flatten() {
            let keys: Vec<String> = doc.keys(&peers_id).collect();
            for key in keys {
                if let Some((Value::Scalar(s), _)) = doc.get(&peers_id, &key).ok().flatten()
                    && let ScalarValue::Str(v) = s.as_ref()
                {
                    peers.insert(key, v.to_string());
                }
            }
        }
        (owner, caps, peers)
    }

    /// Current heads of `room`'s replication document — the baseline a
    /// peer sends so the answer carries only what it's missing. Empty
    /// for a room this machine is not in.
    pub fn room_heads(&mut self, room: RoomId) -> Vec<ChangeHash> {
        match self.room_docs.get_mut(&room) {
            Some(doc) => doc.get_heads(),
            None => Vec::new(),
        }
    }

    /// Changes to `room` since `since`, as opaque blobs to ship to a
    /// peer. `None` when this machine has no workspace in `room`; the
    /// document is brought up to date from the cache first.
    pub fn export_room_changes_since(
        &mut self,
        room: RoomId,
        since: &[ChangeHash],
    ) -> Option<Vec<Vec<u8>>> {
        self.workspace_in_room(room)?;
        self.sync_room_doc(room);
        let doc = self.room_docs.get_mut(&room)?;
        Some(
            doc.get_changes(since)
                .into_iter()
                .map(|c| c.raw_bytes().to_vec())
                .collect(),
        )
    }

    /// True (and logs a warning) if the payload's own `room` field
    /// disagrees with the room it was sent to answer.
    fn declared_room_mismatches(candidate: &AutoCommit, room: RoomId) -> bool {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let declared = match candidate.get(&ROOT, "room").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => RoomId::parse(v.as_str()),
                _ => None,
            },
            _ => None,
        };
        if declared.is_some_and(|d| d != room) {
            tracing::warn!(
                target: "shell::state",
                asked = %room,
                got = %declared.map(|d| d.to_string()).unwrap_or_default(),
                "room sync refused: the payload names a different room than \
                 the one it answers",
            );
            true
        } else {
            false
        }
    }

    /// True (and logs a warning, or pins an unpinned authority) if
    /// `from` is not entitled to write the governance changes
    /// `candidate` carries relative to what's currently held for `room`.
    fn governance_write_refused(
        &mut self,
        room: RoomId,
        candidate: &AutoCommit,
        from: &PeerOrigin,
    ) -> bool {
        // Write authority on the room's governance. Compared by value,
        // not op id: a peer echoing governance it learned from us
        // re-states it under its own actor, which is not a change.
        let before = Self::governance_of(self.room_docs.get(&room));
        let after = Self::governance_of(Some(candidate));
        if before == after {
            return false;
        }
        match self.room_authority(room) {
            // We administer the room, so no peer is entitled to write
            // its governance.
            RoomAuthority::Local => {
                tracing::warn!(
                    target: "shell::state",
                    room = %room,
                    from = %from,
                    "room sync refused: a peer's changes would rewrite \
                     the governance of a room this machine administers",
                );
                true
            }
            // Someone else's room, and this is not them.
            RoomAuthority::Peer(pinned) if pinned != *from => {
                tracing::warn!(
                    target: "shell::state",
                    room = %room,
                    from = %from,
                    pinned = %pinned,
                    "room sync refused: governance may only come from \
                     the peer that administers the room",
                );
                true
            }
            RoomAuthority::Peer(_) => false,
            // Joined without an invite naming an administrator —
            // the first peer to supply governance becomes it, and
            // is pinned so the second is refused above.
            RoomAuthority::Unpinned => {
                tracing::info!(
                    target: "shell::state",
                    room = %room,
                    from = %from,
                    "room sync: pinning this peer as the room's \
                     governance authority (joined without an invite \
                     naming one)",
                );
                self.room_authority
                    .insert(room, RoomAuthority::Peer(from.clone()));
                false
            }
        }
    }

    /// True (and logs a warning) if `candidate` would rewrite windows
    /// this machine owns in `room` — compared only over entries both
    /// sides already hold, so a guest's own history syncing back after a
    /// restart (room documents aren't persisted) is never refused.
    fn owned_contents_conflict(
        &self,
        room: RoomId,
        candidate: &AutoCommit,
        from: &PeerOrigin,
    ) -> bool {
        // Write authority over this machine's own windows: entries this
        // machine minted are this machine's to write — same
        // "refuse the whole batch" reasoning as the governance check above.
        let local_salt = self.local_salt();
        let before = Self::owned_contents_digest(self.room_docs.get(&room), local_salt);
        let after = Self::owned_contents_digest(Some(candidate), local_salt);
        // Compared over the **intersection**: an entry of ours the batch
        // changes is refused; one it merely carries that we don't have
        // yet is not. Matters after a restart — room documents aren't
        // persisted, so comparing whole sets would refuse a guest's own
        // history back forever. Our next projection sweeps any gap.
        let before_by_id: std::collections::BTreeMap<u64, &String> =
            before.iter().map(|(id, v)| (*id, v)).collect();
        let changed_ours = after
            .iter()
            .filter(|(id, v)| before_by_id.get(id).is_some_and(|prev| *prev != v))
            .count();
        if changed_ours == 0 {
            return false;
        }
        tracing::warn!(
            target: "shell::state",
            room = %room,
            from = %from,
            entries = changed_ours,
            "room sync refused: a peer's changes would rewrite windows \
             this machine owns",
        );
        true
    }

    /// `(name, owner, capability overrides)` a candidate room document
    /// declares — the governed fields [`Self::apply_room_changes`] folds
    /// into the cached workspace once every refusal gate passes.
    fn extract_governed_fields(
        candidate: &AutoCommit,
    ) -> (Option<String>, Option<String>, Vec<CapOverride>) {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let name = match candidate.get(&ROOT, "name").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        };
        let owner = match candidate.get(&ROOT, "owner").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        };
        let mut caps: Vec<CapOverride> = Vec::new();
        if let Some((_, caps_id)) = candidate.get(&ROOT, "capabilities").ok().flatten() {
            let keys: Vec<String> = candidate.keys(&caps_id).collect();
            for key in keys {
                if let Some((Value::Scalar(s), _)) = candidate.get(&caps_id, &key).ok().flatten()
                    && let ScalarValue::Bytes(blob) = s.as_ref()
                    && let Ok(cap) = serde_json::from_slice::<CapOverride>(blob)
                {
                    caps.push(cap);
                }
            }
        }
        (name, owner, caps)
    }

    /// Diffs `candidate`'s room-record index against what's already held
    /// for `room`, queues the difference for the shell to fold in (see
    /// [`Self::take_inbound_room_records`]), and reports whether
    /// anything arrived.
    fn queue_inbound_records(&mut self, room: RoomId, candidate: &AutoCommit) -> bool {
        // The difference from what's already held is what a peer
        // brought — queued for the shell to fold in, since the tree
        // can't reach the room's own index.
        let arrived = Self::room_records_of(candidate);
        let already = self.room_records_held.get(&room);
        let mut inbound: Vec<(RoomId, String, String)> = Vec::new();
        for (id, kind) in arrived {
            let known = already.and_then(|h| h.get(&id));
            if known.map(std::string::String::as_str) != Some(kind.as_str()) {
                inbound.push((room, id, kind));
            }
        }
        let records_changed = !inbound.is_empty();
        self.room_records_inbound.extend(inbound);
        records_changed
    }

    /// Merge a peer's changes to `room` into the cached workspace, and
    /// report whether it changed.
    ///
    /// Refuses, before anything is stored: a room this machine isn't in;
    /// a payload whose `room` disagrees with the envelope; and a payload
    /// changing the room's governance unless `from` is entitled to write
    /// it ([`Self::room_authority`]) — automerge has no per-key ACL, so
    /// otherwise a guest's cache wins a coin toss over its host's
    /// ownership.
    ///
    /// Refusal is of the **whole batch**: a partial merge would advance
    /// local heads past rejected ops (the sender's next export then skips
    /// them, unrecoverably) while leaving those ops in the graph exported
    /// onward. This also blocks a stale lineage from resurrecting a
    /// capability revoked while that peer was away.
    ///
    /// # Errors
    ///
    /// Returns the underlying `automerge::AutomergeError` if any blob in
    /// `changes` fails to decode, or if applying them to the room's
    /// document fails.
    pub fn apply_room_changes(
        &mut self,
        room: RoomId,
        changes: &[Vec<u8>],
        from: &PeerOrigin,
    ) -> Result<bool, automerge::AutomergeError> {
        if self.workspace_in_room(room).is_none() {
            return Ok(false);
        }
        let parsed: Result<Vec<automerge::Change>, _> = changes
            .iter()
            .map(|b| automerge::Change::try_from(b.as_slice()))
            .collect();
        let parsed = parsed?;

        // Applied to a copy first: a check that runs after the merge has
        // already happened is not a check.
        let mut candidate = self
            .room_docs
            .get(&room)
            .cloned()
            .unwrap_or_else(AutoCommit::new);
        // See `sync_room_doc`: a patch log carried into `apply_changes`
        // is what turns a peer's first delta into a panic.
        candidate.reset_diff_cursor();
        candidate.apply_changes(parsed)?;

        if Self::declared_room_mismatches(&candidate, room) {
            return Ok(false);
        }
        if self.governance_write_refused(room, &candidate, from) {
            return Ok(false);
        }
        if self.owned_contents_conflict(room, &candidate, from) {
            return Ok(false);
        }

        let (name, owner, caps) = Self::extract_governed_fields(&candidate);
        let records_changed = self.queue_inbound_records(room, &candidate);

        self.room_docs.insert(room, candidate);

        // Folds the room's windows into this machine's copy, preserving
        // view state; see `merge_room_contents`.
        let contents_changed = self.merge_room_contents(room);

        let Some(ws) = self.workspaces.iter_mut().find(|w| w.room == room) else {
            return Ok(records_changed || contents_changed);
        };
        let mut fields_changed = false;
        if let Some(name) = name
            && ws.name != name
        {
            ws.name = name;
            fields_changed = true;
        }
        if let Some(owner) = owner
            && ws.owner.as_deref() != Some(owner.as_str())
        {
            ws.owner = Some(owner);
            fields_changed = true;
        }
        // Replaced rather than merged: a revoked capability is simply
        // absent from the peer's document, so carrying the local list
        // over would let it survive.
        let mut sorted = caps;
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        let mut current = ws.capabilities.clone();
        current.sort_by(|a, b| a.id.cmp(&b.id));
        if serde_json::to_vec(&current).ok() != serde_json::to_vec(&sorted).ok() {
            ws.capabilities = sorted;
            fields_changed = true;
        }

        let fields_changed = fields_changed || contents_changed;
        if fields_changed {
            // Mirror the merged state into the tree document and to disk.
            self.seed_doc_from_cache();
            self.flush_to_disk();
        }
        let landed = fields_changed || records_changed;
        if landed {
            // A peer's window is in the tree now but not on screen yet —
            // this runs on the network thread, and only the frame
            // thread splices the widget tree, on chrome revision bumps.
            self.room_epoch = self.room_epoch.wrapping_add(1);
        }
        Ok(landed)
    }

    /// How many times a peer's changes have landed in this tree.
    /// Monotonic per session, not persisted — read by the frame loop,
    /// which bumps the chrome revision when it moves.
    pub fn room_epoch(&self) -> u64 {
        self.room_epoch
    }

    /// Seed the automerge doc from the cache. Idempotent (truncates
    /// `workspaces` and re-fills it). Tests building literal trees
    /// should call this to flow doc-side changes out via
    /// [`Self::export_changes_since`]. `active_strip`/`active_group`
    /// stay cache-only.
    ///
    /// # Panics
    ///
    /// Never panics: every doc write is best-effort (the cache is the
    /// source of truth), so a write that fails just skips the nested
    /// entries it would have contained rather than aborting the seed.
    pub fn seed_doc_from_cache(&mut self) {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
        let _ = self
            .doc
            .put(&ROOT, "active", ScalarValue::Uint(self.active.raw()));
        // Truncate + re-fill from the cache; not diff-aware, so
        // concurrent appends can clobber.
        let workspaces_id = if let Some((_, id)) = self.doc.get(&ROOT, "workspaces").ok().flatten()
        {
            id
        } else {
            let Ok(id) = self.doc.put_object(&ROOT, "workspaces", ObjType::List) else {
                // No doc-side list to seed into; the cache stays the
                // source of truth either way.
                return;
            };
            id
        };
        let current_len = self.doc.length(&workspaces_id);
        for i in (0..current_len).rev() {
            let _ = self.doc.delete(&workspaces_id, i);
        }
        for (i, ws) in self.workspaces.iter().enumerate() {
            let Ok(entry) = self.doc.insert_object(&workspaces_id, i, ObjType::Map) else {
                continue;
            };
            let _ = self.doc.put(&entry, "id", ScalarValue::Uint(ws.id.raw()));
            let _ = self.doc.put(&entry, "name", ws.name.clone());
            // Room identity travels with the workspace: a peer hydrating
            // from this doc must land in *our* room, not mint its own —
            // the local `id` above is meaningless to it.
            let _ = self.doc.put(&entry, "room", ws.room.to_string());
            if let Some(owner) = &ws.owner {
                let _ = self.doc.put(&entry, "owner", owner.clone());
            }
            if let Ok(caps_id) = self.doc.put_object(&entry, "capabilities", ObjType::Map) {
                for cap in &ws.capabilities {
                    if let Ok(blob) = serde_json::to_vec(cap) {
                        let _ = self.doc.put(&caps_id, &cap.id, ScalarValue::Bytes(blob));
                    }
                }
            }
            // Nested strips + window_groups so per-group maximized +
            // width_permille writes have a doc-side path to land in.
            let Ok(strips_id) = self.doc.put_object(&entry, "strips", ObjType::List) else {
                continue;
            };
            for (j, strip) in ws.strips.iter().enumerate() {
                let Ok(strip_entry) = self.doc.insert_object(&strips_id, j, ObjType::Map) else {
                    continue;
                };
                let _ = self
                    .doc
                    .put(&strip_entry, "id", ScalarValue::Uint(strip.id.raw()));
                let Ok(groups_id) =
                    self.doc
                        .put_object(&strip_entry, "window_groups", ObjType::List)
                else {
                    continue;
                };
                for (k, group) in strip.window_groups.iter().enumerate() {
                    let Ok(group_entry) = self.doc.insert_object(&groups_id, k, ObjType::Map)
                    else {
                        continue;
                    };
                    let _ = self
                        .doc
                        .put(&group_entry, "id", ScalarValue::Uint(group.id));
                    let _ = self.doc.put(
                        &group_entry,
                        "maximized",
                        ScalarValue::Boolean(group.maximized),
                    );
                    let _ = self.doc.put(
                        &group_entry,
                        "width_permille",
                        ScalarValue::Uint(u64::from(group.width.permille())),
                    );
                    // `tile_json` is a JSON-blob shim for `TileContent`
                    // rather than a full per-variant CRDT structure.
                    let Ok(tabs_id) = self.doc.put_object(&group_entry, "tabs", ObjType::List)
                    else {
                        continue;
                    };
                    for (l, tab) in group.tabs.iter().enumerate() {
                        let Ok(tab_entry) = self.doc.insert_object(&tabs_id, l, ObjType::Map)
                        else {
                            continue;
                        };
                        let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab.id));
                        let _ = self.doc.put(&tab_entry, "title", tab.title.clone());
                        if let Ok(blob) = serde_json::to_vec(&tab.tile) {
                            let _ = self
                                .doc
                                .put(&tab_entry, "tile_json", ScalarValue::Bytes(blob));
                        }
                    }
                }
            }
        }
        self.commit_room_change();
    }

    /// Walk the automerge doc and refresh the public cache fields.
    /// `active_strip`/`active_group`/collaborators stay whatever the
    /// cache had. Each rebuilt workspace inherits `strips` from the
    /// existing cache entry sharing its id; one from the doc alone
    /// starts empty.
    /// The child object at `obj[prop]` (a map key or a list index), or
    /// `None` if absent/not an object.
    fn doc_obj(
        doc: &AutoCommit,
        obj: &automerge::ObjId,
        prop: impl Into<automerge::Prop>,
    ) -> Option<automerge::ObjId> {
        use automerge::ReadDoc;
        doc.get(obj, prop).ok().flatten().map(|(_, id)| id)
    }

    /// The `Uint` scalar at `obj[prop]`, or `None` if absent/a different type.
    fn doc_uint(
        doc: &AutoCommit,
        obj: &automerge::ObjId,
        prop: impl Into<automerge::Prop>,
    ) -> Option<u64> {
        use automerge::{ReadDoc, ScalarValue, Value};
        match doc.get(obj, prop).ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Uint(v) => Some(*v),
                _ => None,
            },
            _ => None,
        }
    }

    /// The `Str` scalar at `obj[prop]`, or `None` if absent/a different type.
    fn doc_str(
        doc: &AutoCommit,
        obj: &automerge::ObjId,
        prop: impl Into<automerge::Prop>,
    ) -> Option<String> {
        use automerge::{ReadDoc, ScalarValue, Value};
        match doc.get(obj, prop).ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The `Boolean` scalar at `obj[prop]`, or `None` if absent/a different type.
    fn doc_bool(
        doc: &AutoCommit,
        obj: &automerge::ObjId,
        prop: impl Into<automerge::Prop>,
    ) -> Option<bool> {
        use automerge::{ReadDoc, ScalarValue, Value};
        match doc.get(obj, prop).ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Boolean(v) => Some(*v),
                _ => None,
            },
            _ => None,
        }
    }

    /// The `Bytes` scalar at `obj[prop]`, or `None` if absent/a different type.
    fn doc_bytes(
        doc: &AutoCommit,
        obj: &automerge::ObjId,
        prop: impl Into<automerge::Prop>,
    ) -> Option<Vec<u8>> {
        use automerge::{ReadDoc, ScalarValue, Value};
        match doc.get(obj, prop).ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Bytes(b) => Some(b.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn rebuild_cache_from_doc(&mut self) {
        use automerge::{ROOT, ReadDoc};
        if let Some(raw) = Self::doc_uint(&self.doc, &ROOT, "active") {
            self.active = WorkspaceId::from(raw);
        }
        // Rebuild while preserving any existing nested state by id-match.
        if let Some(workspaces_id) = Self::doc_obj(&self.doc, &ROOT, "workspaces") {
            let len = self.doc.length(&workspaces_id);
            let mut new_workspaces: Vec<Workspace> = Vec::with_capacity(len);
            for i in 0..len {
                let Some(entry_id) = Self::doc_obj(&self.doc, &workspaces_id, i) else {
                    continue;
                };
                let Some(id_raw) = Self::doc_uint(&self.doc, &entry_id, "id") else {
                    continue;
                };
                let name = Self::doc_str(&self.doc, &entry_id, "name").unwrap_or_default();
                let ws_id = WorkspaceId::from(id_raw);
                // Room identity replicates too; absent or unparseable
                // (a pre-rooms doc) leaves whatever the cache already had.
                let replicated_room = Self::doc_str(&self.doc, &entry_id, "room")
                    .and_then(|v| RoomId::parse(v.as_str()))
                    .filter(|r| !r.is_nil());
                // Capabilities/ownership rebuild from the doc, not carried
                // from the cache — this is the half that makes a
                // *revocation* take effect.
                let replicated = self.doc_read_capabilities(&entry_id);
                let mut ws = if let Some(existing_idx) =
                    self.workspaces.iter().position(|w| w.id == ws_id)
                {
                    let mut existing = self.workspaces.remove(existing_idx);
                    existing.name = name;
                    existing
                } else {
                    Workspace::new(ws_id, name)
                };
                if let Some(room) = replicated_room {
                    ws.room = room;
                }
                if let Some((owner, caps)) = replicated {
                    if owner.is_some() {
                        ws.owner = owner;
                    }
                    ws.capabilities = caps;
                }
                new_workspaces.push(ws);
            }
            self.workspaces = new_workspaces;
        }

        // Reconstruct per-window-group `maximized`, `width_permille` and
        // `tabs`, matching each strip/group by id; a missing cache entry
        // gets a fresh `Strip`/`WindowGroup` synthesized.
        if let Some(workspaces_id) = Self::doc_obj(&self.doc, &ROOT, "workspaces") {
            self.rebuild_window_groups_from_doc(&workspaces_id);
        }
    }

    /// Second half of [`Self::rebuild_cache_from_doc`]: walks
    /// `workspaces_id`'s `strips/window_groups/tabs` and folds their doc
    /// state into the matching cache entries.
    fn rebuild_window_groups_from_doc(&mut self, workspaces_id: &automerge::ObjId) {
        use automerge::ReadDoc;
        let ws_len = self.doc.length(workspaces_id);
        for ws_idx in 0..ws_len {
            let Some(ws_entry) = Self::doc_obj(&self.doc, workspaces_id, ws_idx) else {
                continue;
            };
            let Some(ws_id_raw) = Self::doc_uint(&self.doc, &ws_entry, "id") else {
                continue;
            };
            let ws_id = WorkspaceId::from(ws_id_raw);
            let Some(strip_list_id) = Self::doc_obj(&self.doc, &ws_entry, "strips") else {
                continue;
            };
            let strips_len = self.doc.length(&strip_list_id);
            for strip_pos in 0..strips_len {
                let Some(strip_entry) = Self::doc_obj(&self.doc, &strip_list_id, strip_pos) else {
                    continue;
                };
                let Some(strip_id_raw) = Self::doc_uint(&self.doc, &strip_entry, "id") else {
                    continue;
                };
                let strip_id = StripId::from(strip_id_raw);
                let Some(group_list_id) = Self::doc_obj(&self.doc, &strip_entry, "window_groups")
                else {
                    continue;
                };
                let groups_len = self.doc.length(&group_list_id);
                for group_pos in 0..groups_len {
                    let Some(group_entry) = Self::doc_obj(&self.doc, &group_list_id, group_pos)
                    else {
                        continue;
                    };
                    let Some(group_id) = Self::doc_uint(&self.doc, &group_entry, "id") else {
                        continue;
                    };
                    let maximized =
                        Self::doc_bool(&self.doc, &group_entry, "maximized").unwrap_or(false);
                    // Peers publish a *fraction*, so width means the
                    // same share of this replica's own display, not
                    // the sender's pixel count. `width_pixels` is the
                    // pre-fraction shape — migrate rather than drop it.
                    let width = Self::doc_uint(&self.doc, &group_entry, "width_permille")
                        .filter(|v| *v > 0)
                        .and_then(|v| u16::try_from(v).ok())
                        .map(WindowWidth::Fraction)
                        .or_else(|| {
                            Self::doc_uint(&self.doc, &group_entry, "width_pixels")
                                .filter(|v| *v > 0)
                                .and_then(|v| u32::try_from(v).ok())
                                .map(WindowWidth::from_legacy_pixels)
                        });

                    let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == ws_id) else {
                        continue;
                    };
                    let strip = if let Some(s) = ws.strips.iter_mut().find(|s| s.id == strip_id) {
                        s
                    } else {
                        ws.strips.push(Strip::new(strip_id, "Main"));
                        ws.strips.last_mut().expect("strip just pushed")
                    };
                    // Tabs missing from the doc fall back to the
                    // cache; new ids get synthesized entries.
                    // Explicit `&self.doc` (not a `&self` method): `strip`
                    // above already holds a mutable borrow into
                    // `self.workspaces`, so this can only borrow the `doc`
                    // field, not all of `self`.
                    let doc_tabs = Self::collect_doc_tabs(&self.doc, &group_entry);
                    Self::apply_group_from_doc(strip, group_id, maximized, width, &doc_tabs);
                }
            }
        }
    }

    /// Tabs recorded under `group_entry` in the doc, as `(id, title,
    /// tile_json blob)` — the raw material [`Self::apply_group_from_doc`]
    /// folds into a cached [`WindowGroup`]'s `tabs`.
    fn collect_doc_tabs(
        doc: &AutoCommit,
        group_entry: &automerge::ObjId,
    ) -> Vec<(u64, String, Vec<u8>)> {
        use automerge::ReadDoc;
        let mut doc_tabs = Vec::new();
        let Some(tabs_id) = Self::doc_obj(doc, group_entry, "tabs") else {
            return doc_tabs;
        };
        let tabs_len = doc.length(&tabs_id);
        for t_idx in 0..tabs_len {
            let Some(tab_entry) = Self::doc_obj(doc, &tabs_id, t_idx) else {
                continue;
            };
            let Some(id) = Self::doc_uint(doc, &tab_entry, "id") else {
                continue;
            };
            let title = Self::doc_str(doc, &tab_entry, "title").unwrap_or_default();
            let tile_blob = Self::doc_bytes(doc, &tab_entry, "tile_json").unwrap_or_default();
            doc_tabs.push((id, title, tile_blob));
        }
        doc_tabs
    }

    /// Fold a doc-side group's `maximized`/`width`/`doc_tabs` into
    /// `strip`'s cached window group `group_id`, synthesizing a fresh
    /// placeholder group (carrying the doc's tabs) if the cache doesn't
    /// have one yet.
    fn apply_group_from_doc(
        strip: &mut Strip,
        group_id: u64,
        maximized: bool,
        width: Option<WindowWidth>,
        doc_tabs: &[(u64, String, Vec<u8>)],
    ) {
        let Some(g) = strip.window_groups.iter_mut().find(|g| g.id == group_id) else {
            // A placeholder group, carrying doc tabs.
            let synthesized_tabs: Vec<Tab> = doc_tabs
                .iter()
                .map(|(tid, title, blob)| Tab {
                    id: *tid,
                    title: title.clone(),
                    tile: Self::tile_from_blob(blob, title),
                })
                .collect();
            let active_tab = synthesized_tabs.first().map_or(0, |t| t.id);
            strip.window_groups.push(WindowGroup {
                id: group_id,
                tabs: synthesized_tabs,
                active_tab,
                width: width.unwrap_or(WindowWidth::Normal),
                maximized,
            });
            return;
        };
        g.maximized = maximized;
        // Always `Fraction`: mapping back onto Narrow/Normal/Wide would
        // silently coerce a peer's manual resize to the closest preset.
        if let Some(w) = width {
            g.width = w;
        }
        if doc_tabs.is_empty() {
            return;
        }
        let mut new_tabs: Vec<Tab> = Vec::with_capacity(doc_tabs.len());
        for (tid, title, blob) in doc_tabs {
            if let Some(existing) = g.tabs.iter().find(|t| t.id == *tid) {
                new_tabs.push(existing.clone());
            } else {
                new_tabs.push(Tab {
                    id: *tid,
                    title: title.clone(),
                    tile: Self::tile_from_blob(blob, title),
                });
            }
        }
        if g.tabs
            .iter()
            .all(|t| !doc_tabs.iter().any(|(id, _, _)| *id == t.id))
            || g.tabs.is_empty()
        {
            g.active_tab = new_tabs.first().map_or(0, |t| t.id);
        }
        g.tabs = new_tabs;
    }

    /// A `TileContent` from a `tile_json` blob (empty when unset), or a
    /// bare `Stub` carrying just the title if the blob fails to decode.
    fn tile_from_blob(blob: &[u8], title: &str) -> TileContent {
        if blob.is_empty() {
            TileContent::Stub {
                kind: AppKind::Other(String::new()),
                label: title.to_string(),
            }
        } else {
            serde_json::from_slice::<TileContent>(blob).unwrap_or_else(|_| TileContent::Stub {
                kind: AppKind::Other(String::new()),
                label: title.to_string(),
            })
        }
    }

    /// Walk the doc to the [`WindowGroup`] map for `group_id`. `None`
    /// if not yet seeded ([`Self::seed_doc_from_cache`]) — callers
    /// tolerate this by skipping the doc-side write.
    fn locate_group(&self, group_id: u64) -> Option<(automerge::ObjId, usize, usize, usize)> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let (_, workspaces_id) = self.doc.get(&ROOT, "workspaces").ok().flatten()?;
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let (_, ws_entry) = self.doc.get(&workspaces_id, ws_idx).ok().flatten()?;
            let Some(strips_id) = Self::doc_obj(&self.doc, &ws_entry, "strips") else {
                continue;
            };
            let strips_len = self.doc.length(&strips_id);
            for strip_idx in 0..strips_len {
                let (_, strip_entry) = self.doc.get(&strips_id, strip_idx).ok().flatten()?;
                let Some(groups_map_id) = Self::doc_obj(&self.doc, &strip_entry, "window_groups")
                else {
                    continue;
                };
                let groups_len = self.doc.length(&groups_map_id);
                for group_idx in 0..groups_len {
                    let (_, group_entry) =
                        self.doc.get(&groups_map_id, group_idx).ok().flatten()?;
                    if let Some((Value::Scalar(s), _)) =
                        self.doc.get(&group_entry, "id").ok().flatten()
                        && let ScalarValue::Uint(id) = s.as_ref()
                        && *id == group_id
                    {
                        return Some((group_entry, ws_idx, strip_idx, group_idx));
                    }
                }
            }
        }
        None
    }

    /// Mirror a `WindowGroup.maximized` flip into the doc so peer-sync
    /// observes it. No-op if not yet seeded — see [`Self::locate_group`].
    pub fn doc_set_maximized(&mut self, group_id: u64, new_value: bool) {
        use automerge::{ScalarValue, transaction::Transactable};
        if let Some((entry, _, _, _)) = self.locate_group(group_id) {
            let _ = self
                .doc
                .put(&entry, "maximized", ScalarValue::Boolean(new_value));
        }
    }

    /// Mirror a `WindowGroup.width` resize into the doc as a fraction
    /// — a pixel count would reproduce the sender's monitor instead of
    /// the receiver's own share. Same null-tolerance as
    /// [`Self::doc_set_maximized`].
    pub fn doc_set_width(&mut self, group_id: u64, new_permille: u16) {
        use automerge::{ScalarValue, transaction::Transactable};
        if let Some((entry, _, _, _)) = self.locate_group(group_id) {
            let _ = self.doc.put(
                &entry,
                "width_permille",
                ScalarValue::Uint(u64::from(new_permille)),
            );
        }
    }

    /// Walk the doc to the workspace map for `workspace_id`. Same
    /// null-tolerance as [`Self::locate_group`].
    fn locate_workspace(&self, workspace_id: WorkspaceId) -> Option<automerge::ObjId> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let (_, workspaces_map_id) = self.doc.get(&ROOT, "workspaces").ok().flatten()?;
        let len = self.doc.length(&workspaces_map_id);
        for i in 0..len {
            let (_, ws_entry) = self.doc.get(&workspaces_map_id, i).ok().flatten()?;
            if let Some((Value::Scalar(s), _)) = self.doc.get(&ws_entry, "id").ok().flatten()
                && let ScalarValue::Uint(raw) = s.as_ref()
                && *raw == workspace_id.raw()
            {
                return Some(ws_entry);
            }
        }
        None
    }

    /// Write the tree to [`Self::persist_path`], if one is set. Called on
    /// every capability mutation, synchronous and eager rather than
    /// batched on shutdown — a compositor that's killed must not forget
    /// which permissions the user granted.
    pub fn flush_to_disk(&self) {
        if let Some(path) = &self.persist_path
            && let Err(e) = crate::shell::persist::save_tree_to(self, path)
        {
            tracing::warn!("capability persistence failed: {e}");
        }
        self.export_fs_capabilities();
    }

    /// Write the capability list the filesystem enforces. The FUSE mount
    /// is a separate process reading a JSON array from the path its
    /// `--acl` names, written atomically so it never reads a half-written
    /// list and concludes the user has no permissions. All workspaces
    /// export together since `/var/workspace` is one mount, so two
    /// workspaces granting the same path to different subjects union.
    ///
    /// The filesystem authenticates the machine's device key. For a local
    /// identity's path grants, export an equivalent device-key binding only
    /// after the registry and root-signed device certificate establish the
    /// `identity -> device` relationship. Restrictions are duplicated too,
    /// so translation can never widen a grant.
    pub fn export_fs_capabilities(&self) {
        let Some(path) = &self.fs_acl_path else {
            return;
        };
        let identity_paths = crate::firstboot::collaboration_identity_paths();
        let device_public_key =
            qos_user_identity::persist::read_device_key(identity_paths.device_privkey.as_path())
                .ok()
                .map(|key| key.verifying_key().to_bytes());
        let mut caps: Vec<CapOverride> = Vec::new();
        for workspace in &self.workspaces {
            let profile =
                crate::identity_registry::selected_identity_profile(&workspace.room.to_string())
                    .ok()
                    .flatten()
                    .filter(|profile| {
                        let Some(device) = device_public_key else {
                            return false;
                        };
                        let paths = crate::firstboot::collaboration_identity_paths();
                        qos_user_identity::persist::read_device_cert(
                            &paths.device_cert_for_user(&profile.user_public_key),
                        )
                        .ok()
                        .filter(|cert| qos_user_identity::verify_device_cert(cert).is_ok())
                        .is_some_and(|cert| {
                            cert.user_public_key == profile.user_public_key
                                && cert.device_public_key == device
                                && !crate::device_fleet::DeviceFleetStore::from_env()
                                    .and_then(|store| store.load(&profile.id))
                                    .is_ok_and(|fleet| fleet.is_revoked(&device))
                        })
                    });
            let identity_actor = profile.as_ref().map(|profile| Actor {
                role: None,
                key: Some(collab_types::crypto::Ed25519PublicKey::from_bytes(
                    profile.user_public_key,
                )),
                principals: vec![Principal::person_from_key(
                    collab_types::crypto::Ed25519PublicKey::from_bytes(profile.user_public_key),
                )],
            });
            for capability in workspace
                .capabilities
                .iter()
                .filter(|capability| matches!(capability.scope, Scope::FsPath(_)))
            {
                caps.push(capability.clone());
                if let (Some(device), Some(actor)) = (device_public_key, identity_actor.as_ref())
                    && capability.subject.matches_actor(actor)
                {
                    let mut device_capability = capability.clone();
                    device_capability.subject =
                        Subject::User(collab_types::crypto::Ed25519PublicKey::from_bytes(device));
                    caps.push(device_capability);
                }
            }
        }
        let Ok(json) = serde_json::to_vec_pretty(&caps) else {
            return;
        };
        if let Err(e) = atomic_file::write_creating_parent(path, &json) {
            tracing::warn!("filesystem capability export failed: {e}");
        }
    }

    /// Give every workspace with no recorded owner to `user`, returning
    /// the ids adopted. Treating unowned as "owned by anyone" would leave
    /// every grant decision wide open, so this is the closed answer.
    ///
    /// A workspace in a room this machine does not govern is **not**
    /// adopted however ownerless it looks — it gave its governance up by
    /// joining someone else's room, and adopting would hand that room
    /// back to this machine on the next boot.
    pub fn adopt_ownerless_workspaces(&mut self, user: &UserId) -> Vec<WorkspaceId> {
        self.adopt_ownerless_workspaces_with(|_| user.clone())
    }

    /// Give each ownerless locally-governed workspace to the identity selected
    /// for its room. This is the multi-identity form of
    /// [`Self::adopt_ownerless_workspaces`].
    pub fn adopt_ownerless_workspaces_with(
        &mut self,
        identity_for_room: impl Fn(&RoomId) -> UserId,
    ) -> Vec<WorkspaceId> {
        let governed_elsewhere: Vec<RoomId> = self
            .room_authority
            .iter()
            .filter(|(_, a)| **a != RoomAuthority::Local)
            .map(|(room, _)| *room)
            .collect();
        let mut adopted = Vec::new();
        for ws in &mut self.workspaces {
            if ws.owner.is_none() && !governed_elsewhere.contains(&ws.room) {
                let user = identity_for_room(&ws.room);
                ws.set_owner(user.clone());
                adopted.push((ws.id, user));
            }
        }
        for (id, user) in &adopted {
            self.doc_set_owner(*id, user);
            let caps: Vec<CapOverride> = self
                .workspaces
                .iter()
                .find(|w| w.id == *id)
                .map(|w| w.capabilities.clone())
                .unwrap_or_default();
            for cap in caps {
                self.doc_insert_capability(*id, &cap);
            }
        }
        if !adopted.is_empty() {
            self.flush_to_disk();
        }
        adopted.into_iter().map(|(id, _)| id).collect()
    }

    /// Mint a durable [`RoomId`] for every workspace that has none, and
    /// write it down — minting without persisting would be worse than
    /// leaving it nil, since the next boot would mint a different one.
    /// The caller must have set [`Self::persist_path`] first. Returns
    /// the ids minted for.
    pub fn adopt_roomless_workspaces(&mut self) -> Vec<WorkspaceId> {
        let mut adopted = Vec::new();
        for ws in &mut self.workspaces {
            if ws.room.is_nil() {
                ws.room = RoomId::new();
                adopted.push(ws.id);
            }
        }
        for id in &adopted {
            let room = self
                .workspaces
                .iter()
                .find(|w| w.id == *id)
                .map(|w| w.room)
                .unwrap_or_default();
            self.doc_set_room(*id, room);
        }
        if !adopted.is_empty() {
            self.flush_to_disk();
        }
        adopted
    }

    /// The durable room identity of `workspace_id`. What the launch path
    /// needs to turn a local handle into the value the wire speaks.
    pub fn room_of(&self, workspace_id: WorkspaceId) -> Option<RoomId> {
        self.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .map(|w| w.room)
    }

    /// Point `workspace_id` at `room`, mirroring the change to peers and
    /// disk — how an invitee stops being in their own room and starts
    /// being in the inviter's. Refuses a nil `room`. Also where a
    /// workspace stops governing itself
    /// ([`Workspace::surrender_governance`]), since the joined room
    /// already has an administrator.
    pub fn set_room(&mut self, workspace_id: WorkspaceId, room: RoomId) -> bool {
        if room.is_nil() {
            return false;
        }
        // Joining a room some other local workspace is already in is not a
        // join: this machine is already in the room and already knows who
        // governs it.
        let joining = !self.workspaces.iter().any(|w| w.room == room);
        let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) else {
            return false;
        };
        if ws.room == room {
            return false;
        }
        ws.room = room;
        let surrendered = if joining {
            ws.surrender_governance()
        } else {
            false
        };
        if joining {
            self.room_authority
                .entry(room)
                .or_insert(RoomAuthority::Unpinned);
            // See `Self::room_share_floor`: joining is entering someone
            // else's workspace, not merging yours into it.
            self.room_share_floor
                .insert(room, NEXT_ID.load(AtomicOrdering::Relaxed));
        }
        self.doc_set_room(workspace_id, room);
        if surrendered {
            // The owner and admin capability went; the tree document has
            // to say so too, or the next cache rebuild reads the old
            // owner back out of it.
            self.seed_doc_from_cache();
        }
        self.flush_to_disk();
        true
    }

    /// Mirror a workspace's room into the automerge doc, as a string —
    /// what every other consumer of the value speaks
    /// (`QOS_WORKSPACE_ID`, the invite token, `CrdtRoute::cluster_id`).
    /// Null-tolerant like the other `doc_*` mirrors.
    pub fn doc_set_room(&mut self, workspace_id: WorkspaceId, room: RoomId) {
        use automerge::transaction::Transactable;
        let Some(ws_entry) = self.locate_workspace(workspace_id) else {
            return;
        };
        let _ = self.doc.put(&ws_entry, "room", room.to_string());
        self.commit_room_change();
    }

    /// Grant a capability in `workspace_id` on behalf of `grantor`,
    /// mirroring it to peers and disk — the single production entry
    /// point for handing out authority. `None` when the grantor lacked
    /// authority to delegate it.
    ///
    /// Refuses outright in a room whose governance is written elsewhere,
    /// whatever the local cache says: a grant nobody else can see is a
    /// local escalation, honoured by this sandbox until the room's
    /// authority next syncs it away.
    ///
    /// Not symmetric with [`Self::revoke_in_workspace`], which stays
    /// open — narrowing your own access has to work locally whoever
    /// governs the room.
    pub fn grant_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        grantor: &UserId,
        cap: CapOverride,
    ) -> Option<String> {
        let room = self.room_of(workspace_id)?;
        if !room.is_nil() && !self.governs_room(room) {
            tracing::warn!(
                target: "shell::state",
                room = %room,
                grantor = %grantor,
                "grant refused: this machine does not administer the room",
            );
            return None;
        }
        let ws = self.workspaces.iter_mut().find(|w| w.id == workspace_id)?;
        let id = ws.grant_as(grantor, cap)?;
        let stored = ws.capabilities.iter().find(|c| c.id == id).cloned()?;
        self.doc_insert_capability(workspace_id, &stored);
        self.flush_to_disk();
        Some(id)
    }

    /// Revoke a capability in `workspace_id`, mirroring the removal to
    /// peers and disk — a revoke that only touched the local cache left
    /// every peer still holding the capability and let a restart bring
    /// it back.
    pub fn revoke_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        revoker: &UserId,
        cap_id: &str,
    ) -> bool {
        let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) else {
            return false;
        };
        if !ws.revoke_as(revoker, cap_id) {
            return false;
        }
        self.doc_revoke_capability(workspace_id, cap_id);
        self.flush_to_disk();
        true
    }

    /// Mirror a capability grant into the automerge doc so peer-sync
    /// ships it and a restart reloads it. Capabilities live as a **map
    /// keyed by capability id**, not a list — deleting a map key
    /// merges concurrently, while deleting list index 3 on two peers
    /// removes two different capabilities.
    pub fn doc_insert_capability(&mut self, workspace_id: WorkspaceId, cap: &CapOverride) {
        use automerge::{ObjType, ReadDoc, ScalarValue, transaction::Transactable};
        let Some(ws_entry) = self.locate_workspace(workspace_id) else {
            return;
        };
        let caps_id = match self.doc.get(&ws_entry, "capabilities").ok().flatten() {
            Some((_, id)) => id,
            None => match self.doc.put_object(&ws_entry, "capabilities", ObjType::Map) {
                Ok(id) => id,
                Err(_) => return,
            },
        };
        let Ok(blob) = serde_json::to_vec(cap) else {
            return;
        };
        let _ = self.doc.put(&caps_id, &cap.id, ScalarValue::Bytes(blob));
        self.commit_room_change();
    }

    /// Mirror a revocation into the automerge doc — without this a
    /// revoke would mutate only the local cache, so a peer would keep
    /// using a capability the owner took away.
    pub fn doc_revoke_capability(&mut self, workspace_id: WorkspaceId, cap_id: &str) {
        use automerge::{ReadDoc, transaction::Transactable};
        let Some(ws_entry) = self.locate_workspace(workspace_id) else {
            return;
        };
        let Some((_, caps_obj_id)) = self.doc.get(&ws_entry, "capabilities").ok().flatten() else {
            return;
        };
        let _ = self.doc.delete(&caps_obj_id, cap_id);
        self.commit_room_change();
    }

    /// Mirror the room's owner into the doc.
    pub fn doc_set_owner(&mut self, workspace_id: WorkspaceId, owner: &str) {
        use automerge::transaction::Transactable;
        let Some(ws_entry) = self.locate_workspace(workspace_id) else {
            return;
        };
        let _ = self.doc.put(&ws_entry, "owner", owner);
        self.commit_room_change();
    }

    /// Read a workspace's capabilities and owner back out of the doc.
    /// Capabilities absent from the doc are absent from the cache — what
    /// makes a peer's revocation take effect locally.
    fn doc_read_capabilities(
        &self,
        ws_entry: &automerge::ObjId,
    ) -> Option<(Option<UserId>, Vec<CapOverride>)> {
        use automerge::{ReadDoc, ScalarValue, Value};
        let owner = match self.doc.get(ws_entry, "owner").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        };
        let (_, caps_id) = self.doc.get(ws_entry, "capabilities").ok().flatten()?;
        let mut caps = Vec::new();
        for key in self.doc.keys(&caps_id) {
            if let Some((Value::Scalar(s), _)) = self.doc.get(&caps_id, &key).ok().flatten()
                && let ScalarValue::Bytes(blob) = s.as_ref()
                && let Ok(cap) = serde_json::from_slice::<CapOverride>(blob)
            {
                caps.push(cap);
            }
        }
        Some((owner, caps))
    }

    /// Walk the doc to the `Tab` map for `(strip_id, tab_id)`. Strip id
    /// disambiguates if a tab id ever collided across strips. `None` for
    /// the same reasons as [`Self::locate_group`].
    fn locate_tab(
        &self,
        strip_id: StripId,
        tab_id: u64,
    ) -> Option<(automerge::ObjId, automerge::ObjId, automerge::ObjId, usize)> {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let (_, workspaces_id) = self.doc.get(&ROOT, "workspaces").ok().flatten()?;
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let (_, ws_entry) = self.doc.get(&workspaces_id, ws_idx).ok().flatten()?;
            let Some((_, strips_map_id)) = self.doc.get(&ws_entry, "strips").ok().flatten() else {
                continue;
            };
            let strips_len = self.doc.length(&strips_map_id);
            for s_idx in 0..strips_len {
                let (_, strip_entry) = self.doc.get(&strips_map_id, s_idx).ok().flatten()?;
                let strip_id_matches = match self.doc.get(&strip_entry, "id").ok().flatten() {
                    Some((Value::Scalar(s), _)) => match s.as_ref() {
                        ScalarValue::Uint(id) => *id == strip_id.raw(),
                        _ => false,
                    },
                    _ => false,
                };
                if !strip_id_matches {
                    continue;
                }
                let Some((_, groups_id)) =
                    self.doc.get(&strip_entry, "window_groups").ok().flatten()
                else {
                    continue;
                };
                let groups_len = self.doc.length(&groups_id);
                for g_idx in 0..groups_len {
                    let (_, group_entry) = self.doc.get(&groups_id, g_idx).ok().flatten()?;
                    let Some((_, tabs_map_id)) = self.doc.get(&group_entry, "tabs").ok().flatten()
                    else {
                        continue;
                    };
                    let tabs_len = self.doc.length(&tabs_map_id);
                    for t_idx in 0..tabs_len {
                        let (_, tab_entry) = self.doc.get(&tabs_map_id, t_idx).ok().flatten()?;
                        if let Some((Value::Scalar(s), _)) =
                            self.doc.get(&tab_entry, "id").ok().flatten()
                            && let ScalarValue::Uint(id) = s.as_ref()
                            && *id == tab_id
                        {
                            return Some((group_entry, tabs_map_id, tab_entry, t_idx));
                        }
                    }
                }
            }
        }
        None
    }

    /// Mirror a `move_tab_across_strips` mutation into the doc: captures
    /// the tab's scalars from the source, deletes it there, and appends a
    /// fresh map to the destination strip's first window-group's `tabs`.
    /// No-op if the source tab or destination strip aren't seeded.
    pub fn doc_move_tab_across_strips(
        &mut self,
        src_strip: StripId,
        dst_strip: StripId,
        tab_id: u64,
    ) -> bool {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        let Some((_, src_tabs, src_tab_entry, src_idx)) = self.locate_tab(src_strip, tab_id) else {
            return false;
        };
        let title = match self.doc.get(&src_tab_entry, "title").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => v.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        let tile_blob: Vec<u8> = match self.doc.get(&src_tab_entry, "tile_json").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Bytes(b) => b.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };

        let _ = self.doc.delete(&src_tabs, src_idx);

        let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() else {
            return false;
        };
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) = self.doc.get(&workspaces_id, ws_idx).ok().flatten() else {
                continue;
            };
            let Some((_, strips_id)) = self.doc.get(&ws_entry, "strips").ok().flatten() else {
                continue;
            };
            let strips_len = self.doc.length(&strips_id);
            for s_idx in 0..strips_len {
                let Some((_, strip_entry)) = self.doc.get(&strips_id, s_idx).ok().flatten() else {
                    continue;
                };
                let dst_matches = matches!(self.doc.get(&strip_entry, "id").ok().flatten(),
                        Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == dst_strip.raw()));
                if !dst_matches {
                    continue;
                }
                let Some((_, groups_id)) =
                    self.doc.get(&strip_entry, "window_groups").ok().flatten()
                else {
                    return false;
                };
                if self.doc.length(&groups_id) == 0 {
                    return false;
                }
                let Some((_, first_group)) = self.doc.get(&groups_id, 0_usize).ok().flatten()
                else {
                    return false;
                };
                let tabs_map_id = match self.doc.get(&first_group, "tabs").ok().flatten() {
                    Some((_, id)) => id,
                    None => match self.doc.put_object(&first_group, "tabs", ObjType::List) {
                        Ok(id) => id,
                        Err(_) => return false,
                    },
                };
                let new_idx = self.doc.length(&tabs_map_id);
                let Ok(tab_entry) = self.doc.insert_object(&tabs_map_id, new_idx, ObjType::Map)
                else {
                    return false;
                };
                let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab_id));
                let _ = self.doc.put(&tab_entry, "title", title);
                if !tile_blob.is_empty() {
                    let _ = self
                        .doc
                        .put(&tab_entry, "tile_json", ScalarValue::Bytes(tile_blob));
                }
                return true;
            }
        }
        false
    }

    /// Mirror a [`Self::reorder_group_within_strip`] cache move into the
    /// doc: delete the entry, re-insert a fresh map at `new_index`,
    /// rebuilt from the cache [`WindowGroup`] (not read back out of the
    /// doc, so it can't drift). No-op on an unseeded doc.
    pub fn doc_reorder_group_within_strip(
        &mut self,
        strip_id: StripId,
        group: &WindowGroup,
        new_index: usize,
    ) {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() else {
            return;
        };
        let ws_len = self.doc.length(&workspaces_id);
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) = self.doc.get(&workspaces_id, ws_idx).ok().flatten() else {
                continue;
            };
            let Some((_, strips_map_id)) = self.doc.get(&ws_entry, "strips").ok().flatten() else {
                continue;
            };
            let strips_len = self.doc.length(&strips_map_id);
            for s_idx in 0..strips_len {
                let Some((_, strip_entry)) = self.doc.get(&strips_map_id, s_idx).ok().flatten()
                else {
                    continue;
                };
                let matches = matches!(
                    self.doc.get(&strip_entry, "id").ok().flatten(),
                    Some((Value::Scalar(s), _))
                        if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == strip_id.raw())
                );
                if !matches {
                    continue;
                }
                let Some((_, groups_id)) =
                    self.doc.get(&strip_entry, "window_groups").ok().flatten()
                else {
                    return;
                };
                let glen = self.doc.length(&groups_id);
                let mut cur: Option<usize> = None;
                for gi in 0..glen {
                    if let Some((_, g_entry)) = self.doc.get(&groups_id, gi).ok().flatten()
                        && matches!(
                            self.doc.get(&g_entry, "id").ok().flatten(),
                            Some((Value::Scalar(s), _))
                                if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == group.id)
                        )
                    {
                        cur = Some(gi);
                        break;
                    }
                }
                let Some(cur) = cur else { return };
                let _ = self.doc.delete(&groups_id, cur);
                let dest = new_index.min(self.doc.length(&groups_id));
                let Ok(group_entry) = self.doc.insert_object(&groups_id, dest, ObjType::Map) else {
                    return;
                };
                let _ = self
                    .doc
                    .put(&group_entry, "id", ScalarValue::Uint(group.id));
                let _ = self.doc.put(
                    &group_entry,
                    "maximized",
                    ScalarValue::Boolean(group.maximized),
                );
                let _ = self.doc.put(
                    &group_entry,
                    "width_permille",
                    ScalarValue::Uint(u64::from(group.width.permille())),
                );
                if let Ok(tabs_id) = self.doc.put_object(&group_entry, "tabs", ObjType::List) {
                    for (l, tab) in group.tabs.iter().enumerate() {
                        if let Ok(tab_entry) = self.doc.insert_object(&tabs_id, l, ObjType::Map) {
                            let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab.id));
                            let _ = self.doc.put(&tab_entry, "title", tab.title.clone());
                            if let Ok(blob) = serde_json::to_vec(&tab.tile) {
                                let _ =
                                    self.doc
                                        .put(&tab_entry, "tile_json", ScalarValue::Bytes(blob));
                            }
                        }
                    }
                }
                return;
            }
        }
    }

    /// Mirror an `apply_drop`-style insert: append a tab map to the
    /// active workspace's first strip's first window-group's `tabs`,
    /// matching where `apply_drop` targets when no group is named.
    /// No-op if the doc isn't seeded for the active workspace.
    pub fn doc_append_tab_to_first_group_of_active_workspace(
        &mut self,
        tab_id: u64,
        title: String,
        tile_blob: Vec<u8>,
    ) -> bool {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() else {
            return false;
        };
        let ws_len = self.doc.length(&workspaces_id);
        let active_raw = self.active.raw();
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) = self.doc.get(&workspaces_id, ws_idx).ok().flatten() else {
                continue;
            };
            let active_matches = matches!(self.doc.get(&ws_entry, "id").ok().flatten(),
                    Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == active_raw));
            if !active_matches {
                continue;
            }
            let Some((_, strips_id)) = self.doc.get(&ws_entry, "strips").ok().flatten() else {
                return false;
            };
            if self.doc.length(&strips_id) == 0 {
                return false;
            }
            let Some((_, strip_entry)) = self.doc.get(&strips_id, 0_usize).ok().flatten() else {
                return false;
            };
            let Some((_, groups_id)) = self.doc.get(&strip_entry, "window_groups").ok().flatten()
            else {
                return false;
            };
            if self.doc.length(&groups_id) == 0 {
                return false;
            }
            let Some((_, group_entry)) = self.doc.get(&groups_id, 0_usize).ok().flatten() else {
                return false;
            };
            let tabs_map_id = match self.doc.get(&group_entry, "tabs").ok().flatten() {
                Some((_, id)) => id,
                None => match self.doc.put_object(&group_entry, "tabs", ObjType::List) {
                    Ok(id) => id,
                    Err(_) => return false,
                },
            };
            let new_idx = self.doc.length(&tabs_map_id);
            let Ok(tab_entry) = self.doc.insert_object(&tabs_map_id, new_idx, ObjType::Map) else {
                return false;
            };
            let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab_id));
            let _ = self.doc.put(&tab_entry, "title", title);
            if !tile_blob.is_empty() {
                let _ = self
                    .doc
                    .put(&tab_entry, "tile_json", ScalarValue::Bytes(tile_blob));
            }
            return true;
        }
        false
    }

    /// Cursor-targeted counterpart of
    /// [`Self::doc_append_tab_to_first_group_of_active_workspace`], for
    /// a drop onto a non-first group: scans every strip for `group_id`.
    /// `false` means cache-only; peer-sync backfills on the next snapshot.
    pub fn doc_append_tab_to_group_of_active_workspace(
        &mut self,
        group_id: u64,
        tab_id: u64,
        title: String,
        tile_blob: Vec<u8>,
    ) -> bool {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
        let Some((_, workspaces_id)) = self.doc.get(&ROOT, "workspaces").ok().flatten() else {
            return false;
        };
        let ws_len = self.doc.length(&workspaces_id);
        let active_raw = self.active.raw();
        for ws_idx in 0..ws_len {
            let Some((_, ws_entry)) = self.doc.get(&workspaces_id, ws_idx).ok().flatten() else {
                continue;
            };
            let active_matches = matches!(
                self.doc.get(&ws_entry, "id").ok().flatten(),
                Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == active_raw));
            if !active_matches {
                continue;
            }
            let Some((_, strips_id)) = self.doc.get(&ws_entry, "strips").ok().flatten() else {
                return false;
            };
            let strips_len = self.doc.length(&strips_id);
            for strip_idx in 0..strips_len {
                let Some((_, strip_entry)) = self.doc.get(&strips_id, strip_idx).ok().flatten()
                else {
                    continue;
                };
                let Some((_, groups_map_id)) =
                    self.doc.get(&strip_entry, "window_groups").ok().flatten()
                else {
                    continue;
                };
                let groups_len = self.doc.length(&groups_map_id);
                for group_idx in 0..groups_len {
                    let Some((_, group_entry)) =
                        self.doc.get(&groups_map_id, group_idx).ok().flatten()
                    else {
                        continue;
                    };
                    let id_matches = matches!(
                        self.doc.get(&group_entry, "id").ok().flatten(),
                        Some((Value::Scalar(s), _)) if matches!(s.as_ref(), ScalarValue::Uint(id) if *id == group_id));
                    if !id_matches {
                        continue;
                    }
                    let tabs_map_id = match self.doc.get(&group_entry, "tabs").ok().flatten() {
                        Some((_, id)) => id,
                        None => match self.doc.put_object(&group_entry, "tabs", ObjType::List) {
                            Ok(id) => id,
                            Err(_) => return false,
                        },
                    };
                    let new_idx = self.doc.length(&tabs_map_id);
                    let Ok(tab_entry) = self.doc.insert_object(&tabs_map_id, new_idx, ObjType::Map)
                    else {
                        return false;
                    };
                    let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab_id));
                    let _ = self.doc.put(&tab_entry, "title", title);
                    if !tile_blob.is_empty() {
                        let _ =
                            self.doc
                                .put(&tab_entry, "tile_json", ScalarValue::Bytes(tile_blob));
                    }
                    return true;
                }
            }
            return false;
        }
        false
    }

    /// A first-boot tree: one empty workspace, nothing in it. What a
    /// machine with no `state.json` gets — no collaborators, window
    /// groups, or documents, since each would be a claim about someone
    /// who hasn't used the system yet. The workspace itself isn't
    /// fabrication: the compositor needs one to render into.
    ///
    /// For a populated tree, use [`Self::bootstrap_demo`].
    pub fn bootstrap_default() -> Self {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let home = Workspace {
            id: ws_id,
            room: RoomId::new(),
            name: "Home".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: Vec::new(),
            }],
            collaborators: Vec::new(),
            owner: None,
            capabilities: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            shared_surfaces: SharedSurfaces::default(),
        };

        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut tree = Self {
            workspaces: vec![home],
            active: ws_id,
            active_strip,
            active_group: HashMap::new(),
            collaborators: vec![],
            ..Default::default()
        };
        tree.seed_doc_from_cache();
        tree
    }

    /// A populated tree for demos, `--shots` scenarios and tests.
    /// Everything is invented, so it must never be reachable from a
    /// normal boot; production goes through [`Self::bootstrap_default`].
    pub fn bootstrap_demo() -> Self {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id = next_id();
        let tab_notes = next_id();
        let tab_files = next_id();
        let tab_terminal = next_id();
        let tab_word = next_id();
        // Home opens the four in-process apps at `WindowWidth::Wide` so
        // the card fills more than half the viewport on a 1600 px-wide
        // first-run desktop — matches `WindowFrame.tsx`'s behaviour.
        let home = Workspace {
            id: ws_id,
            room: RoomId::new(),
            name: "Home".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![WindowGroup {
                    id: group_id,
                    tabs: vec![
                        Tab {
                            id: tab_notes,
                            title: "Notes".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Other("Notes".into()),
                                label: "Notes".into(),
                            },
                        },
                        Tab {
                            id: tab_files,
                            title: "Files".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::FileExplorer,
                                label: "Files".into(),
                            },
                        },
                        Tab {
                            id: tab_terminal,
                            title: "Terminal".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Terminal,
                                label: "Terminal".into(),
                            },
                        },
                        Tab {
                            id: tab_word,
                            title: "Document".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Other("Word".into()),
                                label: "Document".into(),
                            },
                        },
                    ],
                    active_tab: tab_notes,
                    width: WindowWidth::Wide,
                    maximized: false,
                }],
            }],
            // One invented peer so the top-bar avatar stack has something
            // to paint. "me" is deliberately not seeded so
            // `draw_ws_settings` synthesizes its "You" tile from the
            // fallback path. Hex matches `nirios-concept/src/data.ts`.
            collaborators: vec![
                Collaborator::new("alice", "Alice")
                    .with_cursor_color("#ec4899")
                    .with_role("editor"),
            ],
            owner: None,
            capabilities: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            shared_surfaces: SharedSurfaces::default(),
        };

        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = HashMap::new();
        active_group.insert(strip_id, group_id);
        let mut tree = Self {
            workspaces: vec![home],
            active: ws_id,
            active_strip,
            active_group,
            collaborators: vec![],
            ..Default::default()
        };

        // Research and Design add empty strips for the overview's 3-card
        // grid; Research also gets two invented collaborators to render.
        let research_id = tree.append_empty_workspace("Research");
        if let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == research_id) {
            ws.collaborators.push(
                Collaborator::new("alice", "Alice")
                    .with_cursor_color("#ec4899")
                    .with_role("editor"),
            );
            ws.collaborators.push(
                Collaborator::new("bob", "Bob")
                    .with_cursor_color("#f97316")
                    .with_role("viewer"),
            );
        }
        tree.append_empty_workspace("Design");

        // Home stays active — `append_empty_workspace` doesn't touch
        // `tree.active`. This seeds the doc with the initial Home
        // workspace + active pointer the cache literal set directly.
        tree.seed_doc_from_cache();
        tree
    }

    pub fn append_empty_workspace(&mut self, name: &str) -> WorkspaceId {
        use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, transaction::Transactable};
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        // A workspace the user just created is a room that now exists;
        // it gets its identity at the same moment it gets its id, not
        // the first time someone tries to share it.
        let room = RoomId::new();
        let ws = Workspace {
            id: ws_id,
            room,
            name: name.into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }],
            collaborators: vec![],
            owner: None,
            capabilities: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            shared_surfaces: SharedSurfaces::default(),
        };
        self.active_strip.insert(ws_id, strip_id);
        self.workspaces.push(ws);

        // Mirror the append into the doc so peer-sync sees a discrete
        // change. Creates the list lazily, for trees not yet seeded.
        // Doc writes here are best-effort, same as `seed_doc_from_cache`
        // — the cache push above already happened, so a doc-write
        // failure just means this append doesn't replicate, not that it
        // fails outright.
        let workspaces_id = if let Some((_, id)) = self.doc.get(&ROOT, "workspaces").ok().flatten()
        {
            id
        } else {
            let Ok(id) = self.doc.put_object(&ROOT, "workspaces", ObjType::List) else {
                return ws_id;
            };
            id
        };
        let idx = self.doc.length(&workspaces_id);
        let Ok(entry) = self.doc.insert_object(&workspaces_id, idx, ObjType::Map) else {
            return ws_id;
        };
        let _ = self.doc.put(&entry, "id", ScalarValue::Uint(ws_id.raw()));
        let _ = self.doc.put(&entry, "name", name.to_string());
        // Room goes in with the workspace, not after: a peer must learn
        // the identity in the same change that tells it the workspace
        // exists, or it hydrates one it can't address.
        let _ = self.doc.put(&entry, "room", room.to_string());
        ws_id
    }

    pub fn set_active(&mut self, id: WorkspaceId) {
        use automerge::{ROOT, ScalarValue, transaction::Transactable};
        if self.workspaces.iter().any(|w| w.id == id) {
            self.active = id;
            let _ = self
                .doc
                .put(&ROOT, "active", ScalarValue::Uint(self.active.raw()));
        }
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == self.active)
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| w.id == self.active)
    }

    pub fn switch_up(&mut self) -> bool {
        use automerge::{ROOT, ScalarValue, transaction::Transactable};
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        let Some(prev_idx) = idx.checked_sub(1) else {
            return false;
        };
        let Some(prev) = self.workspaces.get(prev_idx) else {
            return false;
        };
        self.active = prev.id;
        let _ = self
            .doc
            .put(&ROOT, "active", ScalarValue::Uint(self.active.raw()));
        true
    }

    pub fn switch_down(&mut self) -> bool {
        use automerge::{ROOT, ScalarValue, transaction::Transactable};
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        let Some(next) = self.workspaces.get(idx.saturating_add(1)) else {
            return false;
        };
        self.active = next.id;
        let _ = self
            .doc
            .put(&ROOT, "active", ScalarValue::Uint(self.active.raw()));
        true
    }

    // Active-group pointer: one [`GroupId`] per [`StripId`], the
    // receiver of focus/keyboard/presence. Closing the active group's
    // last tab latches a 180 ms shrink before pruning, so
    // [`Self::nudge_active_group_off_closing`] advances the pointer to
    // the next surviving sibling at fade-start rather than letting it
    // resolve to the fading group for that whole window.

    /// The currently-active [`GroupId`] for `strip_id`, or `None` if
    /// unseeded — callers compose this with their own fallback.
    pub fn active_group_for(&self, strip_id: StripId) -> Option<GroupId> {
        self.active_group.get(&strip_id).copied()
    }

    /// Mark `group_id` as the active group of `strip_id`. Not validated
    /// against the strip's actual membership — the shell's reconciler is
    /// expected to be the sole writer.
    pub fn set_active_group(&mut self, strip_id: StripId, group_id: GroupId) {
        self.active_group.insert(strip_id, group_id);
    }

    /// Resolve the active group on `strip_id` under `is_closing`: if
    /// closing, returns the next non-closing sibling right, falling
    /// back left. Pure read; `None` if no group survives, or the strip
    /// doesn't exist.
    pub fn resolve_active_group(
        &self,
        strip_id: StripId,
        is_closing: impl Fn(GroupId) -> bool,
    ) -> Option<GroupId> {
        let strip = self
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .find(|s| s.id == strip_id)?;
        let recorded = self.active_group.get(&strip_id).copied();
        // First non-closing group in the strip scanning right
        // (increasing index) from — and including — `from`.
        let scan_right = |from: usize| -> Option<GroupId> {
            strip
                .window_groups
                .get(from..)
                .unwrap_or(&[])
                .iter()
                .find(|g| !is_closing(g.id))
                .map(|g| g.id)
        };
        // Same, scanning left (decreasing index) from — and including —
        // `from`.
        let scan_left = |from: usize| -> Option<GroupId> {
            strip
                .window_groups
                .get(..=from)
                .unwrap_or(&[])
                .iter()
                .rev()
                .find(|g| !is_closing(g.id))
                .map(|g| g.id)
        };
        match recorded {
            Some(id) => {
                if !is_closing(id) {
                    return Some(id);
                }
                // Recorded group is closing — find its index, then
                // scan right for a survivor; if none, scan left.
                let Some(idx) = strip.window_groups.iter().position(|g| g.id == id) else {
                    // Recorded group no longer in the strip; fall
                    // through to "first non-closing" as a default.
                    return scan_right(0);
                };
                scan_right(idx.saturating_add(1)).or_else(|| idx.checked_sub(1).and_then(scan_left))
            }
            None => scan_right(0),
        }
    }

    /// Mirror a cross-strip tab drag-drop into the persisted tree. The
    /// widget side only mutates the widget tree — without this writeback
    /// the tab would pop back to its source strip on the next restart.
    /// Removes the `Tab` from the source (pruning any group left empty),
    /// appends a fresh single-tab `WindowGroup` to the destination.
    ///
    /// Forgiving: `false` if `tab_id` or either strip id doesn't resolve.
    /// Searches every workspace since a drop could land outside the
    /// active one.
    pub fn move_tab_across_strips(
        &mut self,
        src_strip_id: StripId,
        dest_strip_id: StripId,
        tab_id: TabId,
    ) -> bool {
        if src_strip_id == dest_strip_id {
            // Return-to-source re-splices into a new group on the same
            // strip; the tab is still attributed to this strip, so
            // skipping the write is safe — a restart rebuilds it anyway.
            return false;
        }
        // Scan every workspace's strips: StripId is workspace-agnostic.
        let mut moved_tab: Option<Tab> = None;
        let mut prune_group_at: Option<(usize, usize, usize)> = None; // (ws_idx, strip_idx, group_idx)
        'outer: for (ws_idx, ws) in self.workspaces.iter_mut().enumerate() {
            for (strip_idx, strip) in ws.strips.iter_mut().enumerate() {
                if strip.id != src_strip_id {
                    continue;
                }
                for (group_idx, group) in strip.window_groups.iter_mut().enumerate() {
                    if let Some(pos) = group.tabs.iter().position(|t| t.id == tab_id) {
                        let removed = group.tabs.remove(pos);
                        // If the moved tab was active, promote the
                        // first remaining sibling so a future re-render
                        // doesn't dereference a stale active id.
                        if group.active_tab == tab_id
                            && let Some(first) = group.tabs.first()
                        {
                            group.active_tab = first.id;
                        }
                        if group.tabs.is_empty() {
                            prune_group_at = Some((ws_idx, strip_idx, group_idx));
                        }
                        moved_tab = Some(removed);
                        break 'outer;
                    }
                }
            }
        }
        let Some(moved) = moved_tab else { return false };
        if let Some((ws_idx, strip_idx, group_idx)) = prune_group_at {
            // `ws_idx`/`strip_idx` were captured in the scan just above and
            // nothing has mutated `self.workspaces` since, but a `.get_mut()`
            // chain costs nothing and can't panic if that ever changes.
            if let Some(groups) = self
                .workspaces
                .get_mut(ws_idx)
                .and_then(|w| w.strips.get_mut(strip_idx))
                .map(|s| &mut s.window_groups)
                && group_idx < groups.len()
            {
                groups.remove(group_idx);
            }
        }
        let new_group_id = next_id();
        let active_tab = moved.id;
        let new_group = WindowGroup {
            id: new_group_id,
            tabs: vec![moved],
            active_tab,
            width: WindowWidth::Normal,
            maximized: false,
        };
        for ws in &mut self.workspaces {
            for strip in &mut ws.strips {
                if strip.id == dest_strip_id {
                    strip.window_groups.push(new_group);
                    self.doc_move_tab_across_strips(src_strip_id, dest_strip_id, tab_id);
                    return true;
                }
            }
        }
        // Destination not found after the source was already mutated —
        // the tab is lost. Rare (strip vanished between drag start and
        // harvest tick).
        false
    }

    /// Reorder the window group `group_id` within `strip_id`; the whole
    /// group moves as a unit, unlike [`Self::move_tab_across_strips`].
    /// `new_index` is clamped to the post-removal index space, so a
    /// caller may pass a raw gap index. `true` only when the order
    /// actually changed. Mirrors into the doc afterwards, the same
    /// cache-then-doc shape as `move_tab_across_strips`.
    pub fn reorder_group_within_strip(
        &mut self,
        strip_id: StripId,
        group_id: u64,
        new_index: usize,
    ) -> bool {
        // Locate the strip (StripId is workspace-agnostic, so scan all).
        let mut located: Option<(usize, usize, usize)> = None; // (ws_idx, strip_idx, cur_idx)
        'scan: for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            for (strip_idx, strip) in ws.strips.iter().enumerate() {
                if strip.id != strip_id {
                    continue;
                }
                if let Some(cur_idx) = strip.window_groups.iter().position(|g| g.id == group_id) {
                    located = Some((ws_idx, strip_idx, cur_idx));
                }
                break 'scan;
            }
        }
        let Some((ws_idx, strip_idx, cur_idx)) = located else {
            return false;
        };
        // `ws_idx`/`strip_idx` were captured in the scan just above and
        // nothing has mutated `self.workspaces` since, but `.get_mut()`
        // costs nothing and can't panic if that ever changes.
        let Some(groups) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|w| w.strips.get_mut(strip_idx))
            .map(|s| &mut s.window_groups)
        else {
            return false;
        };
        // Clamp into the post-removal index space so a raw gap index
        // (which can equal `len`, "append") lands on the last slot.
        let dest = new_index.min(groups.len().saturating_sub(1));
        if dest == cur_idx || cur_idx >= groups.len() {
            return false;
        }
        let moved = groups.remove(cur_idx);
        groups.insert(dest, moved.clone());
        // Mirror from the cache copy we just placed, so the doc entry
        // carries the group's full content at the new slot. Tolerant
        // of an unseeded doc — see [`Self::locate_group`].
        self.doc_reorder_group_within_strip(strip_id, &moved, dest);
        true
    }

    /// Move the tab at `from` to index `to` inside `group_id`'s tab
    /// list — the persisted half of an intra-group tab drag, mirroring
    /// what the widget tree already reordered. `to` is clamped to the
    /// last slot; `true` only when the order actually changed.
    pub fn reorder_tab_within_group(&mut self, group_id: u64, from: usize, to: usize) -> bool {
        // Locate the group in the cache (ids are workspace-agnostic).
        let mut located: Option<(usize, usize, usize)> = None;
        'scan: for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            for (strip_idx, strip) in ws.strips.iter().enumerate() {
                if let Some(g_idx) = strip.window_groups.iter().position(|g| g.id == group_id) {
                    located = Some((ws_idx, strip_idx, g_idx));
                    break 'scan;
                }
            }
        }
        let Some((ws_idx, strip_idx, g_idx)) = located else {
            return false;
        };
        // `ws_idx`/`strip_idx`/`g_idx` were captured in the scan just
        // above and nothing has mutated `self.workspaces` since, but
        // `.get_mut()` costs nothing and can't panic if that ever changes.
        let Some(group) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|w| w.strips.get_mut(strip_idx))
            .and_then(|s| s.window_groups.get_mut(g_idx))
        else {
            return false;
        };
        if from >= group.tabs.len() {
            return false;
        }
        let dest = to.min(group.tabs.len().saturating_sub(1));
        if dest == from {
            return false;
        }
        let moved = group.tabs.remove(from);
        group.tabs.insert(dest, moved);
        // Mirror the reordered list into the doc so peer-sync ships the new
        // order. Cloned first because the doc write borrows `self` mutably.
        let tabs = group.tabs.clone();
        self.doc_rewrite_group_tabs(group_id, &tabs);
        true
    }

    /// Replace `group_id`'s doc-side `tabs` list with `tabs`, in order
    /// — rewrites the whole list to stay in step with
    /// [`Self::doc_reorder_group_within_strip`]. No-op on an unseeded
    /// doc.
    fn doc_rewrite_group_tabs(&mut self, group_id: u64, tabs: &[Tab]) {
        use automerge::{ObjType, ScalarValue, transaction::Transactable};
        let Some((group_entry, _, _, _)) = self.locate_group(group_id) else {
            return;
        };
        let Ok(tabs_id) = self.doc.put_object(&group_entry, "tabs", ObjType::List) else {
            return;
        };
        for (i, tab) in tabs.iter().enumerate() {
            if let Ok(tab_entry) = self.doc.insert_object(&tabs_id, i, ObjType::Map) {
                let _ = self.doc.put(&tab_entry, "id", ScalarValue::Uint(tab.id));
                let _ = self.doc.put(&tab_entry, "title", tab.title.clone());
                if let Ok(blob) = serde_json::to_vec(&tab.tile) {
                    let _ = self
                        .doc
                        .put(&tab_entry, "tile_json", ScalarValue::Bytes(blob));
                }
            }
        }
    }

    /// Reconcile the active-group pointer against `closing_ids`: if the
    /// recorded group is closing, advance to the next non-closing
    /// sibling. `None` when every group is closing — the entry is
    /// removed, since an emptying strip has no active group rather than
    /// one clamped to a corpse. Idempotent.
    pub fn nudge_active_group_off_closing(
        &mut self,
        strip_id: StripId,
        closing_ids: &[GroupId],
    ) -> Option<GroupId> {
        let recorded = self.active_group.get(&strip_id).copied()?;
        if !closing_ids.contains(&recorded) {
            return Some(recorded);
        }
        let resolved = self.resolve_active_group(strip_id, |id| closing_ids.contains(&id));
        if let Some(next) = resolved {
            self.active_group.insert(strip_id, next);
            Some(next)
        } else {
            // Whole strip is fading; drop the entry rather than
            // leave a dangling pointer. The shell re-seeds when
            // fresh groups land.
            self.active_group.remove(&strip_id);
            None
        }
    }
}

/// What colour each person on `roster` is drawn in. One function so
/// cursor and avatar cannot disagree and read as two participants: both
/// go through [`ui::presence::assign_peer_colors`], which settles the
/// whole room at once. An explicit [`Collaborator::cursor_color`]
/// outranks anything computed and is held back from the assignment; the
/// local user is included in the settled set (never the avatar stack) so
/// every host computes the same one.
pub fn roster_colors(roster: &[Collaborator]) -> HashMap<String, (u8, u8, u8)> {
    let mut explicit: HashMap<String, (u8, u8, u8)> = HashMap::new();
    for c in roster {
        if let Some(rgb) = c
            .cursor_color
            .as_deref()
            .and_then(ui::presence::parse_hex_color)
        {
            explicit.insert(c.id.clone(), rgb);
        }
    }
    let local = local_user_id();
    let mut ids: Vec<&str> = roster
        .iter()
        .filter(|c| !explicit.contains_key(&c.id))
        .map(|c| c.id.as_str())
        .collect();
    if !explicit.contains_key(&local) {
        ids.push(local.as_str());
    }
    let reserved: Vec<(u8, u8, u8)> = explicit.values().copied().collect();
    let mut assigned = ui::presence::assign_peer_colors(&ids, &reserved);
    assigned.extend(explicit);
    assigned
}

/// Peer count for the active workspace, excluding the local user,
/// falling back to the tree-level roster when it has none of its own —
/// matches `TopBarChrome::sync_from_tree`'s precedence.
pub fn active_workspace_peer_count(tree: &WorkspaceTree) -> usize {
    // Somebody named but not yet arrived is not counted. This drives the
    // avatar cluster's `+N` chip and the wheel disc's presence dot, and
    // both are claims that people are *in* the workspace — an invitation
    // that has not been redeemed is not one of them.
    let present = |c: &&Collaborator| c.id != "me" && !c.invited;
    let active = tree.active_workspace();
    let per_ws = active.map_or(0, |w| w.collaborators.iter().filter(present).count());
    if per_ws > 0 {
        return per_ws;
    }
    tree.collaborators.iter().filter(present).count()
}

#[cfg(test)]
mod bootstrap_tests {
    //! Coverage for the two bootstrap trees, which exist for opposite
    //! reasons and must not drift into each other: `bootstrap_default` is
    //! pinned empty (no invented collaborators, workspaces, or windows —
    //! see [`WorkspaceTree::bootstrap_default`]), `bootstrap_demo` is the
    //! populated fixture for `--shots` and tests needing a tab to act on.
    //!
    //! Re-pinning the demo fixture may also need re-seeding the e2e
    //! goldens (see `tests/e2e_shell_flows.rs`).

    use super::*;

    #[test]
    fn bootstrap_default_invents_no_people() {
        let tree = WorkspaceTree::bootstrap_default();
        assert!(
            tree.collaborators.is_empty(),
            "first-boot tree carries tree-level collaborators: {:?}",
            tree.collaborators.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
        for ws in &tree.workspaces {
            assert!(
                ws.collaborators.is_empty(),
                "first-boot workspace {:?} carries collaborators {:?} — a \
                 roster entry here is a person who does not exist, written \
                 into the user's workspace document and painted in the top bar",
                ws.name,
                ws.collaborators.iter().map(|c| &c.id).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn bootstrap_default_opens_no_windows() {
        let tree = WorkspaceTree::bootstrap_default();
        for ws in &tree.workspaces {
            for strip in &ws.strips {
                assert!(
                    strip.window_groups.is_empty(),
                    "first-boot workspace {:?} strip {:?} already holds {} \
                     window group(s) — windows the person never opened",
                    ws.name,
                    strip.name,
                    strip.window_groups.len()
                );
            }
        }
        assert!(
            tree.active_group.is_empty(),
            "first-boot tree points at an active group with no groups to point at"
        );
    }

    #[test]
    fn bootstrap_default_is_one_empty_workspace_with_somewhere_to_put_things() {
        let tree = WorkspaceTree::bootstrap_default();
        // Exactly one: two would mean naming a second one, and every name
        // ("Research", "Design") is a claim about what this person does.
        assert_eq!(
            tree.workspaces.len(),
            1,
            "first boot should be a single empty workspace; got {:?}",
            tree.workspaces.iter().map(|w| &w.name).collect::<Vec<_>>()
        );
        // Empty, but not structurally broken: the active pointer resolves,
        // and the workspace has a strip for the first window to land in.
        let active = tree
            .active_workspace()
            .expect("first boot must have a resolvable active workspace");
        assert_eq!(active.id, tree.workspaces[0].id);
        assert_eq!(
            active.strips.len(),
            1,
            "the empty workspace still needs a strip to place a window into"
        );
        assert_eq!(
            tree.active_strip.get(&active.id).copied(),
            Some(active.strips[0].id),
            "the active-strip pointer must resolve to the workspace's strip"
        );
    }

    #[test]
    fn bootstrap_demo_carries_three_workspaces() {
        let tree = WorkspaceTree::bootstrap_demo();
        assert!(
            tree.workspaces.len() >= 3,
            "demo tree should carry at least 3 workspaces (Home / Research / \
             Design) so the overview has meaningful content; got {}",
            tree.workspaces.len()
        );
        // This fixture's whole purpose is to be populated —
        // `bootstrap_default`, what a real boot reaches, is pinned empty.
        assert!(
            tree.workspaces.iter().any(|w| !w.collaborators.is_empty()),
            "at least one demo workspace should carry >=1 collaborator"
        );
    }

    /// The same person arriving twice is one avatar — presence
    /// republishes at ~30 Hz, so a push instead of an upsert would grow
    /// the stack by thirty entries a second.
    #[test]
    fn admitting_the_same_person_twice_is_one_row() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let ws = tree.active;
        assert!(tree.admit_collaborator(ws, Collaborator::new("alice", "Alice")));
        assert!(!tree.admit_collaborator(ws, Collaborator::new("alice", "Alice")));
        assert_eq!(tree.active_workspace().unwrap().collaborators.len(), 1);
        assert_eq!(active_workspace_peer_count(&tree), 1);
    }

    /// A room is addressed by its durable identity, not this machine's
    /// local handle for it: presence frames carry the room, and every
    /// peer numbers its workspaces differently.
    #[test]
    fn a_peer_is_admitted_to_the_workspace_that_addresses_the_room() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree.active_workspace().unwrap().room;
        assert!(tree.admit_collaborator_in_room(room, Collaborator::new("bob", "Bob")));
        assert_eq!(active_workspace_peer_count(&tree), 1);

        // A room this machine is not in has no workspace to admit into.
        assert!(!tree.admit_collaborator_in_room(RoomId::new(), Collaborator::new("eve", "Eve")));
        assert_eq!(active_workspace_peer_count(&tree), 1);
    }

    /// A session announces itself before it can say who is at it: the
    /// invite establishes the far end, the first presence frame names
    /// the person — one peer, so the named row replaces the handle row.
    #[test]
    fn a_named_presence_frame_replaces_the_session_handle_row() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let ws = tree.active;
        let room = tree.active_workspace().unwrap().room;
        let peer = 0x2a_u64;
        let handle = session_handle(peer);
        assert!(tree.admit_collaborator(ws, Collaborator::new(handle.clone(), handle)));

        assert!(tree.note_peer_present(room, peer, "alice"));
        let roster = &tree.active_workspace().unwrap().collaborators;
        assert_eq!(
            roster.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["alice"],
            "the session that joined and the session that just said \
             'alice is at me' are the same peer; got {roster:?}"
        );
        // And repeating it is idempotent.
        assert!(!tree.note_peer_present(room, peer, "alice"));
        assert_eq!(tree.active_workspace().unwrap().collaborators.len(), 1);
    }

    /// A publisher that names nobody keeps its handle — inventing a
    /// name is the same fabrication as seeding the roster.
    #[test]
    fn an_unnamed_session_stays_under_its_handle() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree.active_workspace().unwrap().room;
        assert!(tree.note_peer_present(room, 0x2a, ""));
        let roster = &tree.active_workspace().unwrap().collaborators;
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].id, "p000000000000002a");
        assert_eq!(roster[0].name, "p000000000000002a");
    }

    /// Leaving has to be expressible, or the roster only ever grows and
    /// says five people are here of a room four of them left.
    #[test]
    fn a_peer_can_be_taken_off_the_roster() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree.active_workspace().unwrap().room;
        tree.admit_collaborator_in_room(room, Collaborator::new("alice", "Alice"));
        tree.admit_collaborator_in_room(room, Collaborator::new("bob", "Bob"));
        assert_eq!(active_workspace_peer_count(&tree), 2);

        assert!(tree.retire_collaborator_in_room(room, "alice"));
        assert_eq!(active_workspace_peer_count(&tree), 1);
        assert!(
            !tree.retire_collaborator_in_room(room, "alice"),
            "retiring someone already gone changes nothing"
        );
        assert!(tree.retire_collaborator_in_room(room, "bob"));
        assert_eq!(
            active_workspace_peer_count(&tree),
            0,
            "a room everyone left shows nobody"
        );
    }

    /// A session that goes quiet takes its person off the roster by
    /// whichever name they'd acquired — a departure names a peer, never
    /// a person, which is what [`Collaborator::session`] bridges.
    #[test]
    fn a_session_going_quiet_takes_its_person_off_the_roster() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree.active_workspace().unwrap().room;
        let alice = 0x11_u64;
        let bob = 0x22_u64;
        assert!(tree.note_peer_present(room, alice, "alice"));
        assert!(tree.note_peer_present(room, bob, "bob"));
        assert_eq!(active_workspace_peer_count(&tree), 2);

        assert!(tree.retire_session(alice), "alice's session left");
        let left = &tree.active_workspace().unwrap().collaborators;
        assert_eq!(
            left.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["bob"],
            "the person whose session went quiet must come off the bar, \
             and nobody else; got {left:?}"
        );
        assert!(
            !tree.retire_session(alice),
            "a session that already left changes nothing"
        );
    }

    /// A departure reaches every room that session was in — one session
    /// can be in several rooms at once, and it left all of them.
    #[test]
    fn a_departure_sweeps_every_room_the_session_was_in() {
        // Two rooms, since one can't tell a sweep from a hit; both empty
        // so the "nobody left" assertion isn't confused by seeded people.
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.append_empty_workspace("Second");
        let rooms: Vec<RoomId> = tree.workspaces.iter().map(|w| w.room).collect();
        assert!(
            rooms.len() >= 2,
            "need two rooms to tell a sweep from a hit"
        );
        let peer = 0x33_u64;
        for r in &rooms {
            assert!(tree.note_peer_present(*r, peer, "dana"));
        }
        assert!(tree.retire_session(peer));
        for ws in &tree.workspaces {
            assert!(
                ws.collaborators.is_empty(),
                "workspace {:?} still shows a peer whose session left: {:?}",
                ws.name,
                ws.collaborators
            );
        }
    }

    /// A person already on the roster who then publishes presence must
    /// become retirable, not merely renamed — a session-less row (read
    /// back off disk) has `None`, which `retire_session` can't match.
    #[test]
    fn a_presence_frame_teaches_an_existing_row_which_session_it_is() {
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree.active_workspace().unwrap().room;
        let peer = 0x44_u64;

        // A row that got here without a session — what a persisted
        // roster looks like after a restart.
        assert!(tree.admit_collaborator_in_room(room, Collaborator::new("erin", "Erin")));
        assert_eq!(
            tree.active_workspace().unwrap().collaborators[0].session,
            None,
            "fixture precondition: the row starts with no session"
        );

        // Erin publishes presence from a live session.
        tree.note_peer_present(room, peer, "erin");
        assert_eq!(
            tree.active_workspace().unwrap().collaborators.len(),
            1,
            "the same person is one row, not two"
        );

        // That session going quiet must take her off the bar.
        assert!(
            tree.retire_session(peer),
            "a row that has published presence must be retirable by the \
             session it published from; otherwise the avatar stays on the \
             bar for the rest of the session"
        );
        assert!(
            tree.active_workspace().unwrap().collaborators.is_empty(),
            "got {:?}",
            tree.active_workspace().unwrap().collaborators
        );
    }

    /// Roster avatars and peer cursors must be the same colour for the
    /// same person — drawn by different code in different files.
    #[test]
    fn roster_colours_match_what_the_room_settles_for_cursors() {
        let roster = vec![
            Collaborator::new("alice", "Alice"),
            Collaborator::new("bob", "Bob"),
            Collaborator::new("carol", "Carol"),
        ];
        let mine = roster_colors(&roster);

        // What the cursor overlay computes for the same room: everyone
        // in it, the local user included.
        let local = local_user_id();
        let mut ids: Vec<&str> = roster.iter().map(|c| c.id.as_str()).collect();
        ids.push(local.as_str());
        let cursors = ui::presence::assign_peer_colors(&ids, &[]);

        for c in &roster {
            assert_eq!(
                mine.get(&c.id),
                cursors.get(c.id.as_str()),
                "{}'s avatar and cursor must be the same colour",
                c.id
            );
        }
        // `carol` hashes onto `alice`'s pinned pink; the room has to
        // have moved one of them, or two people are one colour.
        assert_ne!(
            mine.get("alice"),
            mine.get("carol"),
            "two people in one room must not be painted the same colour"
        );
    }

    /// An explicit roster colour outranks the assignment, and is held
    /// back from it, so nobody else is computed onto it.
    #[test]
    fn an_explicit_roster_colour_wins_and_is_not_reused() {
        let pink = (0xec, 0x48, 0x99);
        let roster = vec![
            Collaborator::new("bob", "Bob").with_cursor_color("#ec4899"),
            Collaborator::new("alice", "Alice"),
        ];
        let colors = roster_colors(&roster);
        assert_eq!(colors.get("bob"), Some(&pink), "bob asked for pink");
        assert_ne!(
            colors.get("alice"),
            Some(&pink),
            "alice's pinned pink is taken, so the room must move her"
        );
    }

    /// A machine that has just been installed has had nobody join it, so
    /// every roster the first-boot tree hands the chrome is empty.
    #[test]
    fn bootstrap_default_seeds_nobody_into_any_roster() {
        let tree = WorkspaceTree::bootstrap_default();
        for ws in &tree.workspaces {
            assert!(
                ws.collaborators.is_empty(),
                "workspace {:?} boots with {} collaborator(s) nobody invited: \
                 {:?}. The avatar stack is a claim about who is present; \
                 seeding it fabricates participants.",
                ws.name,
                ws.collaborators.len(),
                ws.collaborators.iter().map(|c| &c.name).collect::<Vec<_>>(),
            );
        }
        assert!(
            tree.collaborators.is_empty(),
            "the tree-level roster fallback boots non-empty: {:?}",
            tree.collaborators
                .iter()
                .map(|c| &c.name)
                .collect::<Vec<_>>(),
        );
        // And the chrome's own arithmetic agrees: no peers to count.
        assert_eq!(
            active_workspace_peer_count(&tree),
            0,
            "a fresh boot must count zero peers in the active workspace"
        );
    }

    #[test]
    fn bootstrap_demo_first_group_uses_wide_or_larger() {
        let tree = WorkspaceTree::bootstrap_demo();
        let first_ws = tree.workspaces.first().expect("at least one workspace");
        let first_strip = first_ws.strips.first().expect("at least one strip");
        let first_group = first_strip
            .window_groups
            .first()
            .expect("at least one group");
        // Widths are fractions of the strip, so "wide enough" is a share
        // of the display rather than a pixel count that means a different
        // share on every monitor.
        let share = first_group.width.permille();
        assert!(
            share >= ui::widget::WIDTH_NORMAL_PERMILLE,
            "the demo group should fill at least the Normal share of the \
             strip so the white card dominates the viewport; got {:?} \
             ({share}/1000)",
            first_group.width
        );
    }
}

#[cfg(test)]
mod automerge_foundation_tests {
    //! Tests for the `WorkspaceTree` ↔ automerge sync surface:
    //! `current_heads`, `export_changes_since`, `apply_changes`, and the
    //! mutators (`append_empty_workspace`, …) that write through them.

    use super::*;
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};

    #[test]
    fn empty_tree_has_no_heads_and_no_changes() {
        let mut t = WorkspaceTree::default();
        assert!(t.current_heads().is_empty());
        assert!(t.export_changes_since(&[]).is_empty());
    }

    #[test]
    fn apply_changes_round_trips_a_manual_doc_mutation() {
        let mut tree_a = WorkspaceTree::default();
        // Synthesise a change by writing directly into the doc rather
        // than through a mutator.
        tree_a
            .doc
            .put(&ROOT, "demo", ScalarValue::Uint(42))
            .unwrap();

        let heads_after = tree_a.current_heads();
        assert!(!heads_after.is_empty());
        let changes = tree_a.export_changes_since(&[]);
        assert!(!changes.is_empty());

        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).unwrap();

        // Tree B now sees the same demo value.
        let val = tree_b.doc.get(&ROOT, "demo").unwrap();
        assert!(val.is_some());
    }

    #[test]
    fn export_changes_since_current_heads_yields_empty() {
        let mut t = WorkspaceTree::default();
        t.doc.put(&ROOT, "k", ScalarValue::Uint(1)).unwrap();
        let heads = t.current_heads();
        let changes = t.export_changes_since(&heads);
        assert!(
            changes.is_empty(),
            "exporting since current heads must yield no changes"
        );
    }

    #[test]
    fn appending_a_workspace_propagates_via_apply_changes() {
        // Tree B starts from `default()`, not `bootstrap_default`, so
        // its empty doc cleanly absorbs A's full change-graph without
        // both peers concurrently creating the `workspaces` list at
        // ROOT — seeding isn't diff-aware.
        let mut tree_a = WorkspaceTree::bootstrap_demo();
        let initial_count = tree_a.workspaces.len();

        tree_a.append_empty_workspace("Project X");
        assert_eq!(tree_a.workspaces.len(), initial_count + 1);

        let changes = tree_a.export_changes_since(&[]);
        assert!(
            !changes.is_empty(),
            "append_empty_workspace must produce doc-side changes"
        );

        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        assert!(
            tree_b.workspaces.iter().any(|w| w.name == "Project X"),
            "tree B should observe the appended workspace after sync; \
             got names: {:?}",
            tree_b
                .workspaces
                .iter()
                .map(|w| &w.name)
                .collect::<Vec<_>>()
        );
    }

    /// A peer that hydrates a workspace out of another peer's doc must
    /// end up in *that peer's* room. Minting a local one instead would
    /// leave it holding a workspace it can see but cannot collaborate
    /// in.
    #[test]
    fn a_workspaces_room_propagates_via_apply_changes() {
        let mut tree_a = WorkspaceTree::bootstrap_demo();
        let new_id = tree_a.append_empty_workspace("Project X");
        let room = tree_a.room_of(new_id).expect("the append mints a room");
        assert!(!room.is_nil());

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        let hydrated = tree_b
            .workspaces
            .iter()
            .find(|w| w.name == "Project X")
            .expect("peer B should observe the appended workspace");
        assert_eq!(
            hydrated.room, room,
            "peer B must adopt the room peer A named, not mint one of its \
             own — otherwise the two hold the same workspace and address \
             different clusters"
        );
        assert_eq!(
            hydrated.collab_cluster_id(),
            tree_a
                .workspaces
                .iter()
                .find(|w| w.id == new_id)
                .expect("peer A's workspace")
                .collab_cluster_id(),
            "and the cluster their apps are launched into must match"
        );
    }

    /// `set_room` is what an accepted invite performs — must reach the
    /// doc as well as the cache, or the change never replicates.
    #[test]
    fn set_room_propagates_via_apply_changes() {
        let mut tree_a = WorkspaceTree::bootstrap_demo();
        let id = tree_a.workspaces[0].id;
        let inviters_room = RoomId::new();
        assert!(tree_a.set_room(id, inviters_room), "set_room must apply");

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        assert_eq!(
            tree_b.room_of(id),
            Some(inviters_room),
            "a workspace moved into a new room must carry that room to its \
             peers"
        );
    }

    /// Two trees in one room, so a record-index claim can be read back
    /// from the side that did not make it.
    fn two_in_a_room() -> (WorkspaceTree, WorkspaceTree, RoomId) {
        let mut host = WorkspaceTree::bootstrap_demo();
        let host_ws = host.workspaces[0].id;
        let room = host.room_of(host_ws).expect("bootstrap mints a room");
        host.adopt_ownerless_workspaces(&"quinten".to_string());

        let mut guest = WorkspaceTree::bootstrap_demo();
        let guest_ws = guest.workspaces[0].id;
        assert!(guest.set_room(guest_ws, room));
        guest.adopt_ownerless_workspaces(&"xaver".to_string());
        (host, guest, room)
    }

    /// A window somebody else opened lands *beside* yours, not
    /// underneath — groups scroll horizontally within a strip, so a
    /// peer's window becoming its own strip would stack vertically
    /// on a desktop that doesn't do that.
    #[test]
    fn a_peers_window_lands_beside_yours_and_not_below_it() {
        const HOST: u16 = 0x2222;
        const GUEST: u16 = 0x3333;
        let (mut host, mut guest, room) = two_in_a_room();
        host.set_salt_override(HOST);
        guest.set_salt_override(GUEST);

        // Each has one window of their own, in a strip of their own.
        for (tree, salt, title) in [
            (&mut host, HOST, "Host window"),
            (&mut guest, GUEST, "Guest window"),
        ] {
            let ws = tree.workspaces.iter().position(|w| w.room == room).unwrap();
            let tab_id = salted(salt, 3);
            tree.workspaces[ws].strips = vec![Strip {
                id: StripId::from(salted(salt, 1)),
                name: "Main".into(),
                window_groups: vec![WindowGroup {
                    id: salted(salt, 2),
                    tabs: vec![Tab {
                        id: tab_id,
                        title: title.into(),
                        tile: TileContent::AppPlaceholder {
                            kind: AppKind::Other("Notes".into()),
                            label: title.into(),
                        },
                    }],
                    active_tab: tab_id,
                    width: WindowWidth::Normal,
                    maximized: false,
                }],
            }];
            tree.sync_room_doc(room);
        }

        ship(&mut host, &mut guest, room, 1);

        let ws = guest.workspaces.iter().find(|w| w.room == room).unwrap();
        let titles: Vec<&str> = ws
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .flat_map(|g| g.tabs.iter())
            .map(|t| t.title.as_str())
            .collect();
        assert!(
            titles.contains(&"Host window") && titles.contains(&"Guest window"),
            "the host's window has to arrive at all; got {titles:?}",
        );
        assert_eq!(
            ws.strips.len(),
            1,
            "both windows belong in one horizontally-scrolling strip; \
             {} strips means they are stacked vertically, with strips {:?}",
            ws.strips.len(),
            ws.strips
                .iter()
                .map(|s| (s.id.raw(), s.window_groups.len()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            ws.strips[0].window_groups.len(),
            2,
            "one group each, side by side",
        );
    }

    fn ship(from: &mut WorkspaceTree, to: &mut WorkspaceTree, room: RoomId, hub: u64) {
        let changes = from
            .export_room_changes_since(room, &[])
            .expect("in the room");
        to.apply_room_changes(room, &changes, &PeerOrigin::hub(hub))
            .expect("merges");
    }

    fn held(one: (&str, &str)) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::from([(one.0.to_string(), one.1.to_string())])
    }

    /// An id in `salt`'s range, without touching the process-wide
    /// counter — `next_id()` reads a global salt a test can't flip
    /// without affecting every test beside it.
    fn salted(salt: u16, counter: u64) -> u64 {
        (u64::from(salt) << PEER_ID_SALT_SHIFT) | counter
    }

    /// The claim the whole room-index path exists to make: a record one
    /// member puts in the room reaches the others.
    #[test]
    fn a_record_referenced_here_reaches_the_other_member() {
        let (mut host, mut guest, room) = two_in_a_room();
        host.set_room_records(room, held(("file:/docs/plan.md", "document")));

        ship(&mut host, &mut guest, room, 1);

        assert_eq!(
            guest.take_inbound_room_records(),
            vec![(
                room,
                "file:/docs/plan.md".to_string(),
                "document".to_string()
            )],
            "the guest must be handed the reference its host made",
        );
    }

    /// And the other way: a room only its owner may add to isn't a
    /// shared room — the projection isn't gated on governance the
    /// way owner/capabilities are.
    #[test]
    fn a_guest_can_put_a_record_in_the_room_too() {
        let (mut host, mut guest, room) = two_in_a_room();
        guest.set_room_records(room, held(("file:/docs/from-xaver.md", "document")));

        ship(&mut guest, &mut host, room, 2);

        assert_eq!(
            host.take_inbound_room_records(),
            vec![(
                room,
                "file:/docs/from-xaver.md".to_string(),
                "document".to_string()
            )],
            "a guest's reference must reach the room's owner",
        );
    }

    /// Two people bringing something at the same moment — keyed by
    /// record id rather than a list, so the merge is a union.
    #[test]
    fn two_members_referencing_at_once_both_survive() {
        let (mut host, mut guest, room) = two_in_a_room();
        host.set_room_records(room, held(("file:/docs/from-quinten.md", "document")));
        guest.set_room_records(room, held(("file:/docs/from-xaver.md", "document")));

        // Neither has seen the other's yet: both project, then both merge.
        ship(&mut host, &mut guest, room, 1);
        ship(&mut guest, &mut host, room, 2);

        for (who, tree) in [("host", &mut host), ("guest", &mut guest)] {
            let doc = tree.room_docs.get(&room).expect("a room document");
            let ids: Vec<String> = WorkspaceTree::room_records_of(doc)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            assert_eq!(
                ids,
                vec![
                    "file:/docs/from-quinten.md".to_string(),
                    "file:/docs/from-xaver.md".to_string()
                ],
                "{who} lost one of two simultaneous references",
            );
        }
    }

    /// Re-projecting unchanged state must not manufacture a change, or
    /// two peers exporting to each other would never fall quiet.
    #[test]
    fn re_projecting_the_same_index_says_nothing_new() {
        let (mut host, _guest, room) = two_in_a_room();
        host.set_room_records(room, held(("file:/docs/plan.md", "document")));
        host.sync_room_doc(room);
        let heads = host.room_heads(room);

        host.sync_room_doc(room);
        host.sync_room_doc(room);

        assert_eq!(
            host.room_heads(room),
            heads,
            "an export of unchanged state must add no changes",
        );
    }

    /// A record already known here isn't re-announced as an arrival —
    /// otherwise the shell gets handed the same reference every round.
    #[test]
    fn a_reference_already_held_here_is_not_an_arrival() {
        let (mut host, mut guest, room) = two_in_a_room();
        host.set_room_records(room, held(("file:/docs/plan.md", "document")));
        ship(&mut host, &mut guest, room, 1);
        assert_eq!(guest.take_inbound_room_records().len(), 1);

        // The shell folded it in, so the guest now holds it too.
        guest.set_room_records(room, held(("file:/docs/plan.md", "document")));
        ship(&mut host, &mut guest, room, 1);

        assert!(
            guest.take_inbound_room_records().is_empty(),
            "a reference this machine already holds must not arrive again",
        );
    }

    /// **The view does not follow, the state does.** The room document
    /// is what crosses, so the guarantee is checkable by looking at
    /// what's in it: which records the room holds, nothing about who's
    /// standing where.
    #[test]
    fn the_room_document_carries_no_view_state() {
        use automerge::{ROOT, ReadDoc};

        let (mut host, _guest, room) = two_in_a_room();
        host.set_room_records(room, held(("file:/docs/plan.md", "document")));
        // Put this session somewhere specific, so anything that leaked
        // would have a value to leak.
        let ws = host.workspaces[0].id;
        let strip = host.workspaces[0].strips[0].id;
        let group = host.workspaces[0].strips[0].window_groups[0].id;
        host.active_strip.insert(ws, strip);
        host.set_active_group(strip, group);
        host.sync_room_doc(room);

        let doc = host.room_docs.get(&room).expect("a room document");
        let keys: Vec<String> = doc.keys(&ROOT).collect();
        assert!(
            keys.contains(&"records".to_string()),
            "the room's records must be in its document; got {keys:?}",
        );
        for forbidden in ["active", "active_strip", "active_group", "workspaces"] {
            assert!(
                !keys.contains(&forbidden.to_string()),
                "{forbidden:?} is a fact about a person at a desk, not about the \
                 room, and must not be in the document that crosses; got {keys:?}",
            );
        }
    }

    /// A window one member opens appears for the other — **and the
    /// other stays on the tab they were reading.** Both halves in one
    /// test: projecting the whole struct passes one and fails the
    /// other; projecting nothing does the reverse.
    #[test]
    fn a_window_the_host_opens_appears_for_the_guest_without_moving_them() {
        const HOST: u16 = 0xAAAA;
        const GUEST: u16 = 0xBBBB;
        let (mut host, mut guest, room) = two_in_a_room();
        host.set_salt_override(HOST);
        guest.set_salt_override(GUEST);

        let (_host_strip, host_group, host_tab) = seed_single_tab_strip(&mut host, room, HOST);
        // The guest has a desk of their own, and is reading their second
        // tab — the thing that must not move.
        let (guest_ws, guest_strip, guest_group, _guest_tab_a, guest_tab_b) =
            seed_two_tab_strip_and_focus_second(&mut guest, room, GUEST);

        let changes = host
            .export_room_changes_since(room, &[])
            .expect("the host is in the room");
        guest
            .apply_room_changes(room, &changes, &PeerOrigin::hub(1))
            .expect("the guest merges");

        // The window opened for the guest.
        let gws = guest.workspaces.iter().find(|w| w.room == room).unwrap();
        let landed = gws
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == host_group)
            .expect("the host's window group must exist on the guest");
        assert_eq!(
            landed
                .tabs
                .iter()
                .map(|t| t.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Plan"],
            "the window must arrive with what is in it",
        );
        // Opened on its own first tab — decided here, not carried.
        assert_eq!(landed.active_tab, host_tab);

        // And the guest did not move.
        let mine = gws
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == guest_group)
            .expect("the guest keeps its own window");
        assert_eq!(
            mine.active_tab, guest_tab_b,
            "the guest was reading their second tab; a window opening \
             elsewhere in the room must not move them off it",
        );
        assert_eq!(
            guest.active_strip.get(&guest_ws).copied(),
            Some(guest_strip),
            "the guest's scrolled-to strip must not follow the host",
        );
        assert_eq!(
            guest.active_group.get(&guest_strip).copied(),
            Some(guest_group),
            "the guest's focus must not follow the host",
        );
    }

    /// Give `tree`'s workspace in `room` a single strip/group holding one
    /// tab titled "Plan", salted under `salt`. Returns `(strip, group,
    /// tab)`. Half of
    /// [`a_window_the_host_opens_appears_for_the_guest_without_moving_them`]'s
    /// fixture — the host's side.
    fn seed_single_tab_strip(
        tree: &mut WorkspaceTree,
        room: RoomId,
        salt: u16,
    ) -> (StripId, u64, u64) {
        let strip = StripId::from(salted(salt, 1));
        let group = salted(salt, 2);
        let tab = salted(salt, 3);
        let i = tree.workspaces.iter().position(|w| w.room == room).unwrap();
        tree.workspaces[i].strips = vec![Strip {
            id: strip,
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: group,
                tabs: vec![Tab {
                    id: tab,
                    title: "Plan".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Terminal,
                        label: "Plan".into(),
                    },
                }],
                active_tab: tab,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }];
        (strip, group, tab)
    }

    /// Give `tree`'s workspace in `room` a strip/group holding two tabs
    /// ("One", "Two"), salted under `salt`, and focus it on the second
    /// tab. Returns `(workspace, strip, group, tab_a, tab_b)`. The other
    /// half of
    /// [`a_window_the_host_opens_appears_for_the_guest_without_moving_them`]'s
    /// fixture — the guest's side, whose focus must not move.
    fn seed_two_tab_strip_and_focus_second(
        tree: &mut WorkspaceTree,
        room: RoomId,
        salt: u16,
    ) -> (WorkspaceId, StripId, u64, u64, u64) {
        let strip = StripId::from(salted(salt, 1));
        let group = salted(salt, 2);
        let tab_a = salted(salt, 3);
        let tab_b = salted(salt, 4);
        let i = tree.workspaces.iter().position(|w| w.room == room).unwrap();
        let ws = tree.workspaces[i].id;
        tree.workspaces[i].strips = vec![Strip {
            id: strip,
            name: "Mine".into(),
            window_groups: vec![WindowGroup {
                id: group,
                tabs: vec![
                    Tab {
                        id: tab_a,
                        title: "One".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Terminal,
                            label: "One".into(),
                        },
                    },
                    Tab {
                        id: tab_b,
                        title: "Two".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Terminal,
                            label: "Two".into(),
                        },
                    },
                ],
                active_tab: tab_b,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }];
        tree.active_strip.insert(ws, strip);
        tree.set_active_group(strip, group);
        (ws, strip, group, tab_a, tab_b)
    }

    /// A peer may add to the room; it may not rewrite what this machine
    /// owns. The batch is refused whole rather than partly merged.
    #[test]
    fn a_peer_may_not_rewrite_this_machines_own_window() {
        const HOST: u16 = 0xA1A1;
        const GUEST: u16 = 0xB2B2;
        let (mut host, mut guest, room) = two_in_a_room();
        host.set_salt_override(HOST);
        guest.set_salt_override(GUEST);

        let strip = StripId::from(salted(HOST, 1));
        let group = salted(HOST, 2);
        let tab = salted(HOST, 3);
        let hi = host.workspaces.iter().position(|w| w.room == room).unwrap();
        host.workspaces[hi].strips = vec![Strip {
            id: strip,
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: group,
                tabs: vec![Tab {
                    id: tab,
                    title: "Plan".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Terminal,
                        label: "Plan".into(),
                    },
                }],
                active_tab: tab,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }];

        // The guest takes the host's document, renames the host's own
        // window in it, and offers it back.
        let seed = host
            .export_room_changes_since(room, &[])
            .expect("host is in the room");
        guest
            .apply_room_changes(room, &seed, &PeerOrigin::hub(1))
            .expect("guest merges the host's room");
        {
            use automerge::{ROOT, ReadDoc, transaction::Transactable};
            let doc = guest.room_docs.get_mut(&room).expect("room doc");
            let (_, contents) = doc.get(&ROOT, "contents").ok().flatten().expect("contents");
            let (_, s_entry) = doc
                .get(&contents, WorkspaceTree::contents_key(strip.raw()).as_str())
                .ok()
                .flatten()
                .expect("strip");
            let (_, groups) = doc.get(&s_entry, "groups").ok().flatten().expect("groups");
            let (_, g_entry) = doc
                .get(&groups, WorkspaceTree::contents_key(group).as_str())
                .ok()
                .flatten()
                .expect("group");
            let (_, tabs) = doc.get(&g_entry, "tabs").ok().flatten().expect("tabs");
            let (_, t_entry) = doc
                .get(&tabs, WorkspaceTree::contents_key(tab).as_str())
                .ok()
                .flatten()
                .expect("tab");
            let _ = doc.put(&t_entry, "title", "Hijacked".to_string());
        }
        let tampered = guest
            .export_room_changes_since(room, &[])
            .expect("guest is in the room");

        let accepted = host
            .apply_room_changes(room, &tampered, &PeerOrigin::hub(2))
            .expect("the check is a refusal, not an error");
        assert!(
            !accepted,
            "a batch rewriting our own window must be refused"
        );
        assert_eq!(
            host.workspaces[hi].strips[0].window_groups[0].tabs[0].title, "Plan",
            "and nothing about it may be stored",
        );
    }

    /// The other half of the same rule: the room's windows **are** in
    /// the document, but `active_tab` is asserted against the
    /// document's own keys rather than inferred from behaviour.
    #[test]
    fn the_rooms_windows_replicate_but_not_the_tab_you_are_on() {
        use automerge::{ROOT, ReadDoc};

        const HOST: u16 = 0x1111;
        let (mut host, _guest, room) = two_in_a_room();
        host.set_salt_override(HOST);
        // Spelled out under a real salt: what `bootstrap_default` made
        // carries salt 0, which is nobody's and is never projected.
        let ws_idx = host.workspaces.iter().position(|w| w.room == room).unwrap();
        let strip_id = StripId::from(salted(HOST, 1));
        let group_id = salted(HOST, 2);
        let tab_id = salted(HOST, 3);
        host.workspaces[ws_idx].strips = vec![Strip {
            id: strip_id,
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: group_id,
                tabs: vec![Tab {
                    id: tab_id,
                    title: "Notes".into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("Notes".into()),
                        label: "Notes".into(),
                    },
                }],
                active_tab: tab_id,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }];
        host.sync_room_doc(room);

        let doc = host.room_docs.get(&room).expect("a room document");
        let (_, contents) = doc
            .get(&ROOT, "contents")
            .ok()
            .flatten()
            .expect("the room's windows must be in its document");
        let (_, strip) = doc
            .get(
                &contents,
                WorkspaceTree::contents_key(strip_id.raw()).as_str(),
            )
            .ok()
            .flatten()
            .expect("the strip");
        let (_, groups) = doc.get(&strip, "groups").ok().flatten().expect("groups");
        let (_, group) = doc
            .get(&groups, WorkspaceTree::contents_key(group_id).as_str())
            .ok()
            .flatten()
            .expect("the group");

        let group_keys: Vec<String> = doc.keys(&group).collect();
        assert!(
            group_keys.contains(&"tabs".to_string()),
            "the group's tabs must replicate; got {group_keys:?}",
        );
        assert!(
            !group_keys.contains(&"active_tab".to_string()),
            "which tab someone is looking at is theirs, not the room's, and \
             must not be in the document that crosses; got {group_keys:?}",
        );
    }

    /// A guest that has projected its own room document must survive
    /// merging the host's — this is a crash, not a wrong answer: both
    /// sides project before syncing, and applying the other's changes
    /// runs `PatchLog::migrate_actors`, which panics if the incoming
    /// actor can't be spliced into the actor list in sort order.
    #[test]
    fn a_guest_merges_its_hosts_room_after_projecting_its_own() {
        for _ in 0..24 {
            // The host's room is the one its own workspace was minted
            // with, so the host actually administers it — the room being
            // synced has a governance to be right about.
            let mut host = WorkspaceTree::bootstrap_demo();
            let host_ws = host.workspaces[0].id;
            let room = host.room_of(host_ws).expect("bootstrap mints a room");
            host.adopt_ownerless_workspaces(&"alice".to_string());

            let mut guest = WorkspaceTree::bootstrap_demo();
            let guest_ws = guest.workspaces[0].id;
            assert!(guest.set_room(guest_ws, room));
            guest.adopt_ownerless_workspaces(&"bob".to_string());

            // The guest projects first — that is what leaves a patch log
            // on its room document for the host's actor to collide with.
            guest.sync_room_doc(room);

            let changes = host
                .export_room_changes_since(room, &[])
                .expect("the host is in the room");
            guest
                .apply_room_changes(room, &changes, &PeerOrigin::hub(1))
                .expect("the guest merges its host's room document");

            // The merge completes, and it is the host's governance that
            // comes out of it — not whichever of the two actor ids sorted
            // higher.
            assert_eq!(
                guest
                    .workspaces
                    .iter()
                    .find(|w| w.id == guest_ws)
                    .and_then(|w| w.owner.clone())
                    .as_deref(),
                Some("alice"),
                "the guest must end up with the room's owner, not its own",
            );
            assert_eq!(
                guest.room_of(guest_ws),
                Some(room),
                "the guest must still be in the room it merged",
            );
            assert!(
                !guest
                    .workspaces
                    .iter()
                    .find(|w| w.id == guest_ws)
                    .expect("the guest's workspace survives the merge")
                    .capabilities
                    .is_empty(),
                "the merge must leave a capability list behind, not wipe it",
            );
        }
    }

    // Write authority on a room's governance: automerge has no per-key
    // ACL, so a peer projecting its own cache isn't proposing a change,
    // it's entering a coin toss for it — these pin that governance
    // comes only from the entitled peer.

    /// The room's real administrator, as the receiver knows it.
    const ROOM_OWNER: &str = "alice-administers-the-room";
    /// A guest that was invited in.
    const ROOM_GUEST: &str = "bob-was-invited";
    /// The capability the owner granted, and the one a guest tries to add.
    const OWNER_GRANT_PATH: &str = "/home/alice/room-notes";
    const ROGUE_GRANT_PATH: &str = "/etc/shadow";

    /// A host in a room it minted, owning it, with one grant of its own.
    /// Minted via `append_empty_workspace`, not `set_room` — minting
    /// and joining are different acts, and the difference is the
    /// whole subject here.
    fn host_owning_a_room() -> (WorkspaceTree, WorkspaceId, RoomId) {
        let mut host = WorkspaceTree::default();
        let ws = host.append_empty_workspace("Design review");
        let room = host.room_of(ws).expect("a fresh workspace mints a room");
        host.adopt_ownerless_workspaces(&ROOM_OWNER.to_string());
        host.grant_in_workspace(
            ws,
            &ROOM_OWNER.to_string(),
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath(OWNER_GRANT_PATH.to_string()),
                Verb::Read,
            )
            .with_id("owners-own-grant"),
        )
        .expect("the owner may grant in the room it owns");
        (host, ws, room)
    }

    /// The bytes a guest puts on the wire when it does not honour the
    /// join: in `room`, but its own state still says it governs, so its
    /// projection carries its own owner and grants. Built by driving
    /// the guest's real projection/export path, not by hand.
    fn governance_claim_from(user: &str, room: RoomId) -> Vec<Vec<u8>> {
        let mut guest = WorkspaceTree::default();
        let guest_ws = guest.append_empty_workspace("Mine");
        assert!(guest.set_room(guest_ws, room), "the guest joins the room");
        // Nothing stops a peer from believing it administers a room —
        // it's *its* machine — which is why the receiver can't take the
        // sender's word for who governs it.
        guest.room_authority.insert(room, RoomAuthority::Local);
        {
            let ws = guest
                .workspaces
                .iter_mut()
                .find(|w| w.id == guest_ws)
                .expect("the guest's own workspace");
            ws.set_owner(user.to_string());
        }
        guest
            .grant_in_workspace(
                guest_ws,
                &user.to_string(),
                CapOverride::grant(
                    Subject::app("rogue"),
                    Scope::FsPath(ROGUE_GRANT_PATH.to_string()),
                    Verb::Write,
                )
                .with_id("rogue-grant"),
            )
            .expect("the guest grants in its own copy");
        guest.sync_room_doc(room);
        guest
            .export_room_changes_since(room, &[])
            .expect("the guest is in the room it exports")
    }

    /// The owner and capability ids the receiver's own **room document**
    /// records — not its cache, so a divergent fold can't hide behind it.
    fn governance_in_room_doc(tree: &WorkspaceTree, room: RoomId) -> (Option<String>, Vec<String>) {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};
        let Some(doc) = tree.room_docs.get(&room) else {
            return (None, Vec::new());
        };
        let owner = match doc.get(&ROOT, "owner").ok().flatten() {
            Some((Value::Scalar(s), _)) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                _ => None,
            },
            _ => None,
        };
        let mut caps: Vec<String> = match doc.get(&ROOT, "capabilities").ok().flatten() {
            Some((_, caps_id)) => doc.keys(&caps_id).collect(),
            None => Vec::new(),
        };
        caps.sort();
        (owner, caps)
    }

    /// A real endpoint id, derived from a keypair like a running
    /// session's, so the tests exercise the same parse production does.
    fn peer_id(seed: u8) -> String {
        iroh::SecretKey::from_bytes(&[seed; 32])
            .public()
            .to_string()
    }

    /// The handle presence frames identify `peer` by.
    fn handle_of(peer: &str) -> u64 {
        crate::network::behaviour::peer_liveness_handle(
            &peer.parse::<iroh::EndpointId>().expect("a peer id"),
        )
    }

    /// A peer the inviter named is shown under that name, addressed by
    /// the handle a caret arrives under.
    #[test]
    fn the_inviter_names_a_peer_and_the_room_answers_to_its_handle() {
        let (mut host, _ws, room) = host_owning_a_room();
        let xaver = peer_id(9);

        assert!(
            host.name_peer(room, &ROOM_OWNER.to_string(), &xaver, "Xaver"),
            "the room's administrator may name a peer in it",
        );

        assert_eq!(
            host.room_peer_names_in_doc(room)
                .get(&xaver)
                .map(String::as_str),
            Some("Xaver"),
            "the name has to be in the room document, which is what replicates",
        );
        assert_eq!(
            host.room_peer_labels(room)
                .get(&handle_of(&xaver))
                .map(String::as_str),
            Some("Xaver"),
            "and it has to be reachable from the handle a presence frame carries",
        );
    }

    /// A host that has just restarted still knows who it invited — room
    /// documents aren't persisted, but the name itself *is*, so it's
    /// reachable before the first sync round.
    #[test]
    fn a_host_that_restarted_still_knows_who_it_invited() {
        let (mut host, _ws, room) = host_owning_a_room();
        let xaver = peer_id(9);
        assert!(host.name_peer(room, &ROOM_OWNER.to_string(), &xaver, "Xaver"));

        // What a restart leaves: the cache, and no projected document.
        host.room_docs.clear();

        assert_eq!(
            host.room_peer_labels(room)
                .get(&handle_of(&xaver))
                .map(String::as_str),
            Some("Xaver"),
            "the machine that governs the room is the one the document is \
             projected from, so its cache is the same authority",
        );
    }

    /// A guest gets no such shortcut — what it knows is what replicated;
    /// a name in its cache with none in the document was never entitled.
    #[test]
    fn a_guest_reads_names_only_out_of_the_replicated_document() {
        let (mut host, mut guest, room) = two_in_a_room();
        let xaver = peer_id(9);
        assert!(host.name_peer(room, &"quinten".to_string(), &xaver, "Xaver"));
        ship(&mut host, &mut guest, room, 1);
        assert!(
            guest
                .room_peer_labels(room)
                .contains_key(&handle_of(&xaver))
        );

        // A name the guest holds locally and the room does not.
        guest
            .room_peer_names
            .entry(room)
            .or_default()
            .insert(peer_id(11), "Somebody Else".to_string());

        let labels = guest.room_peer_labels(room);
        assert!(
            !labels.values().any(|n| n == "Somebody Else"),
            "a guest must not show a name that is not in the room document",
        );
        assert_eq!(
            labels.get(&handle_of(&xaver)).map(String::as_str),
            Some("Xaver")
        );
    }

    /// A peer nobody named is left as the id it arrived under — an
    /// unnamed caret is honest, an invented name is not.
    #[test]
    fn a_peer_the_room_has_not_named_gets_no_label_at_all() {
        let (mut host, _ws, room) = host_owning_a_room();
        let named = peer_id(9);
        let stranger = peer_id(10);
        assert!(host.name_peer(room, &ROOM_OWNER.to_string(), &named, "Xaver"));

        let labels = host.room_peer_labels(room);
        assert!(
            !labels.contains_key(&handle_of(&stranger)),
            "a peer nobody named must have no label, so the caret keeps its id",
        );
        assert_eq!(
            labels.len(),
            1,
            "naming one peer must not manufacture a label for any other",
        );
    }

    /// The name is a fact about the room, so it travels with the room —
    /// to a peer that never wrote it and cannot have made it up.
    #[test]
    fn the_name_the_inviter_gave_reaches_a_peer_that_did_not_write_it() {
        let (mut host, mut guest, room) = two_in_a_room();
        let xaver = peer_id(9);
        // `two_in_a_room` makes the host the owner of its own workspace;
        // it is the machine that minted the room, so it governs it.
        assert!(host.name_peer(room, &"quinten".to_string(), &xaver, "Xaver"));

        ship(&mut host, &mut guest, room, 1);

        assert_eq!(
            guest
                .room_peer_names_in_doc(room)
                .get(&xaver)
                .map(String::as_str),
            Some("Xaver"),
            "a guest reads names out of the replicated room document",
        );
        assert_eq!(
            guest
                .room_peer_labels(room)
                .get(&handle_of(&xaver))
                .map(String::as_str),
            Some("Xaver"),
        );
    }

    /// Taking a wrong name back has to reach the peers that already hold
    /// it, or a mislabelled caret is mislabelled forever.
    #[test]
    fn un_naming_a_peer_reaches_the_peer_that_held_the_name() {
        let (mut host, mut guest, room) = two_in_a_room();
        let xaver = peer_id(9);
        assert!(host.name_peer(room, &"quinten".to_string(), &xaver, "Xaver"));
        ship(&mut host, &mut guest, room, 1);
        assert!(guest.room_peer_names_in_doc(room).contains_key(&xaver));

        assert!(
            host.name_peer(room, &"quinten".to_string(), &xaver, ""),
            "an empty name un-names the peer",
        );
        ship(&mut host, &mut guest, room, 1);

        assert!(
            !guest.room_peer_names_in_doc(room).contains_key(&xaver),
            "the retraction has to replicate, not just the name",
        );
    }

    /// The whole reason the name lives in governance — a guest able to
    /// write this map could put any name over any caret in the room.
    #[test]
    fn a_guest_cannot_name_a_peer_in_the_room_it_joined() {
        let (mut host, _ws, room) = host_owning_a_room();
        let xaver = peer_id(9);
        host.name_peer(room, &ROOM_OWNER.to_string(), &xaver, "Xaver");
        host.sync_room_doc(room);

        // A guest whose own state says it governs the room, projecting
        // a peers map that renames the host's invitee.
        let mut guest = WorkspaceTree::default();
        let guest_ws = guest.append_empty_workspace("Mine");
        assert!(guest.set_room(guest_ws, room));
        guest.room_authority.insert(room, RoomAuthority::Local);
        {
            let ws = guest
                .workspaces
                .iter_mut()
                .find(|w| w.id == guest_ws)
                .expect("the guest's own workspace");
            ws.set_owner(ROOM_GUEST.to_string());
        }
        assert!(guest.name_peer(room, &ROOM_GUEST.to_string(), &xaver, "Quinten"));
        let claim = guest
            .export_room_changes_since(room, &[])
            .expect("the guest is in the room it exports");

        host.apply_room_changes(room, &claim, &PeerOrigin::hub(2))
            .expect("well-formed change blobs parse");

        assert_eq!(
            host.room_peer_names_in_doc(room)
                .get(&xaver)
                .map(String::as_str),
            Some("Xaver"),
            "a guest must not be able to rename anyone in a room it joined",
        );
    }

    /// Naming somebody is a governance act, so it needs the same
    /// authority a grant needs — on the machine and on the person.
    #[test]
    fn naming_a_peer_needs_authority_over_the_room() {
        let (mut host, _ws, room) = host_owning_a_room();
        let xaver = peer_id(9);

        assert!(
            !host.name_peer(room, &ROOM_GUEST.to_string(), &xaver, "Xaver"),
            "somebody who does not administer the room may not name in it",
        );
        assert!(
            host.room_peer_names_in_doc(room).is_empty(),
            "a refused naming must write nothing",
        );

        // And a machine that does not govern the room may not either,
        // however its own workspace reads.
        let mut joiner = WorkspaceTree::default();
        let joiner_ws = joiner.append_empty_workspace("Mine");
        assert!(joiner.set_room(joiner_ws, room));
        joiner.adopt_ownerless_workspaces(&ROOM_GUEST.to_string());
        assert!(
            !joiner.name_peer(room, &ROOM_GUEST.to_string(), &xaver, "Xaver"),
            "a machine that joined the room does not get to name its peers",
        );
    }

    /// A key that isn't a peer id names nobody — storing one would put
    /// a row no presence frame can ever match, reading later as a name
    /// that failed to replicate rather than one never addressed to anyone.
    #[test]
    fn a_name_has_to_be_attached_to_a_real_peer_id() {
        let (mut host, _ws, room) = host_owning_a_room();
        assert!(!host.name_peer(room, &ROOM_OWNER.to_string(), "xaver", "Xaver"));
        assert!(!host.name_peer(room, &ROOM_OWNER.to_string(), "", "Xaver"));
        assert!(host.room_peer_names_in_doc(room).is_empty());
    }

    /// A guest projects its own cache into a room it was invited to; the
    /// owner and capability list on the **host** must stay exactly what
    /// the host granted. Two orderings, since whether the host had
    /// already projected decides whether the guest's write races on
    /// actor-id sort order — 32 rounds so a hole can't get lucky once.
    #[test]
    fn a_guest_cannot_rewrite_the_governance_of_the_room_it_joined() {
        for host_projected_first in [false, true] {
            for round in 0..32 {
                let (mut host, host_ws, room) = host_owning_a_room();
                if host_projected_first {
                    host.sync_room_doc(room);
                }
                let claim = governance_claim_from(ROOM_GUEST, room);

                host.apply_room_changes(room, &claim, &PeerOrigin::hub(2))
                    .expect("well-formed change blobs parse");

                // The receiver's cache: who the host believes administers
                // its own room, and what it believes is granted in it.
                let ws = host
                    .workspaces
                    .iter()
                    .find(|w| w.id == host_ws)
                    .expect("the host keeps its workspace");
                assert_eq!(
                    ws.owner.as_deref(),
                    Some(ROOM_OWNER),
                    "round {round} (host projected first: {host_projected_first}): \
                     a guest's projection took ownership of the host's room",
                );
                assert!(
                    ws.may_administer(&ROOM_OWNER.to_string()),
                    "round {round}: the owner must still administer its own room",
                );
                assert!(
                    !ws.may_administer(&ROOM_GUEST.to_string()),
                    "round {round}: the guest must not have become an \
                     administrator of the host's room",
                );
                let cap_ids: Vec<&str> = ws.capabilities.iter().map(|c| c.id.as_str()).collect();
                assert!(
                    cap_ids.contains(&"owners-own-grant"),
                    "round {round}: the owner's own grant was replaced by the \
                     guest's list; held {cap_ids:?}",
                );
                assert!(
                    !cap_ids.contains(&"rogue-grant"),
                    "round {round}: a grant the guest wrote entered the host's \
                     capability list; held {cap_ids:?}",
                );

                // And the same read off the receiver's own copy of the
                // replicated document, which is what it will hand to the
                // next peer that asks.
                let (doc_owner, doc_caps) = governance_in_room_doc(&host, room);
                assert_ne!(
                    doc_owner.as_deref(),
                    Some(ROOM_GUEST),
                    "round {round}: the host's own room document records the \
                     guest as owner",
                );
                assert!(
                    !doc_caps.iter().any(|c| c == "rogue-grant"),
                    "round {round}: the guest's grant is in the host's own room \
                     document; it holds {doc_caps:?}",
                );
            }
        }
    }

    /// The same forgery between two guests — neither administers the
    /// room, so the receiver must know *which* peer governance comes
    /// from and refuse the rest.
    #[test]
    fn one_guest_cannot_rewrite_the_governance_another_guest_learned() {
        for round in 0..32 {
            let (mut host, _host_ws, room) = host_owning_a_room();
            let legitimate = host
                .export_room_changes_since(room, &[])
                .expect("the host is in its own room");

            // Carol joins and learns the room from the host it was
            // invited by. `PeerOrigin::hub(1)` stands for that host.
            let mut carol = WorkspaceTree::default();
            let carol_ws = carol.append_empty_workspace("Mine");
            assert!(carol.set_room(carol_ws, room), "carol joins the room");
            carol
                .room_authority
                .insert(room, RoomAuthority::Peer(PeerOrigin::hub(1)));
            carol
                .apply_room_changes(room, &legitimate, &PeerOrigin::hub(1))
                .expect("the host's document parses");
            assert_eq!(
                carol
                    .workspaces
                    .iter()
                    .find(|w| w.id == carol_ws)
                    .and_then(|w| w.owner.clone())
                    .as_deref(),
                Some(ROOM_OWNER),
                "round {round}: carol must learn the room's owner from the \
                 peer that invited her — without that the refusal below \
                 would be vacuous",
            );

            // Bob, in the same room, claims it.
            let claim = governance_claim_from(ROOM_GUEST, room);
            carol
                .apply_room_changes(room, &claim, &PeerOrigin::hub(2))
                .expect("well-formed change blobs parse");

            let ws = carol
                .workspaces
                .iter()
                .find(|w| w.id == carol_ws)
                .expect("carol keeps her workspace");
            assert_eq!(
                ws.owner.as_deref(),
                Some(ROOM_OWNER),
                "round {round}: a guest rewrote another guest's copy of the \
                 room's owner",
            );
            let cap_ids: Vec<&str> = ws.capabilities.iter().map(|c| c.id.as_str()).collect();
            assert!(
                !cap_ids.contains(&"rogue-grant"),
                "round {round}: a guest's grant entered another guest's \
                 capability list; held {cap_ids:?}",
            );
            let (doc_owner, doc_caps) = governance_in_room_doc(&carol, room);
            assert_eq!(
                doc_owner.as_deref(),
                Some(ROOM_OWNER),
                "round {round}: carol's own room document records the wrong \
                 owner",
            );
            assert!(
                !doc_caps.iter().any(|c| c == "rogue-grant"),
                "round {round}: carol's own room document carries the rogue \
                 grant; it holds {doc_caps:?}",
            );
        }
    }

    /// The other half: a guest joining a room **must** learn its owner
    /// and capabilities from the peer that admitted it — refusing every
    /// change would close collaboration along with the hole.
    #[test]
    fn a_guest_learns_the_governance_of_the_room_that_invited_it() {
        let (mut host, _host_ws, room) = host_owning_a_room();
        let from_host = host
            .export_room_changes_since(room, &[])
            .expect("the host is in its own room");

        let mut guest = WorkspaceTree::default();
        let guest_ws = guest.append_empty_workspace("Mine");
        assert!(guest.set_room(guest_ws, room), "the guest joins the room");
        guest
            .room_authority
            .insert(room, RoomAuthority::Peer(PeerOrigin::hub(1)));

        let changed = guest
            .apply_room_changes(room, &from_host, &PeerOrigin::hub(1))
            .expect("the host's document parses");
        assert!(changed, "the round must change the guest's copy");

        let ws = guest
            .workspaces
            .iter()
            .find(|w| w.id == guest_ws)
            .expect("the guest keeps its workspace");
        assert_eq!(
            ws.owner.as_deref(),
            Some(ROOM_OWNER),
            "the guest must learn who administers the room it joined",
        );
        assert!(
            ws.capabilities.iter().any(|c| c.id == "owners-own-grant"),
            "the guest must learn the room's capabilities; it holds {:?}",
            ws.capabilities.iter().map(|c| &c.id).collect::<Vec<_>>(),
        );
        assert!(
            !ws.may_administer(&ROOM_GUEST.to_string()),
            "and joining a room must not make the joiner its administrator",
        );
    }

    /// The local grant path has to agree with the replication path: a
    /// grant in a room this machine doesn't administer would be
    /// honoured by its own sandbox while the room never heard of it —
    /// a local escalation dressed as a grant.
    #[test]
    fn a_machine_cannot_grant_in_a_room_it_does_not_administer() {
        let (_host, _host_ws, room) = host_owning_a_room();

        let mut guest = WorkspaceTree::default();
        let guest_ws = guest.append_empty_workspace("Mine");
        assert!(guest.set_room(guest_ws, room), "the guest joins the room");
        // A cache that still names the local user as owner — what an
        // upgrade from a state file written before joining looks like. The
        // ownership check alone would say yes here.
        guest
            .workspaces
            .iter_mut()
            .find(|w| w.id == guest_ws)
            .expect("the guest's workspace")
            .set_owner(ROOM_GUEST.to_string());
        assert!(
            guest
                .workspaces
                .iter()
                .find(|w| w.id == guest_ws)
                .expect("the guest's workspace")
                .may_administer(&ROOM_GUEST.to_string()),
            "precondition: the cache says the guest administers it, so the \
             refusal below is the room's write authority and not ownership",
        );

        let granted = guest.grant_in_workspace(
            guest_ws,
            &ROOM_GUEST.to_string(),
            CapOverride::grant(
                Subject::app("rogue"),
                Scope::FsPath(ROGUE_GRANT_PATH.to_string()),
                Verb::Write,
            ),
        );
        assert!(
            granted.is_none(),
            "a grant in a room administered elsewhere must be refused",
        );
        assert!(
            !guest
                .workspaces
                .iter()
                .find(|w| w.id == guest_ws)
                .expect("the guest's workspace")
                .capabilities
                .iter()
                .any(|c| matches!(&c.scope, Scope::FsPath(p) if p == ROGUE_GRANT_PATH)),
            "and must record nothing",
        );

        // The control: the same call in a room this machine minted is
        // allowed, so the refusal above is about the room and not the call.
        let mut own = WorkspaceTree::default();
        let own_ws = own.append_empty_workspace("Mine");
        own.adopt_ownerless_workspaces(&ROOM_GUEST.to_string());
        assert!(
            own.grant_in_workspace(
                own_ws,
                &ROOM_GUEST.to_string(),
                CapOverride::grant(
                    Subject::app("files"),
                    Scope::FsPath(OWNER_GRANT_PATH.to_string()),
                    Verb::Read,
                ),
            )
            .is_some(),
            "granting in a room this machine administers must still work",
        );
    }

    /// "Join the unminted room" is never a meaningful request, and
    /// honouring it would drop a workspace out of collaboration with no
    /// signal at all.
    #[test]
    fn set_room_refuses_the_nil_room() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let id = tree.workspaces[0].id;
        let before = tree.room_of(id).expect("bootstrap mints a room");
        assert!(
            !tree.set_room(id, RoomId::nil()),
            "setting the nil room must be refused"
        );
        assert_eq!(
            tree.room_of(id),
            Some(before),
            "and must leave the workspace's real room in place"
        );
    }

    #[test]
    fn set_active_propagates_via_apply_changes() {
        // Tree B uses `default()`, same reason as the append test: two
        // `bootstrap_default` calls would race on the workspaces list.
        let mut tree_a = WorkspaceTree::bootstrap_demo();
        let target = tree_a
            .workspaces
            .iter()
            .find(|w| w.id != tree_a.active)
            .map(|w| w.id)
            .expect("bootstrap seeds >=2 workspaces");
        tree_a.set_active(target);
        assert_eq!(tree_a.active, target);

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");
        assert_eq!(
            tree_b.active, target,
            "tree B's active should match peer A's after sync"
        );
    }

    #[test]
    fn maximize_toggle_propagates_via_apply_changes() {
        // Tree B starts from `default()`, same reason as the append test.
        let mut tree_a = WorkspaceTree::bootstrap_demo();
        let target_group = tree_a.workspaces[0].strips[0].window_groups[0].id;
        assert!(
            !tree_a.workspaces[0].strips[0].window_groups[0].maximized,
            "bootstrap_default seeds the first group un-maximized"
        );

        // Mirror to both sides — production calls
        // `harvest_pending_maximize_changes`, which writes cache then
        // doc; in the test we replicate that pattern directly.
        tree_a.workspaces[0].strips[0].window_groups[0].maximized = true;
        tree_a.doc_set_maximized(target_group, true);

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        let target = tree_b
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == target_group);
        assert!(
            target.is_some(),
            "tree B should observe the window group after sync"
        );
        assert!(
            target.unwrap().maximized,
            "maximize flip should propagate; got {target:?}"
        );
    }

    #[test]
    fn width_change_propagates_via_apply_changes() {
        // A share, not a pixel count: B's monitor isn't A's, so
        // replicating pixels would reproduce A's screen on B's.
        let mut tree_a = WorkspaceTree::bootstrap_demo();
        let target_group = tree_a.workspaces[0].strips[0].window_groups[0].id;
        let new_share: u16 = 600;

        tree_a.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Fraction(new_share);
        tree_a.doc_set_width(target_group, new_share);

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        let target = tree_b
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.id == target_group);
        assert!(
            target.is_some(),
            "tree B should observe the window group after sync"
        );
        assert_eq!(
            target.unwrap().width,
            WindowWidth::Fraction(new_share),
            "the width share should round-trip via doc"
        );
        // And that share means the same proportion of whatever display B
        // has, which is the property a pixel count could not carry.
        assert_eq!(target.unwrap().width.resolve(1600.0), 960.0);
        assert_eq!(target.unwrap().width.resolve(3840.0), 2304.0);
    }

    #[test]
    fn moving_a_tab_across_strips_propagates_via_apply_changes() {
        let mut tree_a = WorkspaceTree::bootstrap_demo();
        // Add a second strip with an empty group so the move has
        // somewhere to land; re-seed so the doc carries it too.
        let dst_strip_id = StripId::new();
        let dst_group_id = next_id();
        tree_a.workspaces[0].strips.push(Strip {
            id: dst_strip_id,
            name: "Dest".into(),
            window_groups: vec![WindowGroup {
                id: dst_group_id,
                tabs: Vec::new(),
                active_tab: 0,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        });
        tree_a.seed_doc_from_cache();

        let src_strip_id = tree_a.workspaces[0].strips[0].id;
        let target_tab = tree_a.workspaces[0].strips[0].window_groups[0].tabs[0].id;

        assert!(tree_a.move_tab_across_strips(src_strip_id, dst_strip_id, target_tab));

        let changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&changes).expect("apply changes");

        let dst_strip_in_b = tree_b
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .find(|s| s.id == dst_strip_id);
        assert!(
            dst_strip_in_b.is_some(),
            "destination strip must be observable in tree B"
        );
        let landed_in_dst = dst_strip_in_b
            .unwrap()
            .window_groups
            .iter()
            .flat_map(|g| g.tabs.iter())
            .any(|t| t.id == target_tab);
        assert!(
            landed_in_dst,
            "moved tab must appear in destination strip after sync"
        );
    }
}

#[cfg(test)]
mod collaborator_tests {
    use super::*;

    #[test]
    fn new_initializes_all_concept_fields_to_none() {
        let c = Collaborator::new("alice", "Alice");
        assert_eq!(c.id, "alice");
        assert_eq!(c.name, "Alice");
        assert_eq!(c.active_tab, None);
        assert_eq!(c.cursor_color, None);
        assert_eq!(c.avatar, None);
        assert_eq!(c.role_id, None);
    }

    #[test]
    fn with_active_tab_delegates_to_new_and_sets_tab() {
        let c = Collaborator::with_active_tab("bob", "Bob", 42);
        assert_eq!(c.active_tab, Some(42));
        // Other concept fields stay at their defaults.
        assert_eq!(c.cursor_color, None);
        assert_eq!(c.avatar, None);
        assert_eq!(c.role_id, None);
    }

    #[test]
    fn with_cursor_color_sets_hex_override() {
        let c = Collaborator::new("alice", "Alice").with_cursor_color("#ff00aa");
        assert_eq!(c.cursor_color.as_deref(), Some("#ff00aa"));
    }

    #[test]
    fn with_avatar_sets_reference() {
        let c = Collaborator::new("charlie", "Charlie").with_avatar("CC");
        assert_eq!(c.avatar.as_deref(), Some("CC"));
    }

    #[test]
    fn with_role_sets_role_id() {
        let c = Collaborator::new("bob", "Bob").with_role("editor");
        assert_eq!(c.role_id.as_deref(), Some("editor"));
    }

    #[test]
    fn builder_methods_can_be_chained() {
        let c = Collaborator::with_active_tab("me", "Me", 7)
            .with_cursor_color("#112233")
            .with_avatar("ME")
            .with_role("owner");
        assert_eq!(c.active_tab, Some(7));
        assert_eq!(c.cursor_color.as_deref(), Some("#112233"));
        assert_eq!(c.avatar.as_deref(), Some("ME"));
        assert_eq!(c.role_id.as_deref(), Some("owner"));
    }

    #[test]
    fn legacy_json_without_concept_fields_still_deserializes() {
        // Workspaces persisted before the struct grew concept fields
        // omit them; `#[serde(default)]` keeps those payloads loadable.
        let legacy = r#"{"id":"alice","name":"Alice","active_tab":null}"#;
        let c: Collaborator = serde_json::from_str(legacy).unwrap();
        assert_eq!(c.id, "alice");
        assert_eq!(c.name, "Alice");
        assert_eq!(c.active_tab, None);
        assert_eq!(c.cursor_color, None);
        assert_eq!(c.avatar, None);
        assert_eq!(c.role_id, None);
        // Present, not invited — defaulting the other way would redraw
        // every already-saved participant as someone who never showed.
        assert!(!c.invited);
    }

    #[test]
    fn fully_populated_round_trip() {
        let c = Collaborator::with_active_tab("alice", "Alice", 3)
            .with_cursor_color("#aabbcc")
            .with_avatar("A")
            .with_role("editor");
        let json = serde_json::to_string(&c).unwrap();
        let back: Collaborator = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}

#[cfg(test)]
mod active_group_tests {
    //! Coverage for the active-group pointer + close-fade reconciler.
    //! Tests drive it directly with a hand-built tree and the
    //! closing-group set the shell would harvest from widgets, keeping
    //! the data-layer contract free of the GPU dependency live widgets
    //! would add.

    use super::*;

    /// Build a single-workspace, single-strip tree carrying `n` empty
    /// window groups with monotonic ids `1..=n`. Returns the strip id
    /// alongside the tree so callers can drive the reconciler directly.
    fn tree_with_n_groups(n: u64) -> (WorkspaceTree, StripId, Vec<GroupId>) {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_ids: Vec<GroupId> = (1..=n).collect();
        let strip = Strip {
            id: strip_id,
            name: "Main".into(),
            window_groups: group_ids
                .iter()
                .map(|gid| WindowGroup {
                    id: *gid,
                    tabs: vec![],
                    active_tab: 0,
                    width: WindowWidth::Normal,
                    maximized: false,
                })
                .collect(),
        };
        let ws = Workspace {
            id: ws_id,
            room: RoomId::new(),
            name: "Test".into(),
            strips: vec![strip],
            collaborators: vec![],
            owner: None,
            capabilities: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            shared_surfaces: SharedSurfaces::default(),
        };
        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        let tree = WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group: HashMap::new(),
            collaborators: vec![],
            ..Default::default()
        };
        (tree, strip_id, group_ids)
    }

    #[test]
    fn active_group_advances_when_active_group_starts_closing() {
        // Two groups [A, B] with A active. When A starts closing, the
        // pointer must immediately advance to B so focus / keyboard /
        // presence stop targeting the fading-away group.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, a);
        assert_eq!(tree.active_group_for(strip_id), Some(a));

        let landed = tree.nudge_active_group_off_closing(strip_id, &[a]);

        assert_eq!(landed, Some(b));
        assert_eq!(
            tree.active_group_for(strip_id),
            Some(b),
            "active-group pointer must advance to the next surviving sibling at fade-start, not after the 180 ms fade",
        );
    }

    #[test]
    fn active_group_advances_to_previous_when_last_group_starts_closing() {
        // B (rightmost) is closing with no survivor to its right, so
        // the reconciler falls back to A on the left.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, b);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[b]);

        assert_eq!(landed, Some(a));
        assert_eq!(tree.active_group_for(strip_id), Some(a));
    }

    #[test]
    fn active_group_doesnt_advance_when_non_active_group_closes() {
        // B closes while A is active — a stray reconcile pass on a
        // group not itself closing is a no-op, so the shell can call
        // it freely from a per-tick loop.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, a);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[b]);

        assert_eq!(landed, Some(a));
        assert_eq!(tree.active_group_for(strip_id), Some(a));
    }

    #[test]
    fn active_group_clears_when_every_group_is_closing() {
        // Every group in the strip is closing — the pointer can't land
        // anywhere, so the entry is dropped rather than clamped to a
        // closing-group id.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, b) = (ids[0], ids[1]);
        tree.set_active_group(strip_id, a);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[a, b]);

        assert_eq!(landed, None);
        assert_eq!(tree.active_group_for(strip_id), None);
    }

    #[test]
    fn nudge_is_noop_when_strip_has_no_recorded_pointer() {
        // No call to `set_active_group` first. The reconciler must
        // return `None` and not insert a phantom entry after the
        // first close.
        let (mut tree, strip_id, ids) = tree_with_n_groups(2);
        let (a, _b) = (ids[0], ids[1]);
        let landed = tree.nudge_active_group_off_closing(strip_id, &[a]);
        assert_eq!(landed, None);
        assert_eq!(tree.active_group_for(strip_id), None);
    }

    #[test]
    fn resolve_active_group_returns_recorded_when_not_closing() {
        // Pure read path: with the recorded pointer not in the closing
        // set, `resolve_active_group` returns it unchanged and, unlike
        // `nudge_*`, doesn't mutate the map.
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        tree.set_active_group(strip_id, b);
        let resolved = tree.resolve_active_group(strip_id, |id| id == a || id == c);
        assert_eq!(resolved, Some(b));
        // Map is unchanged.
        assert_eq!(tree.active_group_for(strip_id), Some(b));
    }

    #[test]
    fn resolve_active_group_walks_right_then_left() {
        // Three groups [A, B, C] with B active and closing. The
        // reconciler must prefer C (right) over A (left) so focus
        // moves "forward" rather than ping-ponging back to the start.
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        let (_a, b, c) = (ids[0], ids[1], ids[2]);
        tree.set_active_group(strip_id, b);

        let landed = tree.nudge_active_group_off_closing(strip_id, &[b]);

        assert_eq!(landed, Some(c));
    }

    #[test]
    fn legacy_json_without_active_group_field_still_deserializes() {
        // `#[serde(default)]` keeps the upgrade path lossless.
        let legacy = r#"{
            "workspaces": [],
            "active": 0,
            "active_strip": {},
            "collaborators": []
        }"#;
        let tree: WorkspaceTree = serde_json::from_str(legacy).unwrap();
        assert!(tree.active_group.is_empty());
    }

    #[test]
    fn window_group_maximized_round_trips_through_json() {
        // Window state must survive restart: a fresh tree starts
        // un-maximized, and flipping `maximized = true` on one group
        // must serialize and deserialize cleanly.
        let mut tree = WorkspaceTree::bootstrap_demo();
        let ws = tree.workspaces.first_mut().expect("seed has workspaces");
        let strip = ws.strips.first_mut().expect("seed has strips");
        let group = strip.window_groups.first_mut().expect("seed has groups");
        assert!(!group.maximized);
        group.maximized = true;

        let json = serde_json::to_string(&tree).unwrap();
        let round: WorkspaceTree = serde_json::from_str(&json).unwrap();
        assert!(round.workspaces[0].strips[0].window_groups[0].maximized);
    }

    #[test]
    fn legacy_json_without_maximized_field_defaults_to_false() {
        // `#[serde(default)]` covers persisted trees written before the
        // `maximized` field existed. Stripped-down window-group payload
        // omitting the field must load.
        let legacy = r#"{
            "id": 1,
            "tabs": [],
            "active_tab": 0,
            "width": "Normal"
        }"#;
        let g: WindowGroup = serde_json::from_str(legacy).unwrap();
        assert!(!g.maximized);
    }

    fn group_order(tree: &WorkspaceTree, strip_id: StripId) -> Vec<u64> {
        tree.workspaces[0]
            .strips
            .iter()
            .find(|s| s.id == strip_id)
            .unwrap()
            .window_groups
            .iter()
            .map(|g| g.id)
            .collect()
    }

    /// Dragging a tab to a new slot must persist into the tree and
    /// mirror into the doc, or the reorder resets on the next restart.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn reorder_tab_within_group_persists_and_mirrors_into_the_doc() {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};

        // One group (id 1) carrying three tabs: [10, 11, 12].
        let (mut tree, _strip_id, ids) = tree_with_n_groups(1);
        let group_id = ids[0];
        let mk = |id: u64, name: &str| Tab {
            id,
            title: name.into(),
            tile: TileContent::Stub {
                kind: AppKind::Terminal,
                label: name.into(),
            },
        };
        {
            let g = &mut tree.workspaces[0].strips[0].window_groups[0];
            g.tabs = vec![mk(10, "A"), mk(11, "B"), mk(12, "C")];
            g.active_tab = 10;
        }
        tree.seed_doc_from_cache();

        let tab_order = |t: &WorkspaceTree| -> Vec<u64> {
            t.workspaces[0].strips[0].window_groups[0]
                .tabs
                .iter()
                .map(|tb| tb.id)
                .collect()
        };
        assert_eq!(tab_order(&tree), vec![10, 11, 12]);

        // Drag the first tab to the last slot → [11, 12, 10].
        let changed = tree.reorder_tab_within_group(group_id, 0, 2);
        assert!(changed, "moving a tab to a new slot reports a change");
        assert_eq!(tab_order(&tree), vec![11, 12, 10], "cache order updated");

        // Same slot is a no-op; unknown group is a no-op.
        assert!(!tree.reorder_tab_within_group(group_id, 1, 1));
        assert!(!tree.reorder_tab_within_group(999, 0, 1));
        assert_eq!(tab_order(&tree), vec![11, 12, 10]);

        // The doc must carry the new order too, so peers/persistence agree.
        let (_, workspaces_id) = tree.doc.get(&ROOT, "workspaces").unwrap().unwrap();
        let (_, ws0) = tree.doc.get(&workspaces_id, 0_usize).unwrap().unwrap();
        let (_, strips_id) = tree.doc.get(&ws0, "strips").unwrap().unwrap();
        let (_, strip0) = tree.doc.get(&strips_id, 0_usize).unwrap().unwrap();
        let (_, groups_id) = tree.doc.get(&strip0, "window_groups").unwrap().unwrap();
        let (_, g0) = tree.doc.get(&groups_id, 0_usize).unwrap().unwrap();
        let (_, tabs_id) = tree.doc.get(&g0, "tabs").unwrap().unwrap();
        let mut doc_order = Vec::new();
        for i in 0..tree.doc.length(&tabs_id) {
            let (_, t) = tree.doc.get(&tabs_id, i).unwrap().unwrap();
            if let Some((Value::Scalar(s), _)) = tree.doc.get(&t, "id").unwrap()
                && let ScalarValue::Uint(id) = s.as_ref()
            {
                doc_order.push(*id);
            }
        }
        assert_eq!(doc_order, vec![11, 12, 10], "doc mirrors the new tab order");
    }

    #[test]
    fn reorder_group_moves_group_to_dropped_index() {
        // Strip [1, 2, 3]. Drag group 1 to index 2 → [2, 3, 1].
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        assert_eq!(group_order(&tree, strip_id), vec![1, 2, 3]);
        let changed = tree.reorder_group_within_strip(strip_id, ids[0], 2);
        assert!(changed, "moving group 1 to a new slot must report a change");
        assert_eq!(
            group_order(&tree, strip_id),
            vec![2, 3, 1],
            "group 1 must land at the dropped index, neighbours shift left",
        );
    }

    #[test]
    fn reorder_group_forward_and_backward() {
        let (mut tree, strip_id, ids) = tree_with_n_groups(4); // [1,2,3,4]
        // Move last group (4) to the front.
        assert!(tree.reorder_group_within_strip(strip_id, ids[3], 0));
        assert_eq!(group_order(&tree, strip_id), vec![4, 1, 2, 3]);
        // Move it back to the end (index >= len clamps to last slot).
        assert!(tree.reorder_group_within_strip(strip_id, 4, 99));
        assert_eq!(group_order(&tree, strip_id), vec![1, 2, 3, 4]);
    }

    #[test]
    fn reorder_group_to_same_index_is_a_noop() {
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        let changed = tree.reorder_group_within_strip(strip_id, ids[1], 1);
        assert!(!changed, "moving a group to its own slot reports no change");
        assert_eq!(group_order(&tree, strip_id), vec![1, 2, 3]);
    }

    #[test]
    fn reorder_group_unknown_group_is_a_noop() {
        let (mut tree, strip_id, _ids) = tree_with_n_groups(2);
        assert!(!tree.reorder_group_within_strip(strip_id, 999, 0));
        assert_eq!(group_order(&tree, strip_id), vec![1, 2]);
    }

    #[test]
    fn reorder_group_mirrors_into_the_seeded_doc() {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value};

        // With the doc seeded from the cache, a reorder must also reshuffle
        // the doc's `window_groups` list so peer-sync ships the new order.
        let (mut tree, strip_id, ids) = tree_with_n_groups(3);
        tree.seed_doc_from_cache();
        tree.reorder_group_within_strip(strip_id, ids[0], 2); // [2,3,1]

        let (_, workspaces_id) = tree.doc.get(&ROOT, "workspaces").unwrap().unwrap();
        let (_, ws0) = tree.doc.get(&workspaces_id, 0_usize).unwrap().unwrap();
        let (_, strips_map_id) = tree.doc.get(&ws0, "strips").unwrap().unwrap();
        let (_, strip0) = tree.doc.get(&strips_map_id, 0_usize).unwrap().unwrap();
        let (_, groups_id) = tree.doc.get(&strip0, "window_groups").unwrap().unwrap();
        let mut doc_order = Vec::new();
        for i in 0..tree.doc.length(&groups_id) {
            let (_, g) = tree.doc.get(&groups_id, i).unwrap().unwrap();
            if let Some((Value::Scalar(s), _)) = tree.doc.get(&g, "id").unwrap()
                && let ScalarValue::Uint(id) = s.as_ref()
            {
                doc_order.push(*id);
            }
        }
        assert_eq!(
            doc_order,
            vec![2, 3, 1],
            "the doc's window_groups list must mirror the cache reorder",
        );
    }
}

#[cfg(test)]
mod active_workspace_peer_count_tests {
    //! Coverage for [`active_workspace_peer_count`] — the shared helper
    //! that keeps the top-bar avatar stack and the wheel-disc active-card
    //! collaborator-dot count in lock-step.

    use super::*;
    use crate::shell::sidebar_wheel_chrome::SidebarWheelChrome;
    use crate::shell::topbar_chrome::TopBarChrome;
    use std::collections::HashMap;

    /// Build a single-workspace tree with the given collaborators on the
    /// active workspace. The tree-level roster is left empty so this
    /// isolates the per-workspace path from the fallback path.
    fn tree_with_active_collaborators(collabs: Vec<Collaborator>) -> WorkspaceTree {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let ws = Workspace {
            id: ws_id,
            room: RoomId::new(),
            name: "Active".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }],
            collaborators: collabs,
            owner: None,
            capabilities: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            shared_surfaces: SharedSurfaces::default(),
        };
        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group: HashMap::new(),
            collaborators: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn topbar_and_wheel_disc_agree_on_active_collab_count() {
        // Two explicit collaborators on the active workspace — one is the
        // local user ("me"). Both chromes must agree that the *peer* count
        // (excluding "me") is 1.
        let tree = tree_with_active_collaborators(vec![
            Collaborator::new("me", "Me"),
            Collaborator::new("alice", "Alice"),
        ]);

        let topbar = TopBarChrome::from_tree(&tree);
        let wheel = SidebarWheelChrome::from_tree(&tree);
        let active_idx = tree
            .workspaces
            .iter()
            .position(|w| w.id == tree.active)
            .expect("active workspace present");

        assert_eq!(
            topbar.collaborator_initials().len(),
            wheel.card_collab_count(active_idx),
            "top-bar avatar stack and wheel-disc active card must agree on \
             the peer count (audit B-10); helper is the single source of truth"
        );
        // And both should equal the helper's value.
        assert_eq!(active_workspace_peer_count(&tree), 1);
        assert_eq!(topbar.collaborator_initials().len(), 1);
    }
}

#[cfg(test)]
mod capability_tests {
    //! Coverage for [`Capability`] / [`GrantScope`] / [`FsMode`] and the
    //! [`Workspace::grants`] field. Pure schema tests — no portal
    //! plumbing is exercised here.
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_new_workspace_holds_no_capabilities_and_grants_nothing() {
        // Both `Workspace::default()` (used by `..Default::default()`
        // sweeps) and `Workspace::new(...)` must start empty, and an
        // empty room must deny rather than default-allow.
        for ws in [
            Workspace::default(),
            Workspace::new(WorkspaceId::from(1), "Demo"),
        ] {
            assert!(ws.capabilities.is_empty());
            assert!(ws.app_grants_snapshot().is_empty());
            assert!(!ws.has_app_grants());
            assert!(
                !ws.may_administer(&"anyone".to_string()),
                "a room with no owner has no administrators"
            );
            assert!(
                !ws.allows(
                    &ws.actor_for_app("org.gnome.gedit"),
                    &ResourceRef::FsPath("/home/u/work".into()),
                    Verb::Read
                ),
                "an app with no capability is denied"
            );
        }
    }

    #[test]
    fn app_kind_app_id_is_a_reverse_dns_id_not_the_display_label() {
        // The grant key must be a real app-id, in the same namespace as
        // an adopted client's `set_app_id` — never the display label.
        assert_eq!(AppKind::FileExplorer.app_id(), "org.nirios.Files");
        assert_eq!(AppKind::Terminal.app_id(), "org.nirios.Terminal");
        assert_eq!(AppKind::Browser.app_id(), "org.nirios.Browser");
        assert_eq!(AppKind::Settings.app_id(), "org.nirios.Settings");
        // A bare `Other` label is namespaced under the compositor prefix.
        assert_eq!(
            AppKind::Other("Editor".into()).app_id(),
            "org.nirios.Editor"
        );
        // The app-id is distinct from the display label for every kind.
        for kind in [
            AppKind::FileExplorer,
            AppKind::Terminal,
            AppKind::Browser,
            AppKind::Settings,
            AppKind::Other("Editor".into()),
        ] {
            assert_ne!(
                kind.app_id(),
                kind.as_str(),
                "app_id must not collapse to the display label for {kind:?}"
            );
        }
    }

    #[test]
    fn app_kind_app_id_trusts_reverse_dns_other_labels_verbatim() {
        // An `Other` value already in reverse-DNS form is used as-is,
        // not double-prefixed.
        assert_eq!(
            AppKind::Other("org.gimp.GIMP".into()).app_id(),
            "org.gimp.GIMP"
        );
    }

    #[test]
    fn grants_round_trip_via_serde_json() {
        // Pins the on-disk schema for portal-side consumers.
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.grant_app(
            "org.gnome.gedit",
            &Capability::Filesystem {
                path: PathBuf::from("/home/u/work"),
                mode: FsMode::ReadWrite,
                scope: GRANT_ALWAYS,
            },
        );
        ws.grant_app(
            "org.gnome.gedit",
            &Capability::Network {
                hosts: Some(vec!["github.com".into()]),
                scope: GRANT_SESSION,
            },
        );

        let json = serde_json::to_string(&ws).expect("serialize");
        let round: Workspace = serde_json::from_str(&json).expect("deserialize");

        let view = round.app_grants_snapshot();
        assert_eq!(view.len(), 1);
        let caps = view.get("org.gnome.gedit").expect("app present");
        assert_eq!(caps.len(), 2);
        assert!(caps.contains(&Capability::Filesystem {
            path: PathBuf::from("/home/u/work"),
            mode: FsMode::ReadWrite,
            scope: GRANT_ALWAYS,
        }));
        assert!(caps.contains(&Capability::Network {
            hosts: Some(vec!["github.com".into()]),
            scope: GRANT_SESSION,
        }));

        // And the decision function agrees after the round-trip.
        let app = round.actor_for_app("org.gnome.gedit");
        assert!(round.allows(
            &app,
            &ResourceRef::FsPath("/home/u/work/notes.txt".into()),
            Verb::Write
        ));
        assert!(
            !round.allows(
                &app,
                &ResourceRef::FsPath("/home/u/secrets".into()),
                Verb::Read
            ),
            "a path outside the grant stays denied across serialisation"
        );
        assert!(round.allows(
            &app,
            &ResourceRef::NetHost("github.com".into()),
            Verb::Write
        ));
        assert!(
            !round.allows(
                &app,
                &ResourceRef::NetHost("evil.example".into()),
                Verb::Write
            ),
            "and a host outside the allowlist stays denied"
        );
    }

    #[test]
    fn legacy_workspace_without_grants_field_deserialises() {
        // `#[serde(default)]` keeps the upgrade path lossless.
        let legacy = r#"{
            "id": 1,
            "name": "Old",
            "strips": []
        }"#;
        let ws: Workspace = serde_json::from_str(legacy).expect("legacy compat");
        assert!(ws.capabilities.is_empty());
        assert!(ws.app_grants_snapshot().is_empty());
        assert_eq!(ws.owner, None, "a legacy workspace records no owner");
        assert_eq!(ws.id, WorkspaceId::from(1));
        assert_eq!(ws.name, "Old");
    }

    #[test]
    fn all_capability_variants_round_trip_through_serde() {
        // Pin the wire shape of every variant so a downstream change
        // breaks loudly here rather than silently in the portal backend.
        let caps = vec![
            Capability::Filesystem {
                path: PathBuf::from("/tmp/ws"),
                mode: FsMode::Read,
                scope: GRANT_ONCE,
            },
            Capability::Camera {
                scope: GRANT_SESSION,
            },
            Capability::Microphone {
                scope: GRANT_ALWAYS,
            },
            Capability::Screencast { scope: GRANT_ONCE },
            Capability::Network {
                hosts: None,
                scope: GRANT_ALWAYS,
            },
        ];
        let json = serde_json::to_string(&caps).expect("serialize caps");
        let round: Vec<Capability> = serde_json::from_str(&json).expect("deserialize caps");
        assert_eq!(round, caps);
    }

    #[test]
    fn legacy_workspace_without_shared_surfaces_field_deserialises() {
        // Persisted workspaces before this field existed lack it;
        // `#[serde(default)]` keeps the upgrade path lossless.
        let legacy = r#"{
            "id": 1,
            "name": "Old",
            "strips": [],
            "grants": {}
        }"#;
        let ws: Workspace = serde_json::from_str(legacy).expect("legacy compat");
        assert!(ws.shared_surfaces.is_empty());
        assert_eq!(ws.id, WorkspaceId::from(1));
    }
}

#[cfg(test)]
mod next_id_seed_tests {
    //! [`WorkspaceTree::seed_next_id_from_self`] must advance the
    //! counter past a loaded tree's highest id. The counter is
    //! process-wide, so tests observe `>= max+1`, not an exact value.
    use super::*;

    /// Restores the process-wide peer salt at scope-exit, unconditionally
    /// (RAII), so a test that flips it doesn't leak into others.
    struct SaltGuard(u16);
    impl SaltGuard {
        fn new() -> Self {
            Self(NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed))
        }
    }
    impl Drop for SaltGuard {
        fn drop(&mut self) {
            NEXT_ID_PEER_SALT.store(self.0, AtomicOrdering::Relaxed);
        }
    }

    #[test]
    #[serial_test::serial(next_id_statics)]
    fn next_id_seeds_above_max_used_id_on_load() {
        let _guard = SaltGuard::new();
        NEXT_ID_PEER_SALT.store(0, AtomicOrdering::Relaxed);
        let mut tree = WorkspaceTree::default();
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from(1), "First"));
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from(50), "Fiftieth"));

        tree.seed_next_id_from_self();

        // Mint a fresh id — it must be strictly greater than every
        // persisted id, including the gap-jumping `50`.
        let allocated = next_id();
        assert!(
            allocated > 50,
            "after seeding, next_id() must skip past every persisted \
             id (max was 50); got {allocated} which would collide with the \
             existing tree (D-9 regression)"
        );
    }

    /// A salt of `0` reproduces the exact id shape every fixture that
    /// expects `WorkspaceId(1)` to round-trip depends on.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn peer_salt_zero_default_keeps_old_ids() {
        let _guard = SaltGuard::new();
        // Reset both the salt and the counter so a parallel test that
        // bumped them doesn't leak.
        NEXT_ID_PEER_SALT.store(0, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);

        let id = next_id();
        assert_eq!(
            id, 1,
            "salt-0 default must mint the bare counter; got 0x{id:016x}"
        );
        assert_eq!(peer_salt_of(id), 0);
        assert_eq!(counter_of(id), 1);
    }

    /// After `set_peer_salt(0xABCD)`, the top 16 bits of every minted
    /// id must equal `0xABCD` — sanity check on the bit-packing.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn peer_salt_high_bits_are_the_salt() {
        let _guard = SaltGuard::new();
        let mut tree = WorkspaceTree::default();
        tree.set_peer_salt(0xABCD);

        // Reset the counter so the assertion below sees a small value
        // it can compare cleanly.
        NEXT_ID.store(1, AtomicOrdering::Relaxed);
        let id = next_id();

        assert_eq!(
            peer_salt_of(id),
            0xABCD,
            "set_peer_salt(0xABCD) must stamp 0xABCD into the high 16 \
             bits of next_id(); got id=0x{:016x} (salt=0x{:04x})",
            id,
            peer_salt_of(id)
        );
        // Counter portion must remain monotonic and small.
        assert_eq!(counter_of(id), 1);
    }

    /// Two peers run independent `next_id` counters in parallel; with
    /// distinct salts, their ids live in disjoint ranges so CRDT sync
    /// surfaces both workspaces instead of folding them into one row.
    /// They share a base doc before appending, so only the minted id
    /// differs between the two `append` calls.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn two_peers_with_independent_next_id_collide_under_sync() {
        let _guard = SaltGuard::new();

        NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);
        let mut tree_a = WorkspaceTree::default();
        tree_a.seed_doc_from_cache();
        let base_changes = tree_a.export_changes_since(&[]);
        let mut tree_b = WorkspaceTree::default();
        tree_b.apply_changes(&base_changes).expect("seed B from A");
        // Snapshot the heads after the base sync so the post-fork
        // export only ships the *incremental* appends.
        let base_heads = tree_a.current_heads();

        // Peer B switches to its own salt and appends a workspace.
        NEXT_ID_PEER_SALT.store(0x2222, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);
        let id_b = tree_b.append_empty_workspace("PeerB");

        // Peer A appends concurrently under its own salt. Without the
        // salt, both peers would mint id `1` here and collide.
        NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);
        let id_a = tree_a.append_empty_workspace("PeerA");

        assert_ne!(
            id_a,
            id_b,
            "two peers with distinct salts must mint distinct ids; \
             got A=0x{:016x} B=0x{:016x} (C-1 regression — pre-patch \
             both peers would have minted id 1)",
            id_a.raw(),
            id_b.raw()
        );
        assert_eq!(peer_salt_of(id_a.raw()), 0x1111);
        assert_eq!(peer_salt_of(id_b.raw()), 0x2222);

        // Sync the incremental appends in both directions and rebuild
        // each peer's cache from the merged doc.
        let changes_a = tree_a.export_changes_since(&base_heads);
        let changes_b = tree_b.export_changes_since(&base_heads);
        tree_b.apply_changes(&changes_a).expect("apply A→B");
        tree_a.apply_changes(&changes_b).expect("apply B→A");

        // Both peers must observe BOTH workspaces — no collision-fold.
        // A shared id would cause `rebuild_cache_from_doc` to merge the
        // two distinct rows into one (strips collide, names overwrite,
        // grants merge).
        for (label, tree) in [("A", &tree_a), ("B", &tree_b)] {
            let has_a = tree.workspaces.iter().any(|w| w.id == id_a);
            let has_b = tree.workspaces.iter().any(|w| w.id == id_b);
            assert!(
                has_a && has_b,
                "peer {label} must see both workspaces after sync; \
                 has_a={has_a} has_b={has_b}, workspaces={:?}",
                tree.workspaces
                    .iter()
                    .map(|w| (w.id.raw(), &w.name))
                    .collect::<Vec<_>>()
            );
        }
    }

    /// `seed_next_id_from_self`: the counter advances based ONLY on ids
    /// minted under the local salt — a remote peer's ids live in a
    /// disjoint range and must not drag it forward.
    #[test]
    #[serial_test::serial(next_id_statics)]
    fn seed_next_id_from_self_respects_peer_salt() {
        let _guard = SaltGuard::new();
        NEXT_ID_PEER_SALT.store(0x1111, AtomicOrdering::Relaxed);
        NEXT_ID.store(1, AtomicOrdering::Relaxed);

        // A tree containing one id under the local salt (counter=5)
        // and a much larger id under a remote salt (counter=1000).
        let local_id_raw = ((0x1111u64) << PEER_ID_SALT_SHIFT) | 5;
        let remote_id_raw = ((0x2222u64) << PEER_ID_SALT_SHIFT) | 0x3E8;
        let mut tree = WorkspaceTree::default();
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from(local_id_raw), "Local"));
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from(remote_id_raw), "Remote"));

        tree.seed_next_id_from_self();

        // The counter must have advanced to 6 (= local max + 1), NOT
        // to 1001 — remote ids do not consume our counter space.
        let next = NEXT_ID.load(AtomicOrdering::Relaxed);
        assert_eq!(
            next, 6,
            "seed_next_id_from_self must only consider ids under the \
             local salt; got counter={next} but the only local-salt id had \
             counter=5 (remote-salt id counter=1000 is in a disjoint \
             range and must be ignored)"
        );
    }
}

/// An agent is admitted to a room the same way anybody is — by a
/// capability naming it. Asserts on the production readers, because the
/// failure worth catching is a roster showing or hiding the wrong people.
#[cfg(test)]
mod agent_membership {
    use super::*;

    fn room() -> Workspace {
        let mut ws = Workspace::test_default(WorkspaceId::new(), "Demo");
        ws.set_owner("quinten");
        ws
    }

    /// The floor, and the one that matters most: a room nobody has
    /// admitted an agent to has no agents in it. A default agent, a
    /// placeholder, or a row that appears because the feature exists
    /// would all show up here.
    #[test]
    fn a_room_with_no_grant_has_no_agents() {
        assert!(room().agent_members().is_empty());
    }

    #[test]
    fn a_room_grant_admits_the_agent_and_a_revoke_removes_it() {
        let mut ws = room();
        let id = ws
            .grant_as(
                &"quinten".to_string(),
                CapOverride::membership(
                    Subject::agent("scribe"),
                    ws.id.raw().to_string(),
                    Verb::Write,
                ),
            )
            .expect("the owner may administer the room");

        assert_eq!(
            ws.agent_members(),
            vec![("scribe".to_string(), CapRole::Editor)],
            "the verb on the membership capability is the role",
        );
        assert_eq!(ws.role_of_agent("scribe"), Some(CapRole::Editor));

        assert!(ws.revoke_as(&"quinten".to_string(), &id));
        assert!(
            ws.agent_members().is_empty(),
            "revoking the capability is the whole of taking the agent out",
        );
        assert_eq!(ws.role_of_agent("scribe"), None);
    }

    #[test]
    fn replicated_membership_does_not_depend_on_the_local_workspace_handle() {
        let mut ws = room();
        ws.grant_capability(CapOverride::membership(
            Subject::person("remote-editor"),
            "a-different-peers-local-id",
            Verb::Write,
        ));
        assert_eq!(
            ws.role_of(&"remote-editor".to_string()),
            Some(CapRole::Editor)
        );
        assert!(ws.allows(
            &ws.actor_for_person(&"remote-editor".to_string()),
            &ResourceRef::App("dev.qos.notes".into()),
            Verb::Write,
        ));
    }

    /// The two lists are disjoint. A person in the roster must never
    /// arrive in the agent list, and an agent must never arrive in the
    /// roster — that second one is the OS claiming a person is present
    /// when none is.
    #[test]
    fn people_and_agents_do_not_leak_into_each_others_lists() {
        let mut ws = room();
        ws.grant_as(
            &"quinten".to_string(),
            CapOverride::membership(
                Subject::person("xaver"),
                ws.id.raw().to_string(),
                Verb::Write,
            ),
        )
        .expect("owner grants");
        ws.grant_as(
            &"quinten".to_string(),
            CapOverride::membership(
                Subject::agent("scribe"),
                ws.id.raw().to_string(),
                Verb::Write,
            ),
        )
        .expect("owner grants");

        let people: Vec<String> = ws.roster().into_iter().map(|(u, _)| u).collect();
        assert!(people.contains(&"xaver".to_string()));
        assert!(
            !people.contains(&"scribe".to_string()),
            "an agent must not be listed as a person: {people:?}",
        );

        let agents: Vec<String> = ws.agent_members().into_iter().map(|(a, _)| a).collect();
        assert_eq!(agents, vec!["scribe".to_string()]);
    }

    /// An agent that holds a capability on one *app* and nothing else is
    /// not a member — the same rule that keeps a person who was shared
    /// one file out of the roster. Without this an agent granted access
    /// to a single document would be presented as being in the room.
    #[test]
    fn a_grant_on_one_app_is_not_membership() {
        let mut ws = room();
        ws.grant_as(
            &"quinten".to_string(),
            CapOverride::grant(
                Subject::agent("scribe"),
                Scope::App("dev.qos.word-processor".into()),
                Verb::Write,
            ),
        )
        .expect("owner grants");
        assert!(
            ws.agent_members().is_empty(),
            "a scope naming one app is a share, not membership",
        );
    }

    /// An expired capability is not a quieter membership, it is none.
    #[test]
    fn an_expired_grant_admits_nobody() {
        let mut ws = room();
        ws.grant_as(
            &"quinten".to_string(),
            CapOverride::membership(
                Subject::agent("scribe"),
                ws.id.raw().to_string(),
                Verb::Write,
            )
            .with_expiry(1),
        )
        .expect("owner grants");
        assert!(ws.agent_members().is_empty());
    }
}

#[cfg(test)]
mod peer_window_merge_tests {
    //! What a peer's window becomes here, and what keeps it that way.
    //!
    //! Asserted against `WorkspaceTree::merge_group`, the function the
    //! room-sync path actually runs, rather than against a helper written
    //! to agree with it.

    use super::*;

    const APP: &str = "dev.qos.word-processor";

    /// Somebody else's tab, as the room document delivers it.
    fn remote_tab(id: u64, surface: u64) -> ReplicatedTab {
        ReplicatedTab {
            id,
            order: 0,
            title: "Word Processor".into(),
            tile: Some(TileContent::Foreign {
                app_id: APP.into(),
                surface_id: surface,
            }),
        }
    }

    fn group_with(tab: Tab) -> WindowGroup {
        let active_tab = tab.id;
        WindowGroup {
            id: next_id(),
            tabs: vec![tab],
            active_tab,
            width: WindowWidth::Normal,
            maximized: false,
        }
    }

    /// A peer's `Foreign` names a surface on *their* compositor. Kept
    /// verbatim it would resolve here to nothing, or to an unrelated
    /// window of ours; what survives instead is the app id, which means
    /// the same thing on both machines and is what a local instance can be
    /// started from.
    #[test]
    fn a_peers_window_arrives_as_the_app_it_names() {
        let local = group_with(Tab {
            id: 7,
            title: String::new(),
            tile: TileContent::Stub {
                kind: AppKind::Other(String::new()),
                label: String::new(),
            },
        });
        let rep = ReplicatedGroup {
            id: local.id,
            order: 0,
            width: None,
            maximized: None,
            tabs: vec![remote_tab(7, 4242)],
        };
        let mut changed = false;
        let merged = WorkspaceTree::merge_group(local, rep, &|_| false, &mut changed);
        match &merged.tabs[0].tile {
            TileContent::PeerWindow {
                app_id,
                label,
                note,
            } => {
                assert_eq!(app_id, APP);
                assert_eq!(label, "Word Processor", "the peer's own name for it");
                assert!(note.is_empty(), "nobody has answered for it yet");
            }
            other => panic!("expected a peer window, got {other:?}"),
        }
    }

    /// The binding survives the peer's next projection: once bound, the
    /// tile names *our* surface while the peer keeps re-projecting
    /// theirs, so a naive "the document wins" merge would take the
    /// window away again every sync interval, forever.
    #[test]
    fn a_local_instance_is_not_clobbered_by_the_peers_own_surface() {
        let local = group_with(Tab {
            id: 7,
            title: "Word Processor".into(),
            // What `bind_into_tab` left here after our instance mapped.
            tile: TileContent::Foreign {
                app_id: APP.into(),
                surface_id: 99,
            },
        });
        let rep = ReplicatedGroup {
            id: local.id,
            order: 0,
            width: None,
            maximized: None,
            // The peer's surface id, which is not ours.
            tabs: vec![remote_tab(7, 4242)],
        };
        let mut changed = false;
        let merged = WorkspaceTree::merge_group(local, rep, &|_| false, &mut changed);
        match &merged.tabs[0].tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(app_id, APP);
                assert_eq!(
                    *surface_id, 99,
                    "the tab must keep hosting this machine's own window"
                );
            }
            other => panic!("expected our own surface, got {other:?}"),
        }
        assert!(!changed, "a merge that changed nothing must not say it did");
    }

    /// A different app taking the tab over *is* news, and is taken.
    /// The rule is "a local binding for the same app wins", not "whatever
    /// is here wins" — the second would strand the tab on a window whose
    /// tab now holds something else.
    #[test]
    fn a_different_app_in_the_tab_still_replaces_what_is_here() {
        let local = group_with(Tab {
            id: 7,
            title: "Word Processor".into(),
            tile: TileContent::Foreign {
                app_id: APP.into(),
                surface_id: 99,
            },
        });
        let mut rep_tab = remote_tab(7, 4242);
        rep_tab.title = "Terminal".into();
        rep_tab.tile = Some(TileContent::Foreign {
            app_id: "dev.qos.terminal".into(),
            surface_id: 4242,
        });
        let rep = ReplicatedGroup {
            id: local.id,
            order: 0,
            width: None,
            maximized: None,
            tabs: vec![rep_tab],
        };
        let mut changed = false;
        let merged = WorkspaceTree::merge_group(local, rep, &|_| false, &mut changed);
        match &merged.tabs[0].tile {
            TileContent::PeerWindow { app_id, .. } => assert_eq!(app_id, "dev.qos.terminal"),
            other => panic!("expected the new app, got {other:?}"),
        }
        assert!(changed);
    }
}

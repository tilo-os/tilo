//! What a desktop surface may ask its topology owner to do, and the pure
//! reduction the owner applies once it has checked the caller.
//!
//! A [`DesktopCommand`] names strips, groups and tabs by id, never by
//! on-screen index, and a [`BoundCommand`] pins it to the exact admitted room
//! revision the requester saw. Neither this payload nor any transport
//! carrying it is a mutation receipt: the owner validates authority and
//! freshness, reserves identities in its own namespace, and only then runs
//! [`apply_command`].

use serde::{Deserialize, Serialize};

use crate::apps::canonical_app_id;
use crate::ids::{MintIds, StripId};
use crate::layout::{Strip, Tab, TileContent, WindowGroup};
use crate::projection::{
    ROOM_CONTENTS_KEY, ROOM_CONTENTS_OWNERS_KEY, ROOM_DESKTOP_COUNTER_KEY, ROOM_ID_KEY,
    ROOM_NAME_KEY, ROOM_OWNER_KEY,
};
use crate::topology::DropTarget;
use crate::tree::DesktopTree;
use crate::width::WindowWidth;
use crate::workspace::Workspace;

mod admission;
mod placement;
pub use admission::{check_owned, namespace_counter, resolve_open_app};
pub use placement::{GroupSpan, OpenContext, OpenIntent, Seam, StripViewport, open_app_placement};

pub const APP_ID: &str = "so.tilo.desktop";
pub const RESOURCE: &str = "desktop:session";
pub const ACTION: &str = "command";
/// Resource prefix of the per-application open action (`desktop:app:<app>`).
pub const OPEN_APP_RESOURCE_PREFIX: &str = "desktop:app:";
/// Resource kind of the per-application open action.
pub const OPEN_APP_RESOURCE_KIND: &str = "desktop.application";
/// Action id of the per-application open action.
pub const OPEN_APP_ACTION: &str = "open";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopCommandError {
    /// Rejected before any canonical state or durable identity changed.
    Rejected(String),
    /// The durable barrier failed or its outcome cannot be established.
    Uncertain(String),
}
impl std::fmt::Display for DesktopCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) | Self::Uncertain(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for DesktopCommandError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSource {
    pub room: String,
    /// Exact admitted Automerge heads observed by the initiating desktop.
    pub heads: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundCommand {
    pub source: CommandSource,
    pub command: DesktopCommand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabLocation {
    pub strip: u64,
    pub group: u64,
    pub tab: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabPlacement {
    /// Native default placement when an admitted workspace has no strips yet.
    EmptyWorkspace,
    InGroup {
        strip: u64,
        group: u64,
        after: Option<u64>,
    },
    NewGroup {
        strip: u64,
        after: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DesktopCommand {
    OpenTab {
        app: String,
        placement: TabPlacement,
    },
    CloseTab {
        location: TabLocation,
    },
    DuplicateTab {
        location: TabLocation,
    },
    ReorderTab {
        location: TabLocation,
        before: Option<u64>,
    },
    ReorderGroup {
        strip: u64,
        group: u64,
        before: Option<u64>,
    },
    MoveTab {
        location: TabLocation,
        placement: TabPlacement,
    },
    SplitTab {
        location: TabLocation,
        before: Option<u64>,
    },
    CreateStrip {
        name: String,
    },
    RenameStrip {
        strip: u64,
        name: String,
    },
    RemoveStrip {
        strip: u64,
    },
    RenameWorkspace {
        name: String,
    },
    /// Open `app` in a new window: the same result as a person opening it
    /// from the launcher, for callers that do not hold a desktop layout (an
    /// agent, the command palette). The owner places it in its own turn, as
    /// a new group at the end of its last strip, or as the first strip of an
    /// empty workspace. Placement-independent, so a stale source revision
    /// does not refuse it. Appended last: existing encodings are unchanged.
    OpenApp {
        app: String,
    },
    /// Save a shortcut in the workspace, replacing the shortcut with the same
    /// id. The shortcut travels as its JSON: its inputs and bindings carry
    /// schema-free values with no fixed binary form. The owner records the
    /// caller as the shortcut's saver, whatever the JSON claims. Appended
    /// last: existing encodings are unchanged.
    SaveShortcut {
        shortcut_json: String,
    },
    /// Delete the workspace's shortcut `id`. Appended last: existing
    /// encodings are unchanged.
    DeleteShortcut {
        id: String,
    },
    /// Register topology namespace `salt` to the calling device, so it can
    /// publish its own windows (a session's legacy windows) into a room
    /// another device governs. Only the governor writes the registry; a
    /// namespace is never rebound. Placement-independent. Appended last:
    /// existing encodings are unchanged.
    ClaimNamespace {
        salt: u16,
    },
    /// Save a workspace permission profile, replacing the one with the same
    /// name. The profile travels as its JSON. Only a native owner applies it,
    /// with the caller's authority over workspace governance. Appended last:
    /// existing encodings are unchanged.
    SavePermissionProfile {
        profile_json: String,
    },
    /// Assign the saved permission profile `profile` to a `person`, `app` or
    /// `agent` `subject` until `expires_unix` (`0` never expires), as the
    /// capabilities it expands to. Only a native owner applies it, as for
    /// [`Self::SavePermissionProfile`]. Appended last: existing encodings are
    /// unchanged.
    AssignPermissionProfile {
        profile: String,
        subject_kind: String,
        subject: String,
        expires_unix: u64,
    },
    /// Record the effect receipt of an invocation that a member's app served
    /// in this room, as the redacted
    /// [`WorkspaceEffectReceipt`](crate::WorkspaceEffectReceipt) JSON the
    /// member's broker produced. Only a native owner applies it; a receipt,
    /// once recorded, never changes. Appended last: existing encodings are
    /// unchanged.
    RecordEffectReceipt {
        receipt_json: String,
    },
    /// Let application `app` use `target_app`'s `action` here, for
    /// `lifetime` `always` (a hosted workspace has no session to end a
    /// `session` grant with). Only a native owner applies it, with the
    /// caller's authority over workspace governance. Appended last: existing
    /// encodings are unchanged.
    GrantAppAction {
        app: String,
        target_app: String,
        action: String,
        lifetime: String,
    },
    /// Take back every grant letting `app` use `target_app`'s `action`, as
    /// for [`Self::GrantAppAction`]. Appended last: existing encodings are
    /// unchanged.
    RevokeAppAction {
        app: String,
        target_app: String,
        action: String,
    },
    /// Move window group `group`, whole, from whichever strip of the
    /// workspace holds it into strip `strip`, before group `before` or at
    /// the end. A move within one strip is a reorder. Appended last:
    /// existing encodings are unchanged.
    MoveGroup {
        group: u64,
        strip: u64,
        before: Option<u64>,
    },
}

impl DesktopCommand {
    /// Whether the owner applies this against its own current desktop, so it
    /// needs no source revision: an [`Self::OpenApp`] it places itself, a
    /// namespace claim that changes only the registry, or a governance or
    /// receipt command that touches no topology.
    pub fn is_placement_independent(&self) -> bool {
        matches!(
            self,
            Self::OpenApp { .. }
                | Self::ClaimNamespace { .. }
                | Self::SavePermissionProfile { .. }
                | Self::AssignPermissionProfile { .. }
                | Self::RecordEffectReceipt { .. }
                | Self::GrantAppAction { .. }
                | Self::RevokeAppAction { .. }
        )
    }
}

/// The profile a [`DesktopCommand::SavePermissionProfile`] carries.
///
/// # Errors
///
/// Why the JSON is not a valid permission profile.
pub fn decode_permission_profile(json: &str) -> Result<collab_types::PermissionProfile, String> {
    let profile: collab_types::PermissionProfile =
        serde_json::from_str(json).map_err(|error| format!("not a permission profile: {error}"))?;
    profile.validate().map_err(|error| error.to_string())?;
    Ok(profile)
}

/// The subject and expiry a [`DesktopCommand::AssignPermissionProfile`] names.
///
/// # Errors
///
/// When `kind` is not `person`, `app` or `agent`.
pub fn permission_assignment(
    kind: &str,
    subject: &str,
    expires_unix: u64,
) -> Result<(collab_types::Subject, collab_types::Expiry), String> {
    let subject = match kind.trim().to_ascii_lowercase().as_str() {
        "person" => collab_types::Subject::person(subject.trim()),
        "app" => collab_types::Subject::app(subject.trim()),
        "agent" => collab_types::Subject::agent(subject.trim()),
        _ => return Err("subject_kind must be person, app, or agent".into()),
    };
    let expiry = (expires_unix > 0)
        .then_some(expires_unix)
        .map_or(collab_types::Expiry::Never, collab_types::Expiry::AtUnix);
    Ok((subject, expiry))
}

/// The shortcut a [`DesktopCommand::SaveShortcut`] carries.
///
/// # Errors
///
/// Why the JSON is not a shortcut, or names none.
pub fn decode_shortcut(json: &str) -> Result<tilo_automation::Shortcut, String> {
    let shortcut: tilo_automation::Shortcut =
        serde_json::from_str(json).map_err(|error| format!("not a shortcut: {error}"))?;
    if shortcut.id.trim().is_empty() {
        return Err("a shortcut needs an id".into());
    }
    Ok(shortcut)
}

/// `json`, the shortcut a [`DesktopCommand::SaveShortcut`] carries, with
/// `saver` recorded as the person who saved it.
///
/// # Errors
///
/// Why the shortcut does not encode.
pub fn stamp_saver(json: &str, saver: &str) -> Result<String, String> {
    let mut shortcut = decode_shortcut(json)?;
    shortcut.saved_by = Some(saver.to_string());
    serde_json::to_string(&shortcut).map_err(|error| error.to_string())
}

/// Failure code of every definite desktop command refusal.
pub const REJECTED_CODE: &str = "desktop_command_rejected";
/// `kind` of a desktop command receipt value.
pub const RECEIPT_KIND: &str = "desktop.command";

/// Identity of one desktop command (optimistic open, close, reorder): the
/// certified issuer device and the durable invocation UUID the issuer minted.
/// A retry reuses it. It is only an identifier: it never grants anything,
/// and the device comes from authenticated admission, never from arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DesktopCommandId {
    pub device: [u8; 32],
    pub invocation: uuid::Uuid,
}
/// `<64 hex device>:<uuid>`: the Cloud Store's receipt scope.
impl std::fmt::Display for DesktopCommandId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", hex::encode(self.device), self.invocation)
    }
}
impl std::str::FromStr for DesktopCommandId {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        let (hex_device, invocation) = text.split_once(':').ok_or("desktop command id")?;
        let device: [u8; 32] = hex::decode(hex_device)
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|_| "desktop command device")?;
        let invocation = uuid::Uuid::parse_str(invocation).map_err(|e| e.to_string())?;
        if invocation.is_nil()
            || Some(invocation.to_string().as_str()) != text.get(65..)
            || hex::encode(device) != hex_device
        {
            return Err("desktop command id must be canonical".into());
        }
        Ok(Self { device, invocation })
    }
}

/// u64 desktop ids cross to JavaScript as decimal strings.
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    // The lint's size limit is the pointer width, so it only fires on 64-bit.
    #[cfg_attr(
        target_pointer_width = "64",
        expect(
            clippy::trivially_copy_pass_by_ref,
            reason = "serde `serialize_with` passes the field by reference"
        )
    )]
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// One window a committed command created.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedWindow {
    pub app_id: String,
    pub record_id: String,
    #[serde(with = "decimal")]
    pub strip: u64,
    #[serde(with = "decimal")]
    pub group: u64,
    #[serde(with = "decimal")]
    pub tab: u64,
}

/// The value of a desktop command's `Succeeded` update: the receipt the
/// issuer (person or agent) matches its command id against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopCommandReceipt {
    pub kind: String,
    /// [`DesktopCommandId`] text form.
    pub command: String,
    /// Owner generation holding the change.
    pub generation: u64,
    /// Windows created, in creation order; empty for close and reorder, and
    /// on owners that do not yet report them.
    pub opened: Vec<OpenedWindow>,
}
impl DesktopCommandReceipt {
    pub fn new(id: &DesktopCommandId, generation: u64, opened: Vec<OpenedWindow>) -> Self {
        Self {
            kind: RECEIPT_KIND.into(),
            command: id.to_string(),
            generation,
            opened,
        }
    }
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    /// JSON Schema of the receipt value: the declared result of every
    /// published desktop command action (open and the layout command).
    pub fn schema() -> serde_json::Value {
        let id = serde_json::json!({"type": "string", "pattern": "^[0-9]+$"});
        serde_json::json!({
            "type": "object",
            "required": ["kind", "command", "generation", "opened"],
            "properties": {
                "kind": {"const": RECEIPT_KIND},
                "command": {"type": "string"},
                "generation": {"type": "integer", "minimum": 1},
                "opened": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["appId", "recordId", "strip", "group", "tab"],
                        "properties": {
                            "appId": {"type": "string"},
                            "recordId": {"type": "string"},
                            "strip": id,
                            "group": id,
                            "tab": id,
                        },
                    },
                },
            },
        })
    }
}

/// Stable reason of a definite refusal, for presentation only: every
/// refusal snaps an optimistic command back, whatever its reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopRefusal {
    NotPermitted,
    StaleSource,
    UnknownApp,
    ForeignTarget,
    TargetMissing,
    IdConflict,
    Invalid,
}
impl DesktopRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotPermitted => "not_permitted",
            Self::StaleSource => "stale_source",
            Self::UnknownApp => "unknown_app",
            Self::ForeignTarget => "foreign_target",
            Self::TargetMissing => "target_missing",
            Self::IdConflict => "id_conflict",
            Self::Invalid => "invalid",
        }
    }
    /// Classify an owner's [`DesktopCommandError::Rejected`] message.
    pub fn classify(message: &str) -> Self {
        let m = message.to_ascii_lowercase();
        let any = |needles: &[&str]| needles.iter().any(|needle| m.contains(needle));
        if any(&[
            "reused",
            "identity conflict",
            "substitution",
            "already reserved",
        ]) {
            Self::IdConflict
        } else if any(&["another topology owner", "foreign"]) {
            Self::ForeignTarget
        } else if any(&[
            "cannot be opened",
            "checked record factory",
            "canonical application identity",
        ]) {
            Self::UnknownApp
        } else if any(&[
            "cannot change",
            "no longer admitted",
            "no longer current",
            "blocked",
            "may only open",
            "require certified",
            "not a member",
            "permission",
            "denied",
            "refused",
        ]) {
            Self::NotPermitted
        } else if any(&["stale", "no admitted source revision"]) {
            Self::StaleSource
        } else if any(&["was removed", "no longer present", "removed or moved"]) {
            Self::TargetMissing
        } else {
            Self::Invalid
        }
    }
    /// `ActionFailure.details` of a refusal: the command id and the reason.
    pub fn details(self, id: &DesktopCommandId) -> serde_json::Value {
        serde_json::json!({"command": id.to_string(), "reason": self.as_str()})
    }
}

/// Room heads also advance for receipts, enrollment and document durability.
/// Those changes must not invalidate a gesture against an unchanged desktop.
/// Compare the original admitted desktop with the current one, without
/// rebinding the command or weakening the current caller/target checks.
///
/// # Errors
///
/// Returns a description when the document cannot be read at `heads`.
pub fn desktop_source_matches(
    doc: &automerge::AutoCommit,
    heads: &[[u8; 32]],
) -> Result<bool, String> {
    use automerge::ReadDoc;
    let heads = heads
        .iter()
        .copied()
        .map(automerge::ChangeHash)
        .collect::<Vec<_>>();
    if heads.is_empty()
        || heads
            .iter()
            .any(|head| doc.get_change_by_hash(head).is_none())
    {
        return Ok(false);
    }
    for key in [
        ROOM_ID_KEY,
        ROOM_OWNER_KEY,
        ROOM_NAME_KEY,
        ROOM_CONTENTS_KEY,
        ROOM_CONTENTS_OWNERS_KEY,
        // Also prevents an old launch from being reused after its tab closes.
        ROOM_DESKTOP_COUNTER_KEY,
    ] {
        let previous = doc
            .get_all_at(automerge::ROOT, key, &heads)
            .map_err(|e| e.to_string())?;
        let current = doc
            .get_all(automerge::ROOT, key)
            .map_err(|e| e.to_string())?;
        if previous != current {
            return Ok(false);
        }
        for (value, object) in current {
            if matches!(value, automerge::Value::Object(_))
                && doc
                    .hydrate(&object, Some(&heads))
                    .map_err(|e| e.to_string())?
                    != doc.hydrate(&object, None).map_err(|e| e.to_string())?
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Strip, group and tab indices of one located tab.
type TabIndex = (usize, usize, usize);

fn strip_index(workspace: &Workspace, id: u64) -> Result<usize, String> {
    workspace
        .strips
        .iter()
        .position(|strip| strip.id.0 == id)
        .ok_or_else(|| "strip was removed".into())
}
fn strip_at(workspace: &Workspace, index: usize) -> Result<&Strip, String> {
    workspace
        .strips
        .get(index)
        .ok_or_else(|| "strip was removed".into())
}
fn strip_at_mut(workspace: &mut Workspace, index: usize) -> Result<&mut Strip, String> {
    workspace
        .strips
        .get_mut(index)
        .ok_or_else(|| "strip was removed".into())
}
fn group_at(workspace: &Workspace, strip: usize, group: usize) -> Result<&WindowGroup, String> {
    strip_at(workspace, strip)?
        .window_groups
        .get(group)
        .ok_or_else(|| "group was removed".into())
}
fn group_at_mut(
    workspace: &mut Workspace,
    strip: usize,
    group: usize,
) -> Result<&mut WindowGroup, String> {
    strip_at_mut(workspace, strip)?
        .window_groups
        .get_mut(group)
        .ok_or_else(|| "group was removed".into())
}
fn location(workspace: &Workspace, at: &TabLocation) -> Result<TabIndex, String> {
    let strip = strip_index(workspace, at.strip)?;
    let (group, window) = strip_at(workspace, strip)?
        .window_groups
        .iter()
        .enumerate()
        .find(|(_, window)| window.id == at.group)
        .ok_or("group was removed")?;
    let tab = window
        .tabs
        .iter()
        .position(|tab| tab.id == at.tab)
        .ok_or("tab was removed or moved")?;
    Ok((strip, group, tab))
}
fn after<T>(items: &[T], anchor: Option<u64>, id: impl Fn(&T) -> u64) -> Result<usize, String> {
    anchor.map_or(Ok(items.len()), |anchor| {
        items
            .iter()
            .position(|item| id(item) == anchor)
            .map(|index| index.saturating_add(1))
            .ok_or_else(|| "placement anchor was removed".into())
    })
}
fn validate_placement(workspace: &Workspace, placement: &TabPlacement) -> Result<(), String> {
    match placement {
        TabPlacement::EmptyWorkspace => {
            if !workspace.strips.is_empty() {
                return Err("workspace is no longer empty".into());
            }
        }
        TabPlacement::InGroup {
            strip,
            group,
            after: anchor,
        } => {
            let strip = strip_index(workspace, *strip)?;
            let window = strip_at(workspace, strip)?
                .window_groups
                .iter()
                .find(|window| window.id == *group)
                .ok_or("destination group was removed")?;
            after(&window.tabs, *anchor, |tab| tab.id)?;
        }
        TabPlacement::NewGroup {
            strip,
            after: anchor,
        } => {
            let strip = strip_index(workspace, *strip)?;
            after(
                &strip_at(workspace, strip)?.window_groups,
                *anchor,
                |window| window.id,
            )?;
        }
    }
    Ok(())
}
fn place(
    workspace: &mut Workspace,
    tab: Tab,
    placement: &TabPlacement,
    group_id: u64,
    new_strip: Option<u64>,
) -> Result<(), String> {
    match placement {
        TabPlacement::EmptyWorkspace => workspace.strips.push(Strip {
            id: StripId(new_strip.ok_or("fresh strip identity missing")?),
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: group_id,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }),
        TabPlacement::InGroup {
            strip,
            group,
            after: anchor,
        } => {
            let strip = strip_index(workspace, *strip)?;
            let window = strip_at_mut(workspace, strip)?
                .window_groups
                .iter_mut()
                .find(|window| window.id == *group)
                .ok_or("destination group was removed")?;
            let index = after(&window.tabs, *anchor, |tab| tab.id)?;
            window.tabs.insert(index, tab);
        }
        TabPlacement::NewGroup {
            strip,
            after: anchor,
        } => {
            let strip = strip_index(workspace, *strip)?;
            let groups = &mut strip_at_mut(workspace, strip)?.window_groups;
            let index = after(groups, *anchor, |window| window.id)?;
            groups.insert(
                index,
                WindowGroup {
                    id: group_id,
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                },
            );
        }
    }
    Ok(())
}
fn remove(workspace: &mut Workspace, at: TabIndex) -> Result<Tab, String> {
    let (strip, group, tab) = at;
    let groups = &mut strip_at_mut(workspace, strip)?.window_groups;
    let window = groups.get_mut(group).ok_or("group was removed")?;
    if tab >= window.tabs.len() {
        return Err("tab was removed or moved".into());
    }
    let removed = window.tabs.remove(tab);
    if window.active_tab == removed.id {
        window.active_tab = window
            .tabs
            .get(tab)
            .or_else(|| window.tabs.last())
            .map_or(0, |next| next.id);
    }
    if window.tabs.is_empty() {
        groups.remove(group);
    }
    Ok(removed)
}
fn name(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err("invalid desktop name".into());
    }
    Ok(value.into())
}
fn duplicate_tab(
    workspace: &mut Workspace,
    at: &TabLocation,
    mint: &mut impl FnMut() -> Result<u64, String>,
    create: &mut impl FnMut(&str, u64) -> Result<(String, TileContent), String>,
) -> Result<(), String> {
    let (strip, group, tab) = location(workspace, at)?;
    let original = group_at(workspace, strip, group)?
        .tabs
        .get(tab)
        .ok_or("tab was removed or moved")?;
    if !matches!(
        original.tile,
        TileContent::AppPlaceholder { .. } | TileContent::Stub { .. }
    ) {
        return Err("native or foreign surface cannot be duplicated".into());
    }
    let app = canonical_app_id(&original.tile).ok_or("missing canonical application identity")?;
    let title = original.title.clone();
    let id = mint()?;
    let (_, content) = create(&app, id)?;
    let duplicate = Tab {
        id,
        title,
        tile: content,
    };
    group_at_mut(workspace, strip, group)?
        .tabs
        .insert(tab.saturating_add(1), duplicate);
    Ok(())
}
fn move_tab(
    workspace: &mut Workspace,
    at: &TabLocation,
    placement: &TabPlacement,
    mint: &mut impl FnMut() -> Result<u64, String>,
) -> Result<(), String> {
    let source = location(workspace, at)?;
    validate_placement(workspace, placement)?;
    let (source_strip, source_group, _) = source;
    let only_tab =
        group_at(workspace, source_strip, source_group).is_ok_and(|window| window.tabs.len() == 1);
    if matches!(placement, TabPlacement::InGroup { group, .. } if *group == at.group)
        || matches!(placement, TabPlacement::NewGroup { after: Some(group), .. } if *group == at.group && only_tab)
    {
        return Err("move requires a distinct retained destination".into());
    }
    let group = mint()?;
    if let TabPlacement::NewGroup {
        strip,
        after: anchor,
    } = placement
    {
        let destination_strip = strip_index(workspace, *strip)?;
        let destination = after(
            &strip_at(workspace, destination_strip)?.window_groups,
            *anchor,
            |window| window.id,
        )?;
        let mut tree = DesktopTree {
            workspaces: vec![workspace.clone()],
            ..DesktopTree::default()
        };
        let moved = if *strip == at.strip {
            tree.split_tab_into_new_group(
                StripId(*strip),
                at.tab,
                destination,
                &mut Reserved(group),
            )
        } else {
            tree.move_tab_across_strips(
                StripId(at.strip),
                StripId(*strip),
                at.tab,
                group,
                DropTarget::GroupGap {
                    gap_index: destination,
                },
            )
        };
        moved.ok_or("native topology rejected the tab move")?;
        *workspace = tree.workspaces.remove(0);
        return Ok(());
    }
    let tab = remove(workspace, source)?;
    place(workspace, tab, placement, group, None)
}
fn split_tab(
    workspace: &mut Workspace,
    at: &TabLocation,
    before: Option<u64>,
    mint: &mut impl FnMut() -> Result<u64, String>,
) -> Result<(), String> {
    let (strip, group, _) = location(workspace, at)?;
    if group_at(workspace, strip, group)?.tabs.len() < 2 {
        return Err("cannot split a group's only tab".into());
    }
    let groups = &strip_at(workspace, strip)?.window_groups;
    if before.is_some_and(|id| !groups.iter().any(|window| window.id == id)) {
        return Err("destination group was removed".into());
    }
    let id = mint()?;
    let groups = &strip_at(workspace, strip)?.window_groups;
    let destination = before
        .and_then(|id| groups.iter().position(|window| window.id == id))
        .unwrap_or(groups.len());
    let mut tree = DesktopTree {
        workspaces: vec![workspace.clone()],
        ..DesktopTree::default()
    };
    tree.split_tab_into_new_group(StripId(at.strip), at.tab, destination, &mut Reserved(id))
        .ok_or("native topology rejected the tab split")?;
    *workspace = tree.workspaces.remove(0);
    Ok(())
}

/// Apply a shortcut command to `workspace`'s saved shortcuts.
fn apply_shortcut_command(
    workspace: &mut Workspace,
    command: &DesktopCommand,
) -> Result<(), String> {
    match command {
        DesktopCommand::SaveShortcut { shortcut_json } => {
            let shortcut = decode_shortcut(shortcut_json)?;
            match workspace
                .shortcuts
                .iter_mut()
                .find(|saved| saved.id == shortcut.id)
            {
                Some(saved) => *saved = shortcut,
                None => workspace.shortcuts.push(shortcut),
            }
            workspace
                .shortcuts
                .sort_by(|left, right| left.id.cmp(&right.id));
        }
        DesktopCommand::DeleteShortcut { id } => {
            let before = workspace.shortcuts.len();
            workspace.shortcuts.retain(|shortcut| shortcut.id != *id);
            if workspace.shortcuts.len() == before {
                return Err(format!("there is no shortcut `{id}`"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Canonical topology reduction. The host checks authority and supplies fresh
/// identities and a checked record constructor before the tab enters the tree.
///
/// # Errors
///
/// Returns a description when the command no longer matches the topology, a
/// name is invalid, or `mint`/`create` refuses.
pub fn apply_command(
    workspace: &mut Workspace,
    command: &DesktopCommand,
    mint: &mut impl FnMut() -> Result<u64, String>,
    create: &mut impl FnMut(&str, u64) -> Result<(String, TileContent), String>,
) -> Result<(), String> {
    match command {
        DesktopCommand::SaveShortcut { .. } | DesktopCommand::DeleteShortcut { .. } => {
            apply_shortcut_command(workspace, command)?;
        }
        DesktopCommand::SavePermissionProfile { .. }
        | DesktopCommand::AssignPermissionProfile { .. }
        | DesktopCommand::RecordEffectReceipt { .. }
        | DesktopCommand::GrantAppAction { .. }
        | DesktopCommand::RevokeAppAction { .. } => {
            return Err("only the workspace's native owner applies this command".into());
        }
        DesktopCommand::OpenApp { .. } => {
            let resolved = resolve_open_app(workspace, command.clone(), |_| true)?;
            return apply_command(workspace, &resolved, mint, create);
        }
        DesktopCommand::OpenTab { app, placement } => {
            validate_placement(workspace, placement)?;
            let id = mint()?;
            let group = mint()?;
            let (title, content) = create(app, id)?;
            let strip = matches!(placement, TabPlacement::EmptyWorkspace)
                .then(&mut *mint)
                .transpose()?;
            let tab = Tab {
                id,
                title,
                tile: content,
            };
            place(workspace, tab, placement, group, strip)?;
        }
        DesktopCommand::DuplicateTab { location: at } => {
            duplicate_tab(workspace, at, mint, create)?;
        }
        DesktopCommand::CloseTab { location: at } => {
            remove(workspace, location(workspace, at)?)?;
        }
        DesktopCommand::ReorderTab {
            location: at,
            before,
        } => {
            let (strip, group, tab) = location(workspace, at)?;
            let tabs = &mut group_at_mut(workspace, strip, group)?.tabs;
            if before == &Some(at.tab) {
                return Err("tab cannot be its own destination".into());
            }
            if before.is_some_and(|id| !tabs.iter().any(|candidate| candidate.id == id)) {
                return Err("destination tab was removed".into());
            }
            let moved = tabs.remove(tab);
            let destination = before
                .and_then(|id| tabs.iter().position(|candidate| candidate.id == id))
                .unwrap_or(tabs.len());
            tabs.insert(destination, moved);
        }
        DesktopCommand::ReorderGroup {
            strip,
            group,
            before,
        } => reorder_group(workspace, *strip, *group, *before)?,
        DesktopCommand::MoveGroup {
            group,
            strip,
            before,
        } => move_group(workspace, *group, *strip, *before)?,
        DesktopCommand::MoveTab {
            location: at,
            placement,
        } => move_tab(workspace, at, placement, mint)?,
        DesktopCommand::SplitTab {
            location: at,
            before,
        } => split_tab(workspace, at, *before, mint)?,
        DesktopCommand::CreateStrip { name: label } => workspace.strips.push(Strip {
            id: StripId(mint()?),
            name: name(label)?,
            window_groups: vec![],
        }),
        DesktopCommand::RenameStrip { strip, name: label } => {
            let strip = strip_index(workspace, *strip)?;
            strip_at_mut(workspace, strip)?.name = name(label)?;
        }
        DesktopCommand::RemoveStrip { strip } => {
            let strip = strip_index(workspace, *strip)?;
            if !strip_at(workspace, strip)?.window_groups.is_empty() {
                return Err("strip must be empty before removal".into());
            }
            if workspace.strips.len() == 1 {
                return Err("cannot remove the last strip".into());
            }
            workspace.strips.remove(strip);
        }
        DesktopCommand::RenameWorkspace { name: label } => workspace.name = name(label)?,
        DesktopCommand::ClaimNamespace { .. } => {
            return Err("a namespace claim changes the room's registry, not a workspace".into());
        }
    }
    Ok(())
}

fn reorder_group(
    workspace: &mut Workspace,
    strip: u64,
    group: u64,
    before: Option<u64>,
) -> Result<(), String> {
    let strip = strip_index(workspace, strip)?;
    let groups = &mut strip_at_mut(workspace, strip)?.window_groups;
    let at = groups
        .iter()
        .position(|window| window.id == group)
        .ok_or("group was removed")?;
    if before == Some(group) {
        return Err("group cannot be its own destination".into());
    }
    if before.is_some_and(|id| !groups.iter().any(|window| window.id == id)) {
        return Err("destination group was removed".into());
    }
    let moved = groups.remove(at);
    let destination = before
        .and_then(|id| groups.iter().position(|window| window.id == id))
        .unwrap_or(groups.len());
    groups.insert(destination, moved);
    Ok(())
}

fn move_group(
    workspace: &mut Workspace,
    group: u64,
    strip: u64,
    before: Option<u64>,
) -> Result<(), String> {
    let destination = strip_index(workspace, strip)?;
    if before == Some(group) {
        return Err("group cannot be its own destination".into());
    }
    let source = workspace
        .strips
        .iter()
        .position(|candidate| {
            candidate
                .window_groups
                .iter()
                .any(|window| window.id == group)
        })
        .ok_or("group was removed")?;
    if source == destination {
        return reorder_group(workspace, strip, group, before);
    }
    let groups = &strip_at(workspace, destination)?.window_groups;
    if before.is_some_and(|id| !groups.iter().any(|window| window.id == id)) {
        return Err("destination group was removed".into());
    }
    let source_groups = &mut strip_at_mut(workspace, source)?.window_groups;
    let at = source_groups
        .iter()
        .position(|window| window.id == group)
        .ok_or("group was removed")?;
    let moved = source_groups.remove(at);
    let groups = &mut strip_at_mut(workspace, destination)?.window_groups;
    let index = before
        .and_then(|id| groups.iter().position(|window| window.id == id))
        .unwrap_or(groups.len());
    groups.insert(index, moved);
    Ok(())
}

/// [`apply_command`] on the workspace of `room` in `tree`: the reduction a
/// host runs against the desktop it renders.
///
/// # Errors
///
/// Returns a description when `room` is not in `tree`, or as [`apply_command`].
pub fn apply_in_room(
    tree: &mut DesktopTree,
    room: &str,
    command: &DesktopCommand,
    mint: &mut impl FnMut() -> Result<u64, String>,
    create: &mut impl FnMut(&str, u64) -> Result<(String, TileContent), String>,
) -> Result<(), String> {
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room.to_string() == room)
        .ok_or("command room is not in this desktop")?;
    apply_command(workspace, command, mint, create)
}

/// An identity the owner already reserved in its own namespace, handed to a
/// topology operation that mints exactly one.
struct Reserved(u64);

impl MintIds for Reserved {
    fn mint_id(&mut self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod command_id_tests {
    use super::*;

    #[test]
    fn a_command_id_round_trips_through_its_text_form() {
        let id = DesktopCommandId {
            device: [0xab; 32],
            invocation: uuid::Uuid::from_u128(0x1234_5678_9abc_4def_8123_4567_89ab_cdef),
        };
        let text = id.to_string();
        assert_eq!(text.len(), 64 + 1 + 36);
        assert_eq!(text.parse::<DesktopCommandId>().unwrap(), id);
        assert!("ab:not-a-uuid".parse::<DesktopCommandId>().is_err());
        let nil = format!("{}:{}", hex::encode([1; 32]), uuid::Uuid::nil());
        assert!(nil.parse::<DesktopCommandId>().is_err());
        let upper = text.to_ascii_uppercase();
        assert!(
            upper.parse::<DesktopCommandId>().is_err(),
            "one canonical form"
        );
    }

    #[test]
    fn a_receipt_carries_the_id_and_window_ids_as_decimal_strings() {
        let id = DesktopCommandId {
            device: [7; 32],
            invocation: uuid::Uuid::from_u128(9),
        };
        let receipt = DesktopCommandReceipt::new(
            &id,
            5,
            vec![OpenedWindow {
                app_id: "so.tilo.notes".into(),
                record_id: "r".into(),
                strip: u64::MAX,
                group: 2,
                tab: 3,
            }],
        );
        let value = receipt.to_value();
        assert_eq!(value["kind"], RECEIPT_KIND);
        assert_eq!(value["command"], id.to_string());
        assert_eq!(value["opened"][0]["strip"], u64::MAX.to_string());
        assert_eq!(value["opened"][0]["appId"], "so.tilo.notes");
        let back: DesktopCommandReceipt = serde_json::from_value(value).unwrap();
        assert_eq!(back, receipt);
    }

    #[test]
    fn refusal_messages_map_to_stable_reasons() {
        use DesktopRefusal::*;
        for (message, reason) in [
            ("caller cannot change this desktop", NotPermitted),
            ("agent tasks may only open applications", NotPermitted),
            ("desktop source revision is stale", StaleSource),
            ("application cannot be opened here", UnknownApp),
            ("target belongs to another topology owner", ForeignTarget),
            ("tab was removed or moved", TargetMissing),
            (
                "invocation identity reused with a different desktop command",
                IdConflict,
            ),
            ("desktop command exceeds bounded size", Invalid),
        ] {
            assert_eq!(DesktopRefusal::classify(message), reason, "{message}");
        }
        let id = DesktopCommandId {
            device: [1; 32],
            invocation: uuid::Uuid::from_u128(2),
        };
        assert_eq!(
            StaleSource.details(&id),
            serde_json::json!({"command": id.to_string(), "reason": "stale_source"})
        );
    }
}

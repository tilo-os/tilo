//! A person present in a workspace.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    /// Tab id the collaborator is focused on, driving per-tab presence
    /// dots. `None` means present in the workspace but not on content.
    #[serde(default)]
    pub active_tab: Option<u64>,
    /// The id of the app they are in, as their live presence says: what
    /// tells where they are when [`Self::active_tab`] is not a window here,
    /// as in a room whose layout is personal.
    #[serde(default)]
    pub in_app: Option<String>,
    /// Optional `#rrggbb`/`#rrggbbaa` override for the palette
    /// `ui::presence::user_color` would otherwise pick.
    #[serde(default)]
    pub cursor_color: Option<String>,
    /// Optional avatar URL or initials fragment; not yet wired to a
    /// bitmap loader (the runtime renders initials today).
    #[serde(default)]
    pub avatar: Option<String>,
    /// Display-only role chip. Authority comes from
    /// `Workspace::role_of`, never from this field.
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
    /// Another device of the person this one belongs to, named by the
    /// device: it is shown among their devices, never as someone else.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_device: bool,
}

impl Collaborator {
    /// Build a collaborator with no tab attribution.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            active_tab: None,
            in_app: None,
            cursor_color: None,
            avatar: None,
            role_id: None,
            invited: false,
            session: None,
            own_device: false,
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

    /// Mark this row as another device of the local person.
    #[must_use]
    pub fn as_own_device(mut self) -> Self {
        self.own_device = true;
        self
    }

    /// Mark this collaborator as invited rather than arrived.
    #[must_use]
    pub fn as_invited(mut self) -> Self {
        self.invited = true;
        self
    }
}

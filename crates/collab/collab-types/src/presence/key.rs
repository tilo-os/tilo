//! Where a person's presence is: the scope a presence update is keyed by,
//! and what it says about where they are looking.
//!
//! **The postcard encoding of [`PresenceKey`] is a wire contract.** Cap'n
//! Proto carries the key bytes opaquely and peer replication forwards the
//! same bytes ([`crate::PeerPresence::key`]), so variant order never changes.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PathSegment {
    Field(String),
    Index(u32),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PresenceKey {
    Widget {
        path: Vec<PathSegment>,
    },
    Tab {
        id: String,
    },
    Workspace {
        id: String,
    },
    App {
        app: AppKind,
        sub: String,
    },
    /// A participant's broadcast [`ViewSubset`],
    /// carried as its postcard encoding. `PresencePayload` has no View
    /// variant of its own, so it rides the existing presence frame inside
    /// the routing key instead. Bytes rather than a live struct because the
    /// key must stay `Hash + Eq`, which `f64` viewport fields are not.
    View {
        data: Vec<u8>,
    },
    /// A pointer position, together with the coordinate space it was measured
    /// in. Every other variant is a scope; the scope of a pointer is the space
    /// its numbers mean something in. See [`PointerSpace`].
    Pointer {
        space: PointerSpace,
    },
    /// Stable action resource scope. It deliberately carries no pixels: the
    /// receiving app resolves `id` against its own live layout.
    Resource {
        app: String,
        id: String,
        kind: String,
    },
}

/// The coordinate space a pointer position is measured in.
///
/// A pointer is a pair of numbers that mean nothing on their own: the same
/// `(230, 34)` is a place on the desktop, in a file manager, and in a word
/// processor. The space travels with the position because it's what decides
/// who may place it — and who may not: a presence frame reaches every app in
/// a room, so without this each read the position as its own and one person
/// was drawn once per open window. With the space carried, at most one
/// receiver can answer "that is my space".
///
/// **Variant order is a wire contract.** Cap'n Proto carries the postcard key
/// bytes opaquely and peer replication forwards the same bytes, so reordering
/// silently misreads every pointer scope rather than failing to build.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PointerSpace {
    /// Compositor scene coordinates on this device's own desktop. Only
    /// meaningful on the device that measured them: its monitors decide
    /// where anything is, so a collaborator's desktop is a different shape.
    /// A person's pointer goes to other devices as [`Self::Tile`] instead.
    Desktop { room: String },
    /// Content coordinates inside the surface `surface` names — the app id
    /// that surface announces (`ui::local_surface_id`). Named by the
    /// app rather than the window instance: two windows of one app on one
    /// document are comparable content, but a file manager and a word
    /// processor are not, even when the broker hands both the same frame.
    Surface { surface: String },
    /// Logical pixels from the top-left corner of the tile that shows tab
    /// `tab` of `room`. Tabs are the room's, shared by every device in it,
    /// while which monitor shows a tab and where is each device's own
    /// layout — so this is how a person's desktop pointer travels: every
    /// receiver places it over its own tile for that tab, above every
    /// window, whatever its monitors look like.
    Tile { room: String, tab: u64 },
}

/// First path field of a [`PresenceKey::digest_form`] key.
pub const DIGEST_KEY_FIELD: &str = "@scope-digest";

impl PresenceKey {
    /// The key of the widget at `path`, or `None` for one without a presence path.
    pub fn widget(path: &[PathSegment]) -> Option<Self> {
        (!path.is_empty()).then(|| Self::Widget {
            path: path.to_vec(),
        })
    }

    /// 16-byte digest of this key's canonical (postcard) encoding. Used where
    /// a scope does not fit a bounded carrier, e.g. the 64-byte `LIVE_V1`
    /// anchor, whose Writer keys can name 130-byte block ids.
    pub fn scope_digest(&self) -> Option<[u8; 16]> {
        let bytes = postcard::to_allocvec(self).ok()?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"tilo-presence-scope-v1\0");
        hash.update(&bytes);
        let mut out = [0; 16];
        out.copy_from_slice(&hash.finalize().as_bytes()[..16]);
        Some(out)
    }

    /// The stand-in key a receiver files a digest-addressed scope under
    /// (`Widget{[DIGEST_KEY_FIELD, hex(digest)]}`). An editor that draws
    /// remote presence also looks its own keys up in this form
    /// ([`Self::digest_form`]), so the two meet without the receiver ever
    /// knowing the full scope.
    pub fn from_scope_digest(digest: [u8; 16]) -> Self {
        PresenceKey::Widget {
            path: vec![
                PathSegment::Field(DIGEST_KEY_FIELD.to_owned()),
                PathSegment::Field(hex::encode(digest)),
            ],
        }
    }

    /// This key's digest-addressed stand-in (see [`Self::from_scope_digest`]).
    pub fn digest_form(&self) -> Option<Self> {
        self.scope_digest().map(Self::from_scope_digest)
    }

    /// Wrap a [`ViewSubset`] as a presence key for
    /// broadcast. Encode failure yields empty bytes, which [`Self::as_view`]
    /// decodes back to `None` — a dropped frame, never a panic.
    pub fn view(subset: &ViewSubset) -> Self {
        PresenceKey::View {
            data: postcard::to_allocvec(subset).unwrap_or_default(),
        }
    }

    /// Decode a broadcast [`ViewSubset`] back out
    /// of a [`PresenceKey::View`]. `None` for any other variant, or for
    /// `View` bytes that fail to decode.
    pub fn as_view(&self) -> Option<ViewSubset> {
        match self {
            PresenceKey::View { data } => postcard::from_bytes(data).ok(),
            _ => None,
        }
    }

    /// The key a pointer measured on `room`'s desktop is published under.
    pub fn desktop_pointer(room: impl Into<String>) -> Self {
        PresenceKey::Pointer {
            space: PointerSpace::Desktop { room: room.into() },
        }
    }

    /// The key a pointer placed relative to the tile of `room`'s tab `tab`
    /// is published under.
    pub fn tile_pointer(room: impl Into<String>, tab: u64) -> Self {
        PresenceKey::Pointer {
            space: PointerSpace::Tile {
                room: room.into(),
                tab,
            },
        }
    }

    /// The key a pointer measured inside `surface`'s content is published
    /// under.
    pub fn surface_pointer(surface: impl Into<String>) -> Self {
        PresenceKey::Pointer {
            space: PointerSpace::Surface {
                surface: surface.into(),
            },
        }
    }

    pub fn resource(
        app: impl Into<String>,
        id: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        Self::Resource {
            app: app.into(),
            id: id.into(),
            kind: kind.into(),
        }
    }

    /// The coordinate space a pointer arriving under this key is measured
    /// in, or `None` for a key that is not a pointer scope at all. Two
    /// older spellings still mean "desktop": [`PresenceKey::Workspace`],
    /// the session's own desktop pointer republished per room, and
    /// [`Self::pointer_default`], the tag the keyless capnp `publishCursor`
    /// path was re-keyed to.
    pub fn pointer_space(&self) -> Option<PointerSpace> {
        match self {
            PresenceKey::Pointer { space } => Some(space.clone()),
            PresenceKey::Workspace { id } => Some(PointerSpace::Desktop { room: id.clone() }),
            PresenceKey::Widget { path } if path.is_empty() => Some(PointerSpace::Desktop {
                room: String::new(),
            }),
            _ => None,
        }
    }

    /// Whether a pointer arriving under this key is a position on the
    /// *desktop* — one the compositor overlay places, above every window —
    /// rather than inside one window's content. Reading the legacy keyless
    /// tag as an ordinary widget scope is what drew one person once per
    /// window: the broker hands every inbound frame to every app in the
    /// room, so each window claimed the pointer as its own. Deliberately
    /// about the *scope*, not the transport: a pointer with no key at all
    /// never comes through here and stays the window's to paint.
    pub fn scopes_a_desktop_pointer(&self) -> bool {
        matches!(
            self.pointer_space(),
            Some(PointerSpace::Desktop { .. } | PointerSpace::Tile { .. })
        )
    }

    /// The canonical `PresenceKey` used to attribute a pointer publish that
    /// carries no explicit scope — the broker synthesises this well-known
    /// routing tag for the capnp `publishCursor(x, y)` path, which accepts
    /// no key of its own. Chosen as `Widget { path: [] }`: the empty widget
    /// path is unambiguously the "app root" scope (no live widget can
    /// occupy it), and its postcard encoding is stable and doesn't depend
    /// on a string tag that could drift.
    pub fn pointer_default() -> Self {
        PresenceKey::Widget { path: Vec::new() }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SelectionRange {
    pub start: usize,
    pub end: usize,
}

impl SelectionRange {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
    pub fn is_caret(&self) -> bool {
        self.start == self.end
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Which system app a presence key or a permission scope refers to.
///
/// This is an *identity*, not a policy: it names an app so a presence
/// cursor can be scoped to it. The stable string [`AppKind::app_id`]
/// returns is the same app id the compositor's portal and a
/// `Principal::App` subject use.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Unknown,
}

impl AppKind {
    /// The stable app id.
    pub fn app_id(self) -> &'static str {
        match self {
            AppKind::Terminal => "terminal",
            AppKind::FileExplorer => "file-explorer",
            AppKind::Browser => "browser",
            AppKind::Settings => "settings",
            AppKind::Unknown => "unknown",
        }
    }
}

/// A person's stable id, as apps and presence spell it.
///
/// Interchangeable with [`Principal::Person`](crate::Principal::Person) — see
/// [`UserId::principal`]. Kept as a distinct newtype since presence and
/// app code pass a bare user string around and gain nothing from a
/// tagged enum.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

impl UserId {
    pub fn principal(&self) -> crate::Principal {
        crate::Principal::Person(self.0.clone())
    }
}

impl AsRef<UserId> for UserId {
    fn as_ref(&self) -> &UserId {
        self
    }
}

/// A rectangle in physical pixels — the app's viewport at capture time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The broadcastable subset of an app's view (`ui::view::View`) — the region and resource a
/// participant is looking at, minus the caret/selection.
///
/// Deliberately small and self-describing so it can travel inside the
/// existing presence frame; it's what an **attach/follow** binds the local
/// viewport to. The caret (the view's focus) is intentionally excluded: it
/// already has a dedicated presence payload (`Caret`/`Selection`) landing in
/// the per-key caret maps, so re-sending it here would double-broadcast and
/// re-key it away from the widget scopes those renderers read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewSubset {
    /// Scroll offset(s) of the participant's scrollable content.
    pub scroll: Vec<f64>,
    /// The resource the participant currently has open, if any.
    pub open_resource: Option<String>,
    /// The participant's viewport rect in physical pixels.
    pub viewport: ViewRect,
    /// The participant's current view mode, if the app has one.
    pub view_mode: Option<String>,
}

impl ViewSubset {
    /// True at the starting point of a freshly-launched app that has not
    /// scrolled, opened a resource, or been laid out — lets the broadcaster
    /// skip the first frame's no-op send.
    pub fn is_empty(&self) -> bool {
        *self == ViewSubset::default()
    }
}

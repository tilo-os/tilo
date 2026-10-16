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
        app: crate::permissions::AppKind,
        sub: String,
    },
    /// A participant's broadcast [`ViewSubset`](crate::view::ViewSubset),
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
    /// that surface announces ([`crate::local_surface_id`]). Named by the
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
    /// a scope does not fit a bounded carrier, e.g. the 64-byte LIVE_V1
    /// anchor, whose Writer keys can name 130-byte block ids.
    pub fn scope_digest(&self) -> Option<[u8; 16]> {
        let bytes = postcard::to_stdvec(self).ok()?;
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

    /// Wrap a [`ViewSubset`](crate::view::ViewSubset) as a presence key for
    /// broadcast. Encode failure yields empty bytes, which [`Self::as_view`]
    /// decodes back to `None` — a dropped frame, never a panic.
    pub fn view(subset: &crate::view::ViewSubset) -> Self {
        PresenceKey::View {
            data: postcard::to_stdvec(subset).unwrap_or_default(),
        }
    }

    /// Decode a broadcast [`ViewSubset`](crate::view::ViewSubset) back out
    /// of a [`PresenceKey::View`]. `None` for any other variant, or for
    /// `View` bytes that fail to decode.
    pub fn as_view(&self) -> Option<crate::view::ViewSubset> {
        match self {
            PresenceKey::View { data } => postcard::from_bytes(data).ok(),
            _ => None,
        }
    }

    /// The scope this process should publish its presence under — the room
    /// it collaborates in, or `None` when it has no room. Read from
    /// [`local_cluster_id`](crate::collaboration::local_cluster_id), the
    /// same value the routing address is built from, so the two cannot drift apart.
    ///
    /// `None` when the room is the nil uuid: that value addresses nobody
    /// (`CrdtRoute::is_addressable` rejects it — an unminted room would
    /// otherwise make every unconfigured machine share one room). Fail closed.
    pub fn for_local_workspace() -> Option<Self> {
        let cluster = crate::collaboration::local_cluster_id();
        if cluster.is_nil() {
            return None;
        }
        Some(PresenceKey::Workspace {
            id: cluster.to_string(),
        })
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

    /// [`Self::surface_pointer`] for this process's own surface. The
    /// coordinates a winit `CursorMoved` carries are surface-local, so this
    /// is the only representation an app can provide.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn local_surface_pointer() -> Self {
        Self::surface_pointer(crate::local_surface_id())
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

#[cfg(test)]
mod view_key_tests {
    use super::*;
    use crate::view::{ViewRect, ViewSubset};

    #[test]
    fn view_keys_round_trip_their_subset_and_nothing_else() {
        // phase: view_key_round_trips_the_subset
        {
            let subset = ViewSubset {
                scroll: vec![12.0, 34.0],
                open_resource: Some("file:/notes.md".into()),
                viewport: ViewRect {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                },
                view_mode: Some("list".into()),
            };
            let key = PresenceKey::view(&subset);
            assert!(matches!(key, PresenceKey::View { .. }));
            assert_eq!(
                key.as_view()
                    .expect("a View key decodes back to its subset"),
                subset
            );
        }
        // phase: as_view_is_none_for_other_key_variants
        {
            assert!(PresenceKey::Tab { id: "t".into() }.as_view().is_none());
            assert!(
                PresenceKey::Workspace { id: "w".into() }
                    .as_view()
                    .is_none()
            );
            assert!(PresenceKey::pointer_default().as_view().is_none());
            // Garbage bytes in a View key decode to None rather than panicking.
            assert!(
                PresenceKey::View {
                    data: vec![0xff, 0x00, 0x13]
                }
                .as_view()
                .is_none()
            );
        }
    }

    #[test]
    fn the_presence_scope_is_the_workspace_room_or_nothing() {
        // phase: the_presence_scope_is_the_room_the_documents_go_to
        // The presence scope is the room, and it is the *same* room the
        // collaboration address uses — deriving the two separately is how a
        // window ends up announcing into one room while addressing documents to
        // another. It's also what makes a collaborator's cursor follow them from
        // the editor into the file manager: neither app names itself.
        {
            let room = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ID", room);
            assert_eq!(
                PresenceKey::for_local_workspace(),
                Some(PresenceKey::Workspace {
                    id: room.to_string()
                })
            );
            assert_eq!(
                crate::collaboration::local_cluster_id().to_string(),
                room,
                "presence must be scoped from the same value the routing uses"
            );
        }
        // phase: no_workspace_means_no_scope
        // The nil uuid addresses nobody, so no workspace means no scope.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_WORKSPACE_ID");
            assert!(crate::collaboration::local_cluster_id().is_nil());
            assert_eq!(PresenceKey::for_local_workspace(), None);
        }
        // phase: empty_workspace_id_is_treated_as_unset
        // An env var set to the empty string is what a shell script produces
        // when the variable it meant to forward was itself unset. Taking it
        // literally would put every such app in one room.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ID", "");
            assert_eq!(PresenceKey::for_local_workspace(), None);
        }
    }

    #[test]
    fn pointer_keys_pin_their_ordinals_and_carry_their_space() {
        // phase: the_pointer_key_ordinals_are_the_ones_the_bridge_mirrors
        // The ordinals in the postcard key wire contract are written
        // against, pinned as bytes.
        //
        // Postcard tags variants by ordinal, so inserting `Pointer` anywhere but
        // last, or swapping `Desktop` and `Surface`, decodes on the bridge side as
        // a different variant or not at all — and a pointer publish that silently
        // stops arriving looks exactly like a peer who is not moving. The two
        // enums are separate declarations in separate crates and neither mentions
        // the other, so this is the only guard.
        {
            assert_eq!(
                postcard::to_stdvec(&PresenceKey::desktop_pointer("r")).unwrap(),
                vec![5u8, 0, 1, b'r'],
                "PresenceKey::Pointer must stay variant 5 and PointerSpace::Desktop variant 0",
            );
            assert_eq!(
                postcard::to_stdvec(&PresenceKey::surface_pointer("s")).unwrap(),
                vec![5u8, 1, 1, b's'],
                "PointerSpace::Surface must stay variant 1",
            );
            assert_eq!(
                postcard::to_stdvec(&PresenceKey::tile_pointer("r", 7)).unwrap(),
                vec![5u8, 2, 1, b'r', 7],
                "PointerSpace::Tile must stay variant 2",
            );
        }
        // phase: a_pointer_key_carries_its_space_across_the_wire_encoding
        // The space is what a pointer key is *for*, and it has to survive the
        // encoding the wire uses — the whole failure was a space that existed at
        // one end and not the other.
        {
            for key in [
                PresenceKey::desktop_pointer("room-7"),
                PresenceKey::surface_pointer("so.tilo.word-processor"),
                PresenceKey::tile_pointer("room-7", 0x1234_5678_9abc),
            ] {
                let bytes = postcard::to_stdvec(&key).expect("a pointer key encodes");
                let back: PresenceKey =
                    postcard::from_bytes(&bytes).expect("and decodes to the same key");
                assert_eq!(back, key, "the space must survive the round trip");
                assert!(
                    back.pointer_space().is_some(),
                    "a pointer key with no readable space is the bug this variant exists to stop",
                );
            }
        }
        // phase: two_surfaces_are_two_spaces
        // Two surfaces are two spaces. A file manager cannot answer for a
        // position measured in a word processor, and this is where that is
        // decided — before anything has been drawn, from the key alone.
        {
            let wp = PresenceKey::surface_pointer("so.tilo.word-processor");
            let fm = PresenceKey::surface_pointer("so.tilo.files");
            assert_ne!(wp.pointer_space(), fm.pointer_space());
            assert!(!wp.scopes_a_desktop_pointer());
            assert!(!fm.scopes_a_desktop_pointer());
        }
        // phase: every_spelling_of_the_desktop_reads_as_the_desktop
        // The two older spellings of the desktop still mean it: a session's
        // room-scoped republish, and the tag the keyless capnp cursor path was
        // re-keyed to.
        {
            assert!(PresenceKey::desktop_pointer("r").scopes_a_desktop_pointer());
            assert!(PresenceKey::Workspace { id: "r".into() }.scopes_a_desktop_pointer());
            assert!(PresenceKey::pointer_default().scopes_a_desktop_pointer());
            // A pointer placed over a tab's tile is drawn by the compositor
            // above every window, like any desktop pointer.
            assert!(PresenceKey::tile_pointer("r", 1).scopes_a_desktop_pointer());
            // A position inside a document is not a position on a screen.
            assert!(
                !PresenceKey::Widget {
                    path: vec![PathSegment::Index(0)]
                }
                .scopes_a_desktop_pointer()
            );
            assert_eq!(
                PresenceKey::Tab { id: "t".into() }.pointer_space(),
                None,
                "a tab is an object, not a coordinate space",
            );
        }
    }
}

#[cfg(test)]
mod scope_digest_tests {
    use super::*;

    #[test]
    fn digest_form_is_stable_distinct_and_self_describing() {
        let key = |id: &str| PresenceKey::Widget {
            path: vec![
                PathSegment::Field("wp_block".into()),
                PathSegment::Field(id.into()),
            ],
        };
        let long = format!("{}.7", "ab".repeat(64));
        let a = key(&long);
        assert_eq!(a.scope_digest(), key(&long).scope_digest());
        assert_ne!(a.scope_digest(), key("other").scope_digest());
        // Known vector shared with the LIVE_V1 anchor tests (host + browser).
        assert_eq!(
            hex::encode(key("b1").scope_digest().unwrap()),
            SCOPE_DIGEST_B1
        );
        let PresenceKey::Widget { path } = a.digest_form().unwrap() else {
            panic!()
        };
        assert_eq!(path[0], PathSegment::Field(DIGEST_KEY_FIELD.into()));
        assert_eq!(
            a.digest_form(),
            Some(PresenceKey::from_scope_digest(a.scope_digest().unwrap()))
        );
    }

    /// `hex(scope_digest(Widget{[wp_block, "b1"]}))`.
    const SCOPE_DIGEST_B1: &str = "9c59c4ded04a69ccf5b2ac58797c3b77";
}

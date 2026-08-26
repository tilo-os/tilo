use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PathSegment {
    Field(String),
    Index(u32),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PresenceKey {
    Widget { path: Vec<PathSegment> },
    Tab { id: String },
    Workspace { id: String },
    App { app: crate::permissions::AppKind, sub: String },
    /// A participant's broadcast [`ViewSubset`](crate::view::ViewSubset),
    /// carried as its postcard encoding.
    ///
    /// The View — where a participant is looking (scroll / open resource /
    /// viewport / view mode) — has no `PresencePayload` variant of its own,
    /// so it rides the existing presence frame inside the routing key
    /// instead: the sender pairs this key with a benign payload and the
    /// receiver recognises the variant and stores the decoded subset per
    /// peer (see [`PresenceCtx::set_peer_view`](crate::PresenceCtx::set_peer_view))
    /// rather than treating it as a scope. Bytes rather than a live struct
    /// because the key must stay `Hash + Eq`, which `f64` viewport fields
    /// are not.
    View { data: Vec<u8> },
    /// A pointer position, together with the coordinate space it was measured
    /// in. Every other variant is a scope; the scope of a pointer is the space
    /// its numbers mean something in. See [`PointerSpace`].
    Pointer { space: PointerSpace },
}

/// The coordinate space a pointer position is measured in.
///
/// A pointer is a pair of numbers and they mean nothing on their own: the same
/// `(230, 34)` is a place on the desktop, a place in a file manager and a place
/// in a word processor. So the space travels with the position, and it is the
/// space — not the transport, not the receiving widget tree — that decides who
/// may place it.
///
/// It also decides who may *not*, which is the property worth having. A
/// presence frame is delivered to every app in a room, so without this each of
/// them read the position as its own and one person was drawn once per open
/// window, in the same place in each. With the space carried, at most one
/// receiver can answer "that is my space", and the rest can tell it is not
/// theirs without comparing notes.
///
/// **Variant order is a wire contract.** `collab-bridge`'s `wire::PointerSpace`
/// mirrors this enum and postcard tags variants by ordinal, so a reordering
/// here silently drops every pointer publish rather than failing to build.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PointerSpace {
    /// Compositor scene coordinates on `room`'s desktop.
    ///
    /// The only space that spans more than one window, so the only one a
    /// pointer can be followed across the strip and the gaps between windows
    /// in — and therefore the only space a *person's* pointer is ever in. The
    /// compositor owns it, being the only layer that knows where a window is.
    Desktop { room: String },
    /// Content coordinates inside the surface `surface` names — the app id
    /// that surface announces ([`crate::local_surface_id`]).
    ///
    /// Named by the app rather than by the window instance: what makes two
    /// content coordinates comparable is that they are measured in the same
    /// content, and two windows of one app on one document are the case that
    /// wants a peer's cursor drawn in it. A file manager and a word processor
    /// are not, and cannot claim each other's positions even though the broker
    /// hands both the same frame.
    Surface { surface: String },
}

impl PresenceKey {
    /// Wrap a [`ViewSubset`](crate::view::ViewSubset) as a presence key for
    /// broadcast. Encode failure (unreachable for these owned, self-
    /// describing fields) yields empty bytes, which [`Self::as_view`]
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

    /// The scope this process should publish its presence under: the room
    /// it collaborates in, or `None` when it has no room.
    ///
    /// "Who else is here" is a fact about the room — the broker widens
    /// every presence frame to cluster scope for exactly that reason, so
    /// that someone editing a document and someone browsing files in one
    /// workspace see each other. The scope therefore has to *be* the room,
    /// and it is read from
    /// [`local_cluster_id`](crate::collaboration::local_cluster_id) — the
    /// same value the routing address is built from, so the two cannot
    /// drift apart.
    ///
    /// `None` when the room is the nil uuid, which is what "no workspace
    /// in the environment" looks like. That value addresses nobody
    /// (`CrdtRoute::is_addressable` rejects it, on the grounds that an
    /// unminted room would otherwise make every workspace on every machine
    /// one shared room), so publishing under it would be announcing into a
    /// room no one can be in. Fail closed and stay quiet.
    pub fn for_local_workspace() -> Option<Self> {
        let cluster = crate::collaboration::local_cluster_id();
        if cluster.is_nil() {
            return None;
        }
        Some(PresenceKey::Workspace { id: cluster.to_string() })
    }

    /// The key a pointer measured on `room`'s desktop is published under.
    pub fn desktop_pointer(room: impl Into<String>) -> Self {
        PresenceKey::Pointer {
            space: PointerSpace::Desktop { room: room.into() },
        }
    }

    /// The key a pointer measured inside `surface`'s content is published
    /// under.
    pub fn surface_pointer(surface: impl Into<String>) -> Self {
        PresenceKey::Pointer {
            space: PointerSpace::Surface { surface: surface.into() },
        }
    }

    /// [`Self::surface_pointer`] for this process's own surface. The
    /// coordinates a winit `CursorMoved` carries are surface-local, so this is
    /// the only honest thing an app can say about them — the offset it would
    /// need to say more is the compositor's.
    pub fn local_surface_pointer() -> Self {
        Self::surface_pointer(crate::local_surface_id())
    }

    /// The coordinate space a pointer arriving under this key is measured in,
    /// or `None` for a key that is not a pointer scope at all.
    ///
    /// One place, because the answer used to be reached by three routes that
    /// disagreed. Two older spellings still mean "desktop" and are read here
    /// rather than re-derived at each call site: [`PresenceKey::Workspace`],
    /// the session's own desktop pointer republished per room, and
    /// [`Self::pointer_default`], the tag the keyless capnp `publishCursor`
    /// path was re-keyed to. Anything else is a position in a document rather
    /// than on a screen.
    pub fn pointer_space(&self) -> Option<PointerSpace> {
        match self {
            PresenceKey::Pointer { space } => Some(space.clone()),
            PresenceKey::Workspace { id } => Some(PointerSpace::Desktop { room: id.clone() }),
            PresenceKey::Widget { path } if path.is_empty() => {
                Some(PointerSpace::Desktop { room: String::new() })
            }
            _ => None,
        }
    }

    /// Whether a pointer arriving under this key is a position on the
    /// *desktop* — one the compositor overlay places, above every window —
    /// rather than a position inside one window's content.
    ///
    /// Reading the legacy keyless tag as an ordinary widget scope is what drew
    /// one person once per window: the broker hands every inbound frame to
    /// every app in the room, so each window claimed the pointer as its own.
    ///
    /// Deliberately about the *scope*, not the transport: a pointer that
    /// reaches a window with no key at all (the relay dialect, the hosted
    /// backend's interpolated cursors) never comes through here and stays the
    /// window's to paint, because nothing is above it that could.
    pub fn scopes_a_desktop_pointer(&self) -> bool {
        matches!(self.pointer_space(), Some(PointerSpace::Desktop { .. }))
    }

    /// The canonical `PresenceKey` used to attribute a pointer
    /// (cursor) publish that carries no explicit scope. Used by the
    /// capnp `publishCursor(x, y)` path which does not accept a key —
    /// the broker synthesises this well-known routing tag so peers
    /// can filter on it.
    ///
    /// Chosen as `Widget { path: [] }` because:
    /// - The empty widget path is unambiguously the "app root"
    ///   scope; no live widget can occupy it (every real path has at
    ///   least one segment).
    /// - The postcard encoding is stable (variant 0 + length-0 vec)
    ///   and does not depend on any string tag that could drift.
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
    fn view_key_round_trips_the_subset() {
        let subset = ViewSubset {
            scroll: vec![12.0, 34.0],
            open_resource: Some("file:/notes.md".into()),
            viewport: ViewRect { x: 0.0, y: 0.0, width: 800.0, height: 600.0 },
            view_mode: Some("list".into()),
        };
        let key = PresenceKey::view(&subset);
        assert!(matches!(key, PresenceKey::View { .. }));
        assert_eq!(
            key.as_view().expect("a View key decodes back to its subset"),
            subset
        );
    }

    /// The presence scope is the room, and it is the *same* room the
    /// collaboration address uses. Deriving the two separately is how a
    /// window ends up announcing itself into one room while addressing its
    /// documents to another, with nothing to report the disagreement.
    ///
    /// It is also what makes a collaborator's cursor follow them from the
    /// editor into the file manager: neither app names itself, so both
    /// publish under one scope.
    #[test]
    #[serial_test::serial]
    fn the_presence_scope_is_the_room_the_documents_go_to() {
        let room = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
        // SAFETY: serialised against the other env-reading tests here.
        unsafe {
            std::env::set_var("QOS_WORKSPACE_ID", room);
            std::env::remove_var("QUICK_CLUSTER_ID");
        }
        assert_eq!(
            PresenceKey::for_local_workspace(),
            Some(PresenceKey::Workspace { id: room.to_string() })
        );
        assert_eq!(
            crate::collaboration::local_cluster_id().to_string(),
            room,
            "presence must be scoped from the same value the routing uses"
        );
        unsafe { std::env::remove_var("QOS_WORKSPACE_ID") };
    }

    /// No workspace in the environment means no room, and the nil uuid
    /// addresses nobody — the broker rejects it so that an unminted room
    /// does not become one shared room for every workspace on the machine.
    /// Publishing under it would announce into a room no one can be in.
    #[test]
    #[serial_test::serial]
    fn no_workspace_means_no_scope() {
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("QOS_WORKSPACE_ID");
            std::env::remove_var("QUICK_CLUSTER_ID");
        }
        assert!(crate::collaboration::local_cluster_id().is_nil());
        assert_eq!(PresenceKey::for_local_workspace(), None);
    }

    /// An env var set to the empty string is what a shell script produces
    /// when the variable it meant to forward was itself unset. Taking it
    /// literally would put every such app in one room.
    #[test]
    #[serial_test::serial]
    fn empty_workspace_id_is_treated_as_unset() {
        // SAFETY: see above.
        unsafe {
            std::env::set_var("QOS_WORKSPACE_ID", "");
            std::env::remove_var("QUICK_CLUSTER_ID");
        }
        assert_eq!(PresenceKey::for_local_workspace(), None);
        unsafe { std::env::remove_var("QOS_WORKSPACE_ID") };
    }

    /// The ordinals `collab-bridge`'s `wire::PointerSpace` mirror is written
    /// against, pinned as bytes.
    ///
    /// Postcard tags variants by ordinal, so inserting `Pointer` anywhere but
    /// last, or swapping `Desktop` and `Surface`, decodes on the bridge side as
    /// a different variant or not at all — and a pointer publish that silently
    /// stops arriving looks exactly like a peer who is not moving. The two
    /// enums are separate declarations in separate crates and neither mentions
    /// the other, so this is the only guard.
    #[test]
    fn the_pointer_key_ordinals_are_the_ones_the_bridge_mirrors() {
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
    }

    /// The space is what a pointer key is *for*, and it has to survive the
    /// encoding the wire uses — the whole failure was a space that existed at
    /// one end and not the other.
    #[test]
    fn a_pointer_key_carries_its_space_across_the_wire_encoding() {
        for key in [
            PresenceKey::desktop_pointer("room-7"),
            PresenceKey::surface_pointer("dev.qos.word-processor"),
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

    /// Two surfaces are two spaces. A file manager cannot answer for a
    /// position measured in a word processor, and this is where that is
    /// decided — before anything has been drawn, from the key alone.
    #[test]
    fn two_surfaces_are_two_spaces() {
        let wp = PresenceKey::surface_pointer("dev.qos.word-processor");
        let fm = PresenceKey::surface_pointer("dev.qos.files");
        assert_ne!(wp.pointer_space(), fm.pointer_space());
        assert!(!wp.scopes_a_desktop_pointer());
        assert!(!fm.scopes_a_desktop_pointer());
    }

    /// The two older spellings of the desktop still mean it: a session's
    /// room-scoped republish, and the tag the keyless capnp cursor path was
    /// re-keyed to.
    #[test]
    fn every_spelling_of_the_desktop_reads_as_the_desktop() {
        assert!(PresenceKey::desktop_pointer("r").scopes_a_desktop_pointer());
        assert!(PresenceKey::Workspace { id: "r".into() }.scopes_a_desktop_pointer());
        assert!(PresenceKey::pointer_default().scopes_a_desktop_pointer());
        // A position inside a document is not a position on a screen.
        assert!(
            !PresenceKey::Widget { path: vec![PathSegment::Index(0)] }
                .scopes_a_desktop_pointer()
        );
        assert_eq!(
            PresenceKey::Tab { id: "t".into() }.pointer_space(),
            None,
            "a tab is an object, not a coordinate space",
        );
    }

    #[test]
    fn as_view_is_none_for_other_key_variants() {
        assert!(PresenceKey::Tab { id: "t".into() }.as_view().is_none());
        assert!(PresenceKey::Workspace { id: "w".into() }.as_view().is_none());
        assert!(PresenceKey::pointer_default().as_view().is_none());
        // Garbage bytes in a View key decode to None rather than panicking.
        assert!(PresenceKey::View { data: vec![0xff, 0x00, 0x13] }.as_view().is_none());
    }
}

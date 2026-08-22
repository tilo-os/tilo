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

    #[test]
    fn as_view_is_none_for_other_key_variants() {
        assert!(PresenceKey::Tab { id: "t".into() }.as_view().is_none());
        assert!(PresenceKey::Workspace { id: "w".into() }.as_view().is_none());
        assert!(PresenceKey::pointer_default().as_view().is_none());
        // Garbage bytes in a View key decode to None rather than panicking.
        assert!(PresenceKey::View { data: vec![0xff, 0x00, 0x13] }.as_view().is_none());
    }
}

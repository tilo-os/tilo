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

    /// The scope a window should broadcast its pointer under: the
    /// workspace it was launched into.
    ///
    /// "Who else is here" is a fact about the room — the broker widens
    /// every presence frame to cluster scope for exactly that reason, so
    /// that someone editing a document and someone browsing files in the
    /// same workspace see each other's cursor. An app that keys its
    /// broadcast by its own name instead has two windows in one workspace
    /// claiming two different scopes; today that stays invisible only
    /// because the ghost-cursor overlay ignores the key, while everything
    /// that does read it — [`PresenceCtx::pointers`](crate::PresenceCtx),
    /// `pointer_for`, the shell's per-workspace overlay — silently sees
    /// one room per app.
    ///
    /// `QOS_WORKSPACE_ID` is what the launcher stamps on a sandboxed app
    /// (the workspace's room id); `QUICK_CLUSTER_ID` is the same value for
    /// an app started by hand. Outside a session neither is set, so
    /// `fallback` keeps a standalone `cargo run` on a stable scope of its
    /// own rather than lumping every unlaunched app into one empty room.
    pub fn workspace_scope(fallback: &str) -> Self {
        let id = ["QOS_WORKSPACE_ID", "QUICK_CLUSTER_ID"]
            .into_iter()
            .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))
            .unwrap_or_else(|| fallback.to_string());
        PresenceKey::Workspace { id }
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

    /// Two different apps launched into one workspace must broadcast their
    /// pointer under the *same* scope — that identity is what "a
    /// collaborator's cursor follows them from the editor into the file
    /// manager" is made of. Keying by app name gives two scopes and this
    /// fails.
    #[test]
    #[serial_test::serial]
    fn apps_in_one_workspace_share_a_presence_scope() {
        let room = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
        // SAFETY: serialised against the other env-reading tests in this
        // module, and the process is single-threaded at this point.
        unsafe {
            std::env::set_var("QOS_WORKSPACE_ID", room);
            std::env::remove_var("QUICK_CLUSTER_ID");
        }
        let editor = PresenceKey::workspace_scope("word-processor");
        let files = PresenceKey::workspace_scope("file-explorer");
        assert_eq!(editor, files, "one workspace is one presence scope");
        assert_eq!(editor, PresenceKey::Workspace { id: room.to_string() });
        unsafe { std::env::remove_var("QOS_WORKSPACE_ID") };
    }

    /// Outside a session there is no workspace to name, and every app
    /// falling back to one shared empty id would put them all in one room.
    #[test]
    #[serial_test::serial]
    fn workspace_scope_falls_back_per_app_outside_a_session() {
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("QOS_WORKSPACE_ID");
            std::env::remove_var("QUICK_CLUSTER_ID");
        }
        assert_eq!(
            PresenceKey::workspace_scope("file-explorer"),
            PresenceKey::Workspace { id: "file-explorer".to_string() }
        );
        assert_ne!(
            PresenceKey::workspace_scope("file-explorer"),
            PresenceKey::workspace_scope("word-processor"),
        );
    }

    /// An env var set to the empty string is what a shell script produces
    /// when the variable it meant to forward was itself unset. Taking it
    /// literally would give every such app the same empty scope.
    #[test]
    #[serial_test::serial]
    fn empty_workspace_id_is_treated_as_unset() {
        // SAFETY: see above.
        unsafe {
            std::env::set_var("QOS_WORKSPACE_ID", "");
            std::env::remove_var("QUICK_CLUSTER_ID");
        }
        assert_eq!(
            PresenceKey::workspace_scope("notes"),
            PresenceKey::Workspace { id: "notes".to_string() }
        );
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

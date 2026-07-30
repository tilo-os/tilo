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

    #[test]
    fn as_view_is_none_for_other_key_variants() {
        assert!(PresenceKey::Tab { id: "t".into() }.as_view().is_none());
        assert!(PresenceKey::Workspace { id: "w".into() }.as_view().is_none());
        assert!(PresenceKey::pointer_default().as_view().is_none());
        // Garbage bytes in a View key decode to None rather than panicking.
        assert!(PresenceKey::View { data: vec![0xff, 0x00, 0x13] }.as_view().is_none());
    }
}

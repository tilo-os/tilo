//! Persistent UI settings, stored in the shared Automerge document.
//!
//! Selection and transient view state live in the widget (they are
//! ephemeral and per-instance), but durable preferences — which diff
//! layout the user prefers, whether whitespace is shown, the default
//! history author filter — belong in the CRDT so they survive restarts
//! and replicate to collaborators. The restart half of that comes from
//! `main`, which opens the document through `ui::persist` and installs the
//! writer with `App::with_autosave`. This module is the typed accessor in
//! front of the raw `root.settings` map.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use ui::doc::{read_bool, read_str};

/// How the diff pane lays out old vs. new content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLayout {
    /// Single column with +/- gutter markers.
    Unified,
    /// Two columns: removed on the left, added on the right.
    SideBySide,
}

impl DiffLayout {
    /// The document's string form (`"unified"` | `"split"`). Public so the
    /// preference [`crate::actions`] can carry the target layout in its payload
    /// and write the identical shape [`Settings::write`] does.
    pub fn as_str(self) -> &'static str {
        match self {
            DiffLayout::Unified => "unified",
            DiffLayout::SideBySide => "split",
        }
    }
}

impl std::str::FromStr for DiffLayout {
    /// Parsing never fails: any unrecognised form defaults to
    /// [`DiffLayout::Unified`] rather than erroring, so a bad replay can't
    /// wedge a peer.
    type Err = std::convert::Infallible;

    /// Parse the document's string form back into the enum, defaulting to
    /// [`DiffLayout::Unified`] for any unknown value.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "split" => DiffLayout::SideBySide,
            _ => DiffLayout::Unified,
        })
    }
}

/// Typed snapshot of the durable settings, read from / written to the doc.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub diff_layout: DiffLayout,
    pub show_whitespace: bool,
    pub syntax_highlight: bool,
    pub wrap_lines: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            diff_layout: DiffLayout::Unified,
            show_whitespace: false,
            syntax_highlight: true,
            // Code wraps inside its pane by default, so no line is cut at
            // the pane edge; turning it off scrolls long lines sideways.
            wrap_lines: true,
        }
    }
}

/// Document keys for the durable settings, shared by [`Settings`] and the
/// preference [`crate::actions`] so both read and write the identical shape.
pub mod keys {
    /// The settings map under the document root.
    pub const SETTINGS: &str = "settings";
    pub const DIFF_LAYOUT: &str = "diff_layout";
    pub const SHOW_WHITESPACE: &str = "show_whitespace";
    pub const SYNTAX_HIGHLIGHT: &str = "syntax_highlight";
    pub const WRAP_LINES: &str = "wrap_lines";
}

/// Resolve (creating if absent) the `root.settings` map. Crate-visible so the
/// preference [`crate::actions`] mutate the same object the app seeds here.
///
/// `put_object` on the document's own root can only fail if the root itself
/// no longer exists, which never happens for a live `AutoCommit` — see
/// `ui::widget::mod::TaffyTreeExt` for the equivalent taffy-side argument.
#[allow(
    clippy::expect_used,
    reason = "put_object on the document root cannot fail; see doc comment"
)]
pub(crate) fn settings_obj(doc: &mut AutoCommit) -> ObjId {
    match doc.get(&automerge::ROOT, keys::SETTINGS) {
        Ok(Some((Value::Object(ObjType::Map), id))) => id,
        _ => doc
            .put_object(&automerge::ROOT, keys::SETTINGS, ObjType::Map)
            .expect("put settings map"),
    }
}

/// Resolve the `root.settings` map without creating it, for read-only callers
/// (the Ctrl+K resource tree gets an immutable `&AutoCommit`).
fn settings_obj_ref(doc: &AutoCommit) -> Option<ObjId> {
    match doc.get(&automerge::ROOT, keys::SETTINGS) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

impl Settings {
    /// Read the current settings out of the document, falling back to the
    /// defaults for any unset key. Creating the map if absent so the first
    /// read also seeds it.
    pub fn read(doc: &mut AutoCommit) -> Self {
        let obj = settings_obj(doc);
        Self::from_obj(doc, &obj)
    }

    /// Read the current settings from an immutable document, without creating
    /// the settings map. Returns the defaults when the map is absent. Used by
    /// the Ctrl+K resource tree, which only borrows the document.
    pub fn read_only(doc: &AutoCommit) -> Self {
        match settings_obj_ref(doc) {
            Some(obj) => Self::from_obj(doc, &obj),
            None => Settings::default(),
        }
    }

    /// Read every field out of an already-resolved settings map, falling back
    /// to the defaults for any unset key.
    fn from_obj(doc: &AutoCommit, obj: &ObjId) -> Self {
        let def = Settings::default();
        let layout = read_str(doc, obj, keys::DIFF_LAYOUT)
            .map_or(def.diff_layout, |s| s.parse().unwrap_or(def.diff_layout));
        Self {
            diff_layout: layout,
            show_whitespace: read_bool(doc, obj, keys::SHOW_WHITESPACE)
                .unwrap_or(def.show_whitespace),
            syntax_highlight: read_bool(doc, obj, keys::SYNTAX_HIGHLIGHT)
                .unwrap_or(def.syntax_highlight),
            wrap_lines: read_bool(doc, obj, keys::WRAP_LINES).unwrap_or(def.wrap_lines),
        }
    }

    /// Persist this snapshot into the document.
    pub fn write(&self, doc: &mut AutoCommit) {
        let obj = settings_obj(doc);
        let _ = doc.put(&obj, keys::DIFF_LAYOUT, self.diff_layout.as_str());
        let _ = doc.put(&obj, keys::SHOW_WHITESPACE, self.show_whitespace);
        let _ = doc.put(&obj, keys::SYNTAX_HIGHLIGHT, self.syntax_highlight);
        let _ = doc.put(&obj, keys::WRAP_LINES, self.wrap_lines);
    }

    pub fn toggle_layout(&mut self) {
        self.diff_layout = match self.diff_layout {
            DiffLayout::Unified => DiffLayout::SideBySide,
            DiffLayout::SideBySide => DiffLayout::Unified,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings: defaults when unset and a round trip through the document. Each phase is a former test with its own fresh fixture.
    #[test]
    fn settings_default_when_unset_and_round_trip_through_doc() {
        defaults_when_unset();
        round_trips_through_doc();
    }

    fn round_trips_through_doc() {
        let mut doc = AutoCommit::new();
        let s = Settings {
            diff_layout: DiffLayout::SideBySide,
            show_whitespace: true,
            syntax_highlight: false,
            ..Settings::default()
        };
        s.write(&mut doc);

        let back = Settings::read(&mut doc);
        assert_eq!(back.diff_layout, DiffLayout::SideBySide);
        assert!(back.show_whitespace);
        assert!(!back.syntax_highlight);
    }

    fn defaults_when_unset() {
        let mut doc = AutoCommit::new();
        let s = Settings::read(&mut doc);
        assert_eq!(s.diff_layout, DiffLayout::Unified);
        assert!(s.syntax_highlight);
    }
}

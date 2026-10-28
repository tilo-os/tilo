//! Query views a person kept: their own views beside the app's designed
//! ones, such as a smart playlist or a saved filter.
//!
//! A kept view is the person's, not the room's: it follows them to their
//! other devices and no collaborator sees it. It is a personal setting of the
//! app ([`KeptViews`]), read with `settings::<KeptViews>()` from a view or
//! surface context, so it survives a restart.

use serde::{Deserialize, Serialize};
use ui_core::storage::{Scope, SettingsSchema};

use super::kind::{KindDecl, filters};
use super::query::QuerySpec;

/// One kept query view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptView {
    pub id: String,
    /// What the person calls it; at first what was searched for.
    pub name: String,
    pub query: QuerySpec,
}

/// The query views a person kept in one app, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptViews {
    #[serde(default)]
    pub kept_views: Vec<KeptView>,
}

impl SettingsSchema for KeptViews {
    const SCOPE: Scope = Scope::Personal;
}

impl KeptViews {
    /// Keep `query` as a view named `name`, and return its id. A query kept
    /// already is not kept twice: its id comes back.
    pub fn keep(&mut self, query: &QuerySpec, name: impl Into<String>) -> String {
        let key = query.key();
        if let Some(kept) = self.kept_views.iter().find(|kept| kept.query.key() == key) {
            return kept.id.clone();
        }
        let mut stamp = ui_core::time::unix_now_ms();
        let mut id = format!("kept-{stamp}");
        while self.get(&id).is_some() {
            stamp = stamp.wrapping_add(1);
            id = format!("kept-{stamp}");
        }
        self.kept_views.push(KeptView {
            id: id.clone(),
            name: name.into(),
            query: query.clone(),
        });
        id
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&KeptView> {
        self.kept_views.iter().find(|kept| kept.id == id)
    }

    /// Stop keeping the view `id`. Returns whether it was kept.
    pub fn forget(&mut self, id: &str) -> bool {
        let before = self.kept_views.len();
        self.kept_views.retain(|kept| kept.id != id);
        self.kept_views.len() != before
    }

    /// Call the view `id` `name`; a blank name changes nothing.
    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        match self.kept_views.iter_mut().find(|kept| kept.id == id) {
            Some(kept) if !name.is_empty() && kept.name != name => {
                name.clone_into(&mut kept.name);
                true
            }
            _ => false,
        }
    }

    /// The name a view of `query` starts with: what was typed, then its
    /// chips as they read ("ada", "Liked").
    #[must_use]
    pub fn default_name(query: &QuerySpec, kinds: &[KindDecl]) -> String {
        let fields = filters(kinds);
        let mut parts = Vec::new();
        let text = query.text.trim();
        if !text.is_empty() {
            parts.push(text.to_owned());
        }
        for chip in &query.chips {
            let field = fields.iter().copied().find(|field| field.key == chip.key);
            parts.push(chip.label(field));
        }
        parts.join(", ")
    }
}

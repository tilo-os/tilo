//! `OverviewView` — vertical list of workspace mini-cards (Cmd+Tab target).
//!
//! Holds a plain-data [`CardCache`] snapshot of the current `WorkspaceTree` so
//! the shell can render the overview as a tall column of cards, with the
//! active card highlighted by an accent border. Clicking a card activates the
//! matching workspace on the tree.
//!
//! v1 keeps the render side as composable `quick_ui` widgets (`Column` of
//! `Container`s) built by the caller from [`OverviewView::cards`]; wiring into
//! the shell root widget tree is the subject of later Plan-2 tasks. This
//! module only owns the data + activation logic that the chrome tests cover.

use crate::shell::state::{WorkspaceId, WorkspaceTree};

/// Cached, plain-data snapshot of a single workspace for rendering a card.
#[derive(Clone, Debug)]
pub struct CardCache {
    pub id: WorkspaceId,
    pub name: String,
    pub strip_count: usize,
}

/// The overview widget's logical state.
pub struct OverviewView {
    cards: Vec<CardCache>,
    active: Option<WorkspaceId>,
}

impl OverviewView {
    pub fn new() -> Self {
        Self {
            cards: Vec::new(),
            active: None,
        }
    }

    /// Rebuild the card cache from the authoritative `WorkspaceTree`.
    pub fn sync(&mut self, tree: &WorkspaceTree) {
        self.cards = tree
            .workspaces
            .iter()
            .map(|w| CardCache {
                id: w.id,
                name: w.name.clone(),
                strip_count: w.strips.len(),
            })
            .collect();
        self.active = Some(tree.active);
    }

    pub fn card_count(&self) -> usize {
        self.cards.len()
    }

    pub fn cards(&self) -> &[CardCache] {
        &self.cards
    }

    /// Activate the workspace behind the card at `idx`. Out-of-range indices
    /// are ignored.
    pub fn click_card(&mut self, idx: usize, tree: &mut WorkspaceTree) {
        if let Some(card) = self.cards.get(idx) {
            tree.set_active(card.id);
            self.active = Some(tree.active);
        }
    }

    /// Whether the card at `idx` matches the currently active workspace.
    pub fn is_active(&self, idx: usize) -> bool {
        match (self.cards.get(idx), self.active) {
            (Some(card), Some(active)) => card.id == active,
            _ => false,
        }
    }
}

impl Default for OverviewView {
    fn default() -> Self {
        Self::new()
    }
}

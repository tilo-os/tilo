//! Whether the retained layout's raw widget pointers still name live widgets.
//!
//! `layout.node_map` holds a `*mut dyn Widget` per taffy node, built from the
//! tree and kept while no widget reports a structure change. The pointers a
//! rebuild registers are adopted at once ([`App::admit_layout_maps`]), and
//! every structure check walks the live tree first
//! ([`App::layout_maps_stale`]): a widget that replaced children without
//! reporting it makes the maps stale, they are forgotten and the layout is
//! built again, instead of the window calling through a pointer to a freed
//! widget. See [`crate::retained_widgets`].

use super::App;
use crate::widget::Widget;

impl App {
    /// Every tree the layout maps can name: the app root, framework
    /// History's composition around it, the close dialog, the action
    /// palette and the standalone History chrome.
    #[inline(never)]
    fn visit_layout_roots(&self, visit: &mut dyn FnMut(&dyn Widget)) {
        visit(self.root_widget.as_ref());
        if let Some(history) = self.history.as_deref() {
            visit(history);
        }
        if let Some(dialog) = self.close_admission.dialog.as_deref() {
            visit(dialog);
        }
        if let Some(palette) = self.actions.action_palette.as_ref() {
            visit(palette);
        }
        self.document_history.for_each_widget(visit);
    }

    /// Right after a rebuild registered widgets in the maps, before any
    /// widget code runs: find each of them in the live tree.
    pub(super) fn admit_layout_maps(&mut self) {
        self.layout.admissions = self.layout.admissions.wrapping_add(1);
        let mut maps = std::mem::take(&mut self.layout.adopted);
        // SAFETY: every pointer not yet adopted was registered by the build
        // that just ran on these trees, and no widget code ran since.
        unsafe {
            maps.admit(
                &self.layout.node_map,
                &|visit| self.visit_layout_roots(visit),
                &self.layout.taffy,
            );
        }
        self.layout.adopted = maps;
    }

    /// Whether the maps name a widget that is no longer in the tree: a
    /// widget replaced children without reporting it. The maps are then
    /// forgotten and a rebuild requested, so nothing reads them first.
    pub(super) fn layout_maps_stale(&mut self) -> bool {
        if self.layout.node_map.is_empty() {
            return false;
        }
        let mut relaid = Vec::new();
        if self
            .layout
            .adopted
            .still_live(&|visit| self.visit_layout_roots(visit), &mut relaid)
        {
            let removed = crate::retained_widgets::let_go_under(
                &relaid,
                &mut self.layout.taffy,
                &mut self.layout.node_map,
            );
            self.bindings.forget(&removed);
            if self
                .focused_node
                .is_some_and(|node| removed.contains(&node))
            {
                self.focused_node = None;
            }
            return false;
        }
        self.bindings.clear();
        self.layout.node_map.clear();
        self.layout.adopted.clear();
        self.layout.request_rebuild();
        true
    }
}

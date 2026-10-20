//! Dropping files onto the content.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Resolve external content through the same spatial grammar a tab drag
    /// uses. This is intentionally an adapter over `GroupRow`'s resolver: a
    /// file, clipboard payload, and an existing app must never disagree about
    /// whether a point means a tab gap or an inter-window seam.
    pub fn content_drop_placement(&self, p: Point) -> Option<crate::shell::tab_binding::Placement> {
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: read-only use under the same stable Box<GroupRow>
            // contract as every other `group_row_ptrs` accessor.
            let row: &GroupRow = unsafe { &*ptr };
            if !row.covers(p) {
                continue;
            }
            // A live hover target is hysteretic and is exactly what the
            // person can see. Fall back to pure geometry for one-shot OS
            // drops which do not provide hover events.
            match row.tab_drop_target().or_else(|| row.tab_drop_target_at(p)) {
                Some(ui::widget::TabDropTarget::ExistingGroup {
                    group_id,
                    tab_index,
                }) => {
                    return Some(crate::shell::tab_binding::Placement::TabAt(
                        group_id, tab_index,
                    ));
                }
                Some(ui::widget::TabDropTarget::GroupGap { gap_index }) => {
                    return Some(crate::shell::tab_binding::Placement::GroupAt(
                        strip_id, gap_index,
                    ));
                }
                None => {}
            }
        }
        None
    }

    /// Open one path at the same target an in-shell tab drag would use.
    /// Public for the Wayland data-device bridge, whose release arrives in a
    /// protocol callback rather than as winit's `DroppedFile` event.
    pub fn open_dropped_path_at(&self, path: &std::path::Path, p: Point) -> bool {
        let Some(placement) = self.content_drop_placement(p) else {
            return false;
        };
        self.open_dropped_path_with_placement(path, placement)
    }

    /// Open content using a placement captured while its indicator was on
    /// screen. Asynchronous data transfer must not re-resolve against a layout
    /// which may have moved after button-up.
    pub fn open_dropped_path_with_placement(
        &self,
        path: &std::path::Path,
        placement: crate::shell::tab_binding::Placement,
    ) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        self.harvest_widget_only_groups_into_tree();
        // A drop lands in the strip at an exact seam, which only a Tilo tile
        // can honour; a host application opens its own window elsewhere, so
        // drops it would handle stay with the shell's own document tile.
        let owned = matches!(
            crate::shell::doc_open::resolve(path),
            crate::shell::doc_open::DocumentTarget::App { .. }
        );
        if owned {
            match crate::shell::doc_open::open_at(path, &chrome, placement) {
                Ok(_) => true,
                Err(error) => {
                    crate::shell::doc_open::report_open_failure(
                        &chrome,
                        path,
                        &error.user_message(),
                        None,
                    );
                    false
                }
            }
        } else {
            chrome
                .borrow_mut()
                .handle_file_drop_at_placement(path, placement)
                .is_some()
        }
    }

    pub fn has_content_drop_target(&self, p: Point) -> bool {
        self.content_drop_placement(p).is_some()
    }

    /// Paint (or retire) the content-drop preview using the same resolver
    /// whose result will be committed on release.
    pub fn set_content_drop_hover(&mut self, p: Option<Point>) {
        let selected_strip = p.and_then(|point| {
            self.group_row_ptrs.iter().find_map(|(&strip_id, &ptr)| {
                // SAFETY: read-only use under the stable row pointer contract.
                let row: &GroupRow = unsafe { &*ptr };
                row.covers(point).then_some(strip_id)
            })
        });
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: same stable Box<GroupRow> contract as the other
            // `group_row_ptrs` accessors; the frame loop owns mutation here.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            let hover = (selected_strip == Some(strip_id))
                .then_some(p)
                .flatten()
                .filter(|point| row.tab_drop_target_at(*point).is_some());
            row.set_external_content_drag_hover(hover);
        }
    }

    pub(super) fn stamp_active_group_from_click(&mut self, p: Point) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let candidates = self.group_hit_candidates();
        let Some((strip_id, group_id)) = pick_group_at(&candidates, p) else {
            return;
        };
        // Mirror any widget-only group into the tree first: the harvest
        // reconciler runs at frame boundaries, after this dispatch, and
        // an `active_group` the tree doesn't know leaves `active_tab_id`
        // as None.
        self.harvest_widget_only_groups_into_tree();
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        let ws_id = tree.desktop.active;
        let workspace_exists = tree
            .desktop
            .workspaces
            .iter()
            .any(|w| w.id == ws_id && w.strips.iter().any(|s| s.id == strip_id));
        if !workspace_exists {
            return;
        }
        tree.desktop.active_strip.insert(ws_id, strip_id);
        tree.desktop.active_group.insert(strip_id, group_id);
        tree.queue_save();
        drop(tree);
        drop(st);
        // Marks this strip at rest so a click doesn't also pan: the
        // target was visible to be clicked, and panning would drag it
        // out from under the cursor. Keyboard/wheel keep the pan, since
        // they can target off-screen windows.
        if let Some(surface) = self.desktop_surface_mut() {
            surface.keep_clicked_group_in_place(strip_id, group_id);
        }
    }
}

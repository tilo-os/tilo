//! The retained layout: when a frame may keep the last one, and whether its
//! raw widget pointers still name live widgets.
//!
//! `node_map` holds a `*mut dyn Widget` per taffy node, built from the tree
//! and kept while no widget reports a structure change. The pointers a build
//! registers are adopted at once ([`FrameHarness::admit_layout_maps`]), and
//! every structure check walks the live tree first
//! ([`FrameHarness::layout_maps_stale`]): a widget that replaced children
//! without reporting it makes the maps stale and the layout is built again,
//! instead of the frame calling through a pointer to a freed widget. See
//! [`crate::retained_widgets`].

use super::*;

impl FrameHarness {
    /// Every tree the layout maps can name: the app root, framework
    /// History's composition around it, the action palette and the
    /// standalone History chrome.
    #[inline(never)]
    fn visit_layout_roots(&self, visit: &mut dyn FnMut(&dyn Widget)) {
        visit(self.root.as_ref());
        if let Some(history) = self.history.as_deref() {
            visit(history);
        }
        if let Some(palette) = self.action_palette.as_deref() {
            visit(palette);
        }
        self.document_history.for_each_widget(visit);
    }

    /// Right after a build registered widgets in the maps, before any
    /// widget code runs: find each of them in the live tree.
    pub(super) fn admit_layout_maps(&mut self) {
        let mut maps = std::mem::take(&mut self.layout_maps);
        // SAFETY: every pointer not yet adopted was registered by the build
        // that just ran on these trees, and no widget code ran since.
        unsafe {
            maps.admit(
                &self.node_map,
                &|visit| self.visit_layout_roots(visit),
                &self.taffy,
            );
        }
        self.layout_maps = maps;
    }

    /// Whether the maps name a widget that is no longer in the tree: a
    /// widget replaced children without reporting it. The maps are then
    /// forgotten, so nothing reads them before the layout is built again.
    pub(super) fn layout_maps_stale(&mut self) -> bool {
        if self.node_map.is_empty() {
            return false;
        }
        if self
            .layout_maps
            .still_live(&|visit| self.visit_layout_roots(visit))
        {
            return false;
        }
        self.discard_layout_tree();
        true
    }
}

impl FrameHarness {
    /// Whether this frame must rebuild the layout tree regardless of what the
    /// widgets report: the test override first, then `TILO_UI_FULL_RELAYOUT`.
    pub(super) fn full_relayout_forced_now(&self) -> bool {
        self.full_relayout_override
            .unwrap_or_else(full_relayout_forced)
    }

    /// Force (`Some(true)`) or forbid (`Some(false)`) the full-rebuild path
    /// for every following frame, overriding `TILO_UI_FULL_RELAYOUT`; `None`
    /// returns to the environment's choice. Benchmarks use this to compare
    /// both paths inside one process.
    pub fn set_full_relayout(&mut self, forced: Option<bool>) {
        self.invalidate_local();
        self.full_relayout_override = forced;
    }

    /// Cumulative text measurement-cache hits and misses for this host's
    /// renderer. Compare around input to distinguish retained scrolling from
    /// remeasuring a whole page; a cold frame must first warm the counters.
    pub fn text_cache_stats(&self) -> (u64, u64) {
        self.text.cache_stats()
    }

    /// Drain this host renderer's actual shape-cache hits, misses and time
    /// spent shaping. Like `TextRenderer::take_shape_stats`, this isolates
    /// input work after a measured warmup without a wall-clock threshold.
    pub fn take_text_shape_stats(&mut self) -> (u64, u64, u128) {
        self.text.take_shape_stats()
    }

    /// What the most recent completed relayout did with its layout tree.
    pub fn last_relayout(&self) -> RelayoutStats {
        self.last_relayout
    }

    /// Completed relayout frames since construction; unchanged across a
    /// skipped (inert pointer) `send`.
    pub fn relayout_frames(&self) -> u64 {
        self.relayout_frames
    }

    /// Completed frames that recomposed the layout tree. One input may
    /// produce several frames (press and release, a commit tick), so callers
    /// asserting on a structural change compare this across the input.
    pub fn rebuilt_frames(&self) -> u64 {
        self.relayout_frames.saturating_sub(self.retained_frames)
    }

    /// Every taffy node of the current composition, sorted. Equal across two
    /// frames exactly when the tree was retained rather than rebuilt.
    pub fn layout_node_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.node_map.keys().map(|node| u64::from(*node)).collect();
        ids.compact_sort();
        ids
    }

    /// Invalidate taffy's cached measurement for every node whose widget
    /// reports it may have changed size (`is_layout_dirty`) or must be
    /// re-solved (`layout_needs_measure`), so the following
    /// `compute_widget_layout` re-measures exactly those and their ancestors.
    /// A node that cannot be inspected is invalidated too: stale is worse than
    /// a redundant measure. Returns `(marked nodes, marked childless nodes)`.
    pub(super) fn mark_layout_dirty_nodes(&mut self) -> (usize, usize) {
        let dirty: Vec<NodeId> = self
            .node_map
            .keys()
            .copied()
            .filter(|&node| {
                // SAFETY: these maps describe this exclusive runner's current
                // admitted tree; the callback only reads widget flags.
                let flagged = unsafe {
                    crate::bound_dispatch::inspect_bound_node(
                        &self.bindings,
                        &self.node_map,
                        node,
                        |widget| widget.is_layout_dirty() || widget.layout_needs_measure(),
                    )
                }
                .unwrap_or(true);
                // Hidden layout subtrees are not measured by Taffy. Their
                // widget flags can remain dirty until shown, so do not let
                // them request work on every visible edit. Showing them
                // recomposes the tree and performs the required measurement.
                // Only a flagged node walks up: the walk is per node.
                flagged && {
                    let mut ancestor = Some(node);
                    let mut shown = true;
                    while let Some(current) = ancestor {
                        if self
                            .taffy
                            .style(current)
                            .is_ok_and(|style| style.display == taffy::Display::None)
                        {
                            shown = false;
                            break;
                        }
                        ancestor = self.taffy.parent(current);
                    }
                    shown
                }
            })
            .collect();
        let mut leaves = 0usize;
        for node in &dirty {
            if self
                .taffy
                .children(*node)
                .is_ok_and(|children| children.is_empty())
            {
                leaves = leaves.saturating_add(1);
            }
            let _ = self.taffy.mark_dirty(*node);
        }
        (dirty.len(), leaves)
    }

    /// Compare the retained composition's geometry against a fresh rebuild of
    /// the same state. Runs one forced full relayout, so it is a test and
    /// diagnostics hook, not something a live host calls per frame.
    pub fn verify_retained_geometry(&mut self) -> Result<(), String> {
        self.invalidate_local();
        use crate::widget::layout::composition_geometry;
        if !self.layout_built {
            return Ok(());
        }
        let retained = composition_geometry(&self.taffy, self.root_node)
            .map_err(|error| format!("retained geometry unavailable: {error}"))?;
        // The verification frame is not one the caller asked for: keep the
        // diagnostics describing the frame under test.
        let (stats, frames, retained_frames) = (
            self.last_relayout,
            self.relayout_frames,
            self.retained_frames,
        );
        let previous = self.full_relayout_override;
        self.full_relayout_override = Some(true);
        self.verifying_geometry = true;
        let rebuilt = self.try_relayout();
        self.verifying_geometry = false;
        self.full_relayout_override = previous;
        self.last_relayout = stats;
        self.relayout_frames = frames;
        self.retained_frames = retained_frames;
        rebuilt.map_err(|error| format!("fresh rebuild did not settle: {error}"))?;
        let fresh = composition_geometry(&self.taffy, self.root_node)
            .map_err(|error| format!("fresh geometry unavailable: {error}"))?;
        if retained.len() != fresh.len() {
            return Err(format!(
                "retained composition has {} nodes, a fresh rebuild {}",
                retained.len(),
                fresh.len()
            ));
        }
        for (index, (kept, built)) in retained.iter().zip(&fresh).enumerate() {
            if kept != built {
                return Err(format!(
                    "node {index} of {}: retained (children {}, at {:?}, size {:?}) but rebuilt (children {}, at {:?}, size {:?})",
                    fresh.len(),
                    kept.0,
                    kept.1,
                    kept.2,
                    built.0,
                    built.1,
                    built.2
                ));
            }
        }
        Ok(())
    }

    /// Debug builds only: after every `every`th retained frame, verify the
    /// retained geometry against a fresh rebuild and panic on a mismatch.
    /// `0` disables the check (the default).
    pub fn set_retained_geometry_check(&mut self, every: u32) {
        self.invalidate_local();
        self.retained_geometry_check_every = if cfg!(debug_assertions) { every } else { 0 };
    }

    pub(super) fn check_retained_geometry_if_due(&mut self) {
        if self.retained_geometry_check_every == 0
            || self.verifying_geometry
            || self.last_relayout.rebuilt
            || !self
                .retained_frames
                .is_multiple_of(u64::from(self.retained_geometry_check_every))
        {
            return;
        }
        if let Err(mismatch) = self.verify_retained_geometry() {
            tracing::error!(%mismatch, "headless: retained layout diverged from a fresh rebuild");
            debug_assert!(
                false,
                "retained layout diverged from a fresh rebuild: {mismatch}"
            );
        }
    }
}

//! What the driver measures of the frames it runs: rebuilds, relayouts, redraws, profiles and
//! the work each frame queued. Every count is the app's own, kept on its frame path
//! ([`App::surface_frame_with_damage`](super::App::surface_frame_with_damage)), which is what a
//! window runs: a test that holds a frame to doing no more than it must holds the app's frame.

use super::Driver;
use crate::profiling::{FrameProfiler, FrameTiming};
use tilo_sort::CompactSort as _;
use tilo_ui_base::widget::Widget;

/// What one completed layout pass did with its layout tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelayoutStats {
    pub rebuilt: bool,
    /// Widgets changed the retained tree in place ([`tilo_ui_base::widget::StructureChange::Children`]); it
    /// was not rebuilt.
    pub remounted: bool,
    pub measured: bool,
    pub marked_dirty: usize,
    pub marked_leaves: usize,
    /// The frame placed only the widgets the views moved, so what stood still is where it stood and
    /// a long collection places only what is near its clip.
    pub placed_partly: bool,
}

/// How the driver takes the frames input asks for.
#[derive(Default)]
pub(super) struct Metrics {
    /// Input leaves its frame to the next [`Driver::poll_frame`]
    /// ([`Driver::set_coalesce_input_frames`]).
    coalesce_input: bool,
}

impl Driver {
    /// Whether the frame an input asks for waits for the next [`Self::poll_frame`].
    pub(super) fn coalesces_input(&self) -> bool {
        self.metrics.coalesce_input
    }

    /// Leave the frame a wheel, key, press or pointer move asks for to the next
    /// [`Self::poll_frame`] instead of running it inside [`Self::send`], as a host that presents on
    /// its frame clock does: a trackpad, a typist and a drag send input faster than frames, and every
    /// frame but the last would be replaced unseen. The input still reaches the app at once, against
    /// the last frame's layout, as a window hands it over; the app asks for the frame it needs.
    pub fn set_coalesce_input_frames(&mut self, coalesce: bool) {
        self.metrics.coalesce_input = coalesce;
    }

    // ---- profiling ----

    /// Time each of the app's frames from now on, by pass, and keep a sample per frame for
    /// [`Self::take_profile_samples`].
    pub fn begin_profiling(&mut self) {
        self.app.profiler = Some(FrameProfiler::for_capture());
    }

    /// Run one frame with no input, as a display asks for one, and profile it.
    pub fn profile_frame(&mut self) {
        self.run_frame();
    }

    /// Discard the samples taken so far (a warmup) and keep profiling.
    pub fn clear_profile_samples(&mut self) {
        if let Some(profiler) = self.app.profiler.as_mut() {
            profiler.clear_capture();
        }
    }

    /// Every frame sample taken since profiling began or the last take, oldest first.
    pub fn take_profile_samples(&mut self) -> Vec<FrameTiming> {
        self.app
            .profiler
            .as_mut()
            .map(FrameProfiler::take_capture)
            .unwrap_or_default()
    }

    // ---- layout ----

    /// The app's frames whose layout pass completed since it was made: a frame that kept everything
    /// counts as well as one that rebuilt.
    #[must_use]
    pub fn relayout_frames(&self) -> u64 {
        self.app.layout.frames.laid_out
    }

    /// Of [`Self::relayout_frames`], the frames that built the layout tree again.
    #[must_use]
    pub fn rebuilt_frames(&self) -> u64 {
        self.app.layout.frames.rebuilt
    }

    /// What the app's last frame did with its layout tree.
    #[must_use]
    pub fn last_relayout(&self) -> RelayoutStats {
        let pass = self.app.layout.last_pass;
        RelayoutStats {
            rebuilt: pass.rebuilt,
            remounted: pass.remounted,
            measured: pass.measured,
            marked_dirty: usize::try_from(pass.marked.0).unwrap_or(usize::MAX),
            marked_leaves: usize::try_from(pass.marked.1).unwrap_or(usize::MAX),
            placed_partly: pass.placed_partly,
        }
    }

    /// Every node of the app's layout tree, sorted. Equal across two frames exactly when the tree
    /// was kept rather than built again.
    #[must_use]
    pub fn layout_node_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .app
            .layout
            .node_map
            .keys()
            .map(|node| u64::from(*node))
            .collect();
        ids.compact_sort();
        ids
    }

    /// Compare the geometry of the app's kept layout tree with that of a tree built afresh from the
    /// same state, node by node. The frame that builds it is not one the caller asked for: the
    /// counts and the profile go on describing the frames before it.
    ///
    /// # Errors
    /// Names the first node whose children, place or size differ, or why either tree has no
    /// geometry.
    pub fn verify_retained_geometry(&mut self) -> Result<(), String> {
        use tilo_ui_base::widget::layout::composition_geometry;
        let Some(root) = self.app.layout.root_node else {
            return Ok(());
        };
        let retained = composition_geometry(&self.app.layout.taffy, root)
            .map_err(|error| format!("retained geometry unavailable: {error}"))?;
        let (pass, frames) = (self.app.layout.last_pass, self.app.layout.frames);
        let profiler = self.app.profiler.take();
        self.app.layout.request_rebuild();
        self.run_frame();
        let rebuilt = self.app.layout.frames.rebuilt > frames.rebuilt;
        self.app.profiler = profiler;
        self.app.layout.last_pass = pass;
        self.app.layout.frames = frames;
        if !rebuilt {
            return Err("the app's frame did not build its layout tree afresh".to_owned());
        }
        let root = self
            .app
            .layout
            .root_node
            .ok_or("a fresh rebuild left no layout tree")?;
        let fresh = composition_geometry(&self.app.layout.taffy, root)
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
                    "node {index} of {}: retained (children {}, at {:?}, size {:?}) but rebuilt \
                     (children {}, at {:?}, size {:?})",
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

    /// Where the app's frame stands in a window of `viewport`: how far below the window's top it
    /// starts (the window's bar and the host's chrome above it) and how tall it is, in the window's
    /// logical pixels.
    ///
    /// # Panics
    /// When `viewport` is narrower or wider than the surface: the bar stands as laid out at the
    /// surface's width, and at another its height would be a guess.
    #[must_use]
    pub fn frame_extent(&self, viewport: (u32, u32)) -> (f64, f64) {
        assert_eq!(
            viewport.0,
            self.viewport().0,
            "the frame's extent is known at the surface's width, {}, not at {}",
            self.viewport().0,
            viewport.0
        );
        let height = f64::from(viewport.1);
        let top = (self.app.layout.safe_area.top + self.app.layout.part_bar_inset).min(height);
        (top, height - top)
    }

    // ---- what frames owe and draw ----

    /// The app's frames whose model pass took up nothing that arrived and synced nothing: a frame
    /// nothing woke the app for, as a driven app has a wake scope of its own.
    #[must_use]
    pub fn quiet_frames(&self) -> u64 {
        self.app.layout.frames.quiet
    }

    /// What makes a frame owed ([`Self::frame_owed`]), by name: what an idle window would wake for.
    #[must_use]
    pub fn queued_work_sources(&self) -> Vec<&'static str> {
        let due = self
            .app
            .surface_next_frame()
            .is_some_and(|at| at <= crate::time::now());
        [
            ("redraw requested", self.surface.redraw.get()),
            (
                "window bar",
                self.bar
                    .as_deref()
                    .is_some_and(super::super::part_bar::PartBar::needs_frame),
            ),
            ("wake pending", self.app.wake_pending()),
            ("owner wake queued", self.has_pending_owner_models()),
            ("model sync owed", self.app.needs_sync),
            ("layout owed", self.app.layout.layout_dirty),
            ("structure rebuild owed", self.app.layout.structure_dirty),
            ("frame due", due),
        ]
        .into_iter()
        .filter_map(|(name, owed)| owed.then_some(name))
        .collect()
    }

    /// The path of names to every widget that asks to be drawn again or for an animation frame,
    /// from the app's root and its History, without reading what they show.
    #[must_use]
    pub fn redraw_sources(&self) -> Vec<String> {
        fn visit(widget: &dyn Widget, parent: &str, out: &mut Vec<String>) {
            if !(widget.needs_redraw() || widget.needs_animation_frame()) {
                return;
            }
            let path = format!("{parent}/{}", widget.name());
            out.push(path.clone());
            for child in widget.children() {
                visit(child, &path, out);
            }
        }
        let mut sources = Vec::new();
        visit(self.app.root_widget.as_ref(), "", &mut sources);
        if let Some(history) = self.app.history_widget() {
            visit(history, "", &mut sources);
        }
        sources
    }

    /// The draw commands the app's frames encoded since it was made: a frame that draws only what
    /// changed adds few, one that draws the whole tree adds all of it.
    #[must_use]
    pub fn drawn_commands(&self) -> u64 {
        self.app.layout.frames.drawn_commands
    }

    /// The agents' action marks the app's last frame painted: where each rests, and the pointer and
    /// name tag beside it, in surface coordinates.
    #[must_use]
    pub fn drawn_action_markers(&self) -> &[crate::action_locus::PaintedActionMarker] {
        &self.app.floating.drawn_action_markers
    }

    // ---- text ----

    /// The app's text measurement cache's hits and misses since it was made. Compared around input,
    /// they tell a scroll that kept its measurements from one that measured a page again.
    #[must_use]
    pub fn text_cache_stats(&self) -> (u64, u64) {
        self.app.text_renderer.cache_stats()
    }

    /// The app's shaping cache's hits, misses and time spent shaping since the last take.
    pub fn take_text_shape_stats(&mut self) -> (u64, u64, u128) {
        self.app.text_renderer.take_shape_stats()
    }
}

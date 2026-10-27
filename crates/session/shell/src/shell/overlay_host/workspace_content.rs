//! Output geometry, display confirmation and the per-workspace content.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Override where the band under the top bar begins
    /// (`CHROME_TOP_OFFSET_PX` by default).
    #[must_use]
    pub fn with_chrome_top(mut self, y: f32) -> Self {
        self.chrome_top_y = y;
        self
    }

    /// Tell the host which displays it is painting across, so
    /// [`GroupRow`]s can size against per-display strip widths.
    pub fn set_output_geometry(&mut self, outputs: &[OutputGeometry]) {
        if self.outputs != outputs {
            self.outputs = outputs.to_vec();
            self.reconcile_display_partition();
        }
    }

    /// The displays the host was last told about.
    pub fn output_geometry(&self) -> &[OutputGeometry] {
        &self.outputs
    }

    /// Start painting each display's number and name when Settings has
    /// asked to identify displays since the last check. The compositor
    /// calls this at its settings reload point.
    pub fn poll_display_identify(&mut self) -> bool {
        let outputs = if self.outputs.is_empty() {
            vec![OutputGeometry::unscaled(
                0.0,
                0.0,
                self.rect.width(),
                self.rect.height(),
            )]
        } else {
            self.outputs.clone()
        };
        let started = self.display_identify.poll(&outputs, Instant::now());
        if started && let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().bump();
        }
        started
    }

    /// Show each display's Identify card now, as Settings' Identify does,
    /// with `snapshot` naming and numbering the displays: for captures of
    /// the cards, which have no compositor to publish a snapshot.
    pub fn identify_displays_now(
        &mut self,
        snapshot: &settings_store::hardware::DisplayHardwareSnapshot,
    ) {
        let outputs = self.outputs.clone();
        self.display_identify.start(
            &outputs,
            Some(snapshot),
            std::time::Duration::from_millis(settings_store::display_identify::IDENTIFY_MS),
            Instant::now(),
        );
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().bump();
        }
    }

    /// Keep OS identification labels in the current shared display order.
    pub(crate) fn number_identified_displays(
        &mut self,
        layout: &settings_store::person_displays::Layout,
    ) {
        self.display_identify
            .number_by_shared_order(&self.outputs, layout);
    }

    /// Ask the person to keep or revert display transaction `transaction`,
    /// which a layout tool applied. Settings and the Super+P sheet ask in
    /// their own UI; this is for changes from anywhere else.
    pub fn show_display_confirmation(
        &mut self,
        transaction: u64,
        tool: Option<settings_store::display_tools::DisplayTool>,
    ) {
        self.layers
            .display_confirm
            .show(transaction, tool, Instant::now());
        self.bump_chrome();
    }

    /// Tell the person `tool` applied a display layout again right after
    /// they reverted it, and was refused instead of asking them again.
    pub fn report_refused_display_reapply(
        &mut self,
        tool: Option<&settings_store::display_tools::DisplayTool>,
    ) {
        let title = tool.map_or_else(
            || t("display-tool-reapply-refused-unknown"),
            |tool| {
                t_args(
                    "display-tool-reapply-refused",
                    &[("tool", ui::i18n::ArgValue::from(tool.name().to_owned()))],
                )
            },
        );
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().push_notification(title, String::new());
        }
    }

    /// Whether the keep-or-revert prompt for a tool's display change is up.
    /// It owns the keyboard and pointer while it is.
    pub fn display_confirmation_visible(&self) -> bool {
        self.layers.display_confirm.visible()
    }

    /// Re-read the transaction the prompt asks about now: close it if it
    /// was answered elsewhere, revert it if its time ran out. Returns
    /// whether the prompt closed.
    pub fn poll_display_confirmation(&mut self) -> bool {
        let closed = self.layers.display_confirm.poll(Instant::now(), true);
        if closed && let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().bump();
        }
        closed
    }

    /// Where the prompt's card sits on each display.
    pub fn display_confirmation_cards(&self) -> Vec<crate::shell::display_confirm::ConfirmCard> {
        self.layers.display_confirm.cards(&self.layer_outputs())
    }

    /// Every display's rect; the host's bounds alone before it knows of any.
    pub(in crate::shell) fn layer_outputs(&self) -> Vec<Rect> {
        if self.outputs.is_empty() {
            vec![self.rect]
        } else {
            self.outputs.iter().map(OutputGeometry::rect).collect()
        }
    }

    /// Where stacked layers may place themselves, over `outputs` from
    /// [`Self::layer_outputs`].
    pub(in crate::shell) fn layer_geometry<'a>(&self, outputs: &'a [Rect]) -> LayerGeometry<'a> {
        LayerGeometry {
            viewport: self.rect,
            outputs,
            focused_output: self.focused_output().map(|output| output.rect()),
        }
    }

    /// The identify cards on screen right now, one per display.
    pub fn display_identify_labels(&self) -> Vec<crate::shell::display_identify::IdentifyLabel> {
        self.display_identify.labels(Instant::now()).to_vec()
    }

    /// Per-display strip widths: full extent plus width actually
    /// available to window groups, less the strip's edge gutters. Falls back to the host's own
    /// rect when the compositor hasn't pushed geometry yet (`--mode
    /// hosted`).
    pub(super) fn strip_output_spans(&self) -> Vec<OutputSpan> {
        let outputs: Vec<OutputGeometry> = if self.outputs.is_empty() {
            // `self.rect` is zero on a first frame (only set in
            // apply_layout, this runs during build_layout); no spans
            // lets `GroupRow::basis_for` fall back to its own chain.
            if self.rect.width() <= 1.0 {
                return Vec::new();
            }
            vec![OutputGeometry::unscaled(
                0.0,
                0.0,
                self.rect.width(),
                self.rect.height(),
            )]
        } else {
            self.outputs.clone()
        };
        let strip_left = 0.0_f64;
        outputs
            .iter()
            .filter(|o| o.width > 0.0)
            .map(|o| {
                let leads = strip_left >= o.x && strip_left < o.x + o.width;
                let leading = if leads { STRIP_EDGE_GUTTER_PX } else { 0.0 };
                OutputSpan {
                    x: o.x,
                    width: o.width,
                    usable: (o.width - leading - STRIP_TRAILING_GUTTER_PX).max(1.0),
                }
            })
            .collect()
    }

    /// Register the per-strip [`GroupRow`] raw pointers harvested at
    /// build time, used each frame to feed the reconciler without
    /// re-walking the widget tree.
    ///
    /// SAFETY: every pointer in `ptrs` must address a `GroupRow` owned by
    /// the content subtree this host wraps. The caller (currently
    /// [`build_tilo_overlay_host_with_chrome`](crate::shell::build_tilo_overlay_host_with_chrome)) is responsible for
    /// upholding that invariant by capturing the pointer immediately after
    /// `Box::new(strip_row)` and passing the same `Box` into the content
    /// tree — the `Box<T>` -> `Box<dyn Widget>` unsizing coercion keeps
    /// the heap address stable.
    #[must_use]
    pub fn with_group_row_ptrs(mut self, ptrs: HashMap<StripId, *mut GroupRow>) -> Self {
        self.group_row_ptrs = ptrs;
        self
    }

    /// Record which workspace the injected content was built for, so
    /// `Self::resync_workspace_content` can detect the first switch away.
    #[must_use]
    pub fn with_content_workspace(mut self, ws_raw: u64) -> Self {
        self.content_built_for_ws = Some(ws_raw);
        self.content_built_empty = self.chrome.as_ref().and_then(|chrome| {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .map(|workspace| !workspace.has_windows())
        });
        self
    }

    /// Swaps the workspace-scoped content when the active workspace changes
    /// or grows a strip with no row. Content root and pointer map rebuild
    /// TOGETHER via [`crate::shell::build_workspace_root`], so no
    /// reconciler ever observes a pointer into a freed row. A new strip has
    /// no incremental path — [`Self::resync_strips_from_tree`] splices by
    /// strip id and skips strips it has no row for — so a rebuild (costing
    /// scroll offsets, not focus, which lives in the tree) is the only
    /// option, and it's rare. Returns `true` on a swap. Runs at the top of
    /// both [`Widget::update`] and [`Widget::take_structure_dirty`] — the
    /// shots driver's relayout-only path skips `update`.
    pub(super) fn resync_workspace_content(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        {
            let st = chrome.borrow();
            let tree = st.tree();
            if let Some(surface) = self.desktop_surface_mut() {
                surface.retire_departed_requested_groups(&tree.desktop);
            }
        }
        let closing_visual = self
            .desktop_surface_mut()
            .is_some_and(|surface| surface.close_animation_in_flight());
        let (rebuild, stale) = {
            let st = chrome.borrow();
            let tree = st.tree();
            let active_raw = tree.desktop.active.raw();
            let active_empty = tree
                .desktop
                .active_workspace()
                .is_some_and(|workspace| !workspace.has_windows());
            // A kept workspace that was removed (or became another room)
            // lets go of its apps now, not at the next switch; so does one a
            // tab moved out of, before the reconcile that follows mounts the
            // tab where it went. Tabs move only with the chrome's revision.
            let moved_tabs = st.revision != self.last_chrome_revision;
            let stale = self.parked_workspaces.stale(&tree.desktop, moved_tabs);
            let rebuild = match self.content_built_for_ws {
                // Hand-made test content: latch, never rebuild over it.
                None => {
                    self.content_built_for_ws = Some(active_raw);
                    None
                }
                Some(cur)
                    if cur == active_raw
                        && (self.content_built_empty == Some(active_empty) || closing_visual)
                        && self.rows_cover_active_strips(&tree) =>
                {
                    None
                }
                Some(_) => Some((tree.desktop.clone(), st.record_creation_errors(&tree))),
            };
            (rebuild, stale)
        };
        if !stale.is_empty() {
            self.desktop_close_reports
                .extend(parked_workspaces::close(stale));
        }
        let Some((desktop, failures)) = rebuild else {
            return false;
        };
        let shown_before = self.content_built_for_ws;
        let swapped = self.rebuild_workspace_content(&chrome, &desktop, &failures);
        if swapped {
            // A chord announced its switch before making it; any other
            // switch follows the input that just caused it (a click in the
            // workspace menu, a key on its row) and owes its frame now. One
            // no input caused just now (an agent's, a script's, a sync's)
            // lands.
            let made_by = if self.workspace_switch_owed() {
                self.workspace_switch_source
            } else {
                ui::motion::recent_input(ui::motion::frame_now())
                    .unwrap_or(ui::tokens::MotionSource::Action)
            };
            self.slide_workspace(shown_before, &desktop, made_by);
            if !self.workspace_switch_owed() {
                self.start_workspace_switch(made_by);
            }
        }
        swapped
    }

    /// Rebuilds the desktop surface for `desktop` and replaces the row
    /// pointer map with it. Tabs whose tile is unchanged in the same room
    /// keep their live hosts, so a strip added or removed beside them costs
    /// no app state. A different workspace puts back the content kept for it
    /// ([`parked_workspaces`]) or gets fresh content, and the content it
    /// replaces is kept. Takes the desktop alone, with `failures` read
    /// beside it under the same tree lock, so a workspace switch never
    /// copies the replication documents.
    pub(super) fn rebuild_workspace_content(
        &mut self,
        chrome: &Chrome,
        desktop: &crate::shell::state::DesktopTree,
        failures: &std::collections::BTreeMap<u64, String>,
    ) -> bool {
        let records = chrome.borrow().records().clone();
        let switching = self
            .content_built_for_ws
            .is_some_and(|shown| shown != desktop.active.raw());
        if switching && self.desktop_surface_mut().is_some() {
            self.switch_workspace_content(chrome, desktop, failures, &records);
        }
        if let Some(surface) = self.desktop_surface_mut() {
            if !switching {
                let surface_chrome = crate::shell::workspace_surface_chrome(desktop, chrome);
                let state = chrome.borrow();
                surface.rebuild_retaining(
                    desktop,
                    &records,
                    &mut crate::shell::NativeTileResolver {
                        inputs: &state.hosted_inputs,
                        failures,
                    },
                    surface_chrome,
                );
            }
            for id in surface.strip_ids() {
                if let Some(row) = surface.row_mut(id) {
                    for group in &mut row.groups {
                        crate::shell::tab_documents::apply_creation_failures(group, failures);
                    }
                }
            }
            // The rebuild replaced every row; the old addresses are gone.
            self.group_row_ptrs = crate::shell::surface_row_ptrs(surface);
        } else {
            let (root, ptrs) =
                crate::shell::build_desktop_root(desktop, &records, chrome, failures);
            // Settle outgoing components while the old root still owns them,
            // so their recovery reports survive the swap.
            self.desktop_close_reports
                .extend(ui::component_lifecycle::close_components(
                    self.content.as_mut(),
                ));
            self.desktop_close_reports.extend(
                ui::component_lifecycle::take_component_close_reports(self.content.as_mut()),
            );
            // Replace content and pointer map in one step: dropping the old
            // root frees every old GroupRow, and the map must never outlive
            // the rows it points into.
            self.content = root;
            self.group_row_ptrs = ptrs;
        }
        self.content_built_for_ws = Some(desktop.active.raw());
        self.content_built_empty = desktop
            .active_workspace()
            .map(|workspace| !workspace.has_windows());
        self.workspace_content_layout_dirty = true;
        true
    }

    /// Show `desktop`'s active workspace in place of the one shown until
    /// now, which is kept: its content as it was left when it was kept, with
    /// what changed in the tree since reconciled, or else built fresh. The
    /// caller re-reads the row pointers.
    pub(super) fn switch_workspace_content(
        &mut self,
        chrome: &Chrome,
        desktop: &crate::shell::state::DesktopTree,
        failures: &std::collections::BTreeMap<u64, String>,
        records: &ui::record::RecordStore,
    ) {
        use tilo_desktop::composition::{DesktopSurface, ReconcileOutcome};
        // Before the target is mounted: a tab that moved here from a kept
        // workspace is mounted by this one alone.
        let moved_on = self.parked_workspaces.stale(desktop, true);
        self.desktop_close_reports
            .extend(parked_workspaces::close(moved_on));
        let kept = self.parked_workspaces.take(desktop.active.raw());
        let resumed = kept.is_some();
        let incoming = kept.unwrap_or_else(|| {
            crate::shell::build_desktop_root(desktop, records, chrome, failures).0
        });
        let mut outgoing = std::mem::replace(&mut self.content, incoming);
        if let Some(shown) = outgoing
            .as_any_mut()
            .and_then(|content| content.downcast_mut::<DesktopSurface>())
            && let Some(next) = self.desktop_surface_mut()
        {
            next.take_pointer_from(shown);
        }
        // The outgoing rows stay alive, kept; the map moves to the new ones
        // before anything walks it.
        self.group_row_ptrs.clear();
        let dropped = match self.content_built_for_ws {
            Some(shown) => self.parked_workspaces.park(shown, outgoing),
            None => vec![outgoing],
        };
        self.desktop_close_reports
            .extend(parked_workspaces::close(dropped));
        if !resumed {
            return;
        }
        let outcome = {
            let state = chrome.borrow();
            self.desktop_surface_mut().map(|surface| {
                surface.reconcile(
                    desktop,
                    records,
                    &mut crate::shell::NativeTileResolver {
                        inputs: &state.hosted_inputs,
                        failures,
                    },
                )
            })
        };
        if outcome == Some(ReconcileOutcome::RebuildRequired) {
            // Its strips changed while it was away: new chrome around the
            // same mounted apps (the surface is this workspace's room).
            let surface_chrome = crate::shell::workspace_surface_chrome(desktop, chrome);
            let state = chrome.borrow();
            if let Some(surface) = self.desktop_surface_mut() {
                surface.rebuild_retaining(
                    desktop,
                    records,
                    &mut crate::shell::NativeTileResolver {
                        inputs: &state.hosted_inputs,
                        failures,
                    },
                    surface_chrome,
                );
            }
        }
    }

    /// Workspace whose content has actually been reconciled, as opposed to
    /// the navigation target that may still be awaiting the next layout.
    pub(crate) fn workspace_content_id(&self) -> Option<u64> {
        self.content_built_for_ws
    }

    /// The workspaces whose content is kept while another one is shown,
    /// least recently shown first.
    #[cfg(test)]
    pub(crate) fn kept_workspaces_for_tests(&self) -> Vec<u64> {
        self.parked_workspaces.workspaces()
    }

    /// The shared desktop surface, when the content is one. Hand-built test
    /// content is not.
    /// A frame was painted. The frame builder's paint says so itself, for
    /// every host that paints it (the compositor, captures, tests): the
    /// content is never marked drawn, so the window groups' once-per-drawn-
    /// frame motion is told here, and a host that forgot to say it left
    /// every width spring stopped after one step, asking for frames.
    pub(crate) fn frame_presented(&mut self) {
        // The frame that shows a switched workspace has been painted.
        self.workspace_switch_owed = false;
        if let Some(surface) = self.desktop_surface_mut() {
            surface.frame_presented();
        }
    }

    pub(super) fn desktop_surface_mut(
        &mut self,
    ) -> Option<&mut tilo_desktop::composition::DesktopSurface> {
        self.content.as_any_mut()?.downcast_mut()
    }

    pub(super) fn desktop_surface(&self) -> Option<&tilo_desktop::composition::DesktopSurface> {
        self.content.as_any()?.downcast_ref()
    }

    /// The strip deck, while the workspace's strips are on screen.
    pub(crate) fn strip_deck(&self) -> Option<&ui::widget::StripDeck> {
        self.desktop_surface()?.strip_deck()
    }

    /// Focus the strip the deck went to on its own (a tab or content drag
    /// resting at its edge), as the person choosing it would.
    pub(super) fn harvest_strip_deck_request(&mut self) {
        let Some(index) = self
            .desktop_surface_mut()
            .and_then(tilo_desktop::composition::DesktopSurface::strip_deck_mut)
            .and_then(ui::widget::StripDeck::take_requested_strip)
        else {
            return;
        };
        if let Some(chrome) = self.chrome.clone() {
            ui::motion::note_source(ui::tokens::MotionSource::Pointer);
            let _ = chrome.borrow_mut().focus_strip_index(index);
        }
    }

    /// Whether the strip deck is between strips or following fingers: its
    /// strips are then drawn where they are on screen, not as layers.
    pub fn strip_deck_moving(&self) -> bool {
        self.strip_deck().is_some_and(|deck| !deck.at_rest())
    }

    /// Does the content tree have a row for every strip of the active
    /// workspace? A strip with no row can't be drawn (the incremental
    /// resync looks it up by id); extra rows point at strips that went away.
    pub(super) fn rows_cover_active_strips(
        &self,
        tree: &crate::shell::state::WorkspaceTree,
    ) -> bool {
        let Some(ws) = tree.desktop.active_workspace() else {
            return true;
        };
        ws.strips.len() == self.group_row_ptrs.len()
            && ws
                .strips
                .iter()
                .all(|s| self.group_row_ptrs.contains_key(&s.id))
    }

    /// Test hook for the workspace-content reconcile — mirrors the
    /// per-frame call the update/layout paths make.
    pub fn tick_workspace_content_for_tests(&mut self) -> bool {
        self.resync_workspace_content()
    }
}

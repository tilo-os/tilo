//! Harvesting desktop widget changes back into the workspace tree.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    pub(super) fn drain_app_host_intents(&mut self) {
        self.host_shell_allows.retain_mut(|pending| !pending.poll());
        let mut intents = Vec::new();
        for &row_ptr in self.group_row_ptrs.values() {
            // SAFETY: `group_row_ptrs` points into `self.content`; `&mut self`
            // makes this walk exclusive, matching the other row accessors.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            for group in &mut row.groups {
                let source_tab = group.active_tab_id();
                let Some(host) = group.active_host_mut() else {
                    continue;
                };
                // Not `content_mut`: that ends the tile's document lifetime,
                // and this runs on every frame.
                if let Some(intent) = host.take_host_intent() {
                    intents.push((source_tab, intent));
                }
            }
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        for (source_tab, intent) in intents {
            match intent {
                ui::widget::AppHostIntent::MoveDeviceDisplay { request, reply } => {
                    let Some(reply) = reply.upgrade() else {
                        continue;
                    };
                    let app_id = Self::active_tab_app_id(&chrome, source_tab);
                    reply.borrow_mut().result = Some(self.request_device_display_move(
                        app_id.as_deref().unwrap_or_default(),
                        request,
                    ));
                }
                ui::widget::AppHostIntent::ShowWorkspaceActivity {
                    resource_id,
                    version_id,
                    entry,
                } => {
                    let entry_id = entry.as_ref().map(|entry| *entry.id.as_bytes());
                    if let Some(entry) = entry {
                        self.room_activity.open_external_entry(entry);
                    }
                    chrome.borrow_mut().open_room_activity(Some(
                        crate::shell::chrome_state::ActivityFocus {
                            resource_id: Some(resource_id),
                            resource_version: version_id,
                            entry_id,
                            ..crate::shell::chrome_state::ActivityFocus::default()
                        },
                    ));
                }
                ui::widget::AppHostIntent::OpenSearch(request) => {
                    self.open_app_search(source_tab, &request);
                }
                ui::widget::AppHostIntent::AllowHostShell { reply } => {
                    let (prompt, pending) = crate::shell::host_shell_allow::prompt(reply);
                    let mut st = chrome.borrow_mut();
                    st.pending_grants.push_back(prompt);
                    st.bump();
                    self.host_shell_allows.push(pending);
                }
                ui::widget::AppHostIntent::OpenDocument {
                    path,
                    capability,
                    token,
                    reply,
                } => {
                    let Some(reply) = reply.upgrade() else {
                        continue;
                    };
                    let Ok(mut reply) = reply.try_borrow_mut() else {
                        continue;
                    };
                    if !reply.is_pending(token) {
                        continue;
                    }
                    Self::complete_document_open_request(
                        &chrome,
                        source_tab,
                        &path,
                        &capability,
                        token,
                        &mut reply,
                    );
                }
            }
        }
    }

    /// Resolve the one capability-bound Files request that drain admitted.
    /// Keeping the authorization immediately beside the actual opener avoids
    /// any generic app intent becoming a filesystem-read capability.
    pub(super) fn complete_document_open_request(
        chrome: &crate::shell::chrome_state::Chrome,
        source_tab: crate::shell::state::TabId,
        path: &std::path::Path,
        capability: &ui::widget::DocumentOpenCapability,
        token: u64,
        reply: &mut ui::widget::DocumentOpenReply,
    ) {
        let authorized_source = {
            let state = chrome.borrow();
            let files_app_id = crate::shell::state::AppKind::FileExplorer.app_id();
            state.active_tab_id() == Some(source_tab)
                && state.active_app_id().as_deref() == Some(files_app_id.as_str())
                && state
                    .hosted_inputs
                    .accepts_document_open_capability(capability)
        };
        if !authorized_source {
            reply.cancel(token);
            return;
        }
        reply.complete(token, crate::shell::open_document_from_files(chrome, path));
    }

    /// Reconciles the shared desktop surface with the live tree. Groups and
    /// tabs are spliced in place, so untouched rows keep their host state
    /// (scroll, tab selection); a changed strip set rebuilds the surface.
    /// Cheap on the no-op path — one u64 compare against
    /// `last_chrome_revision`.
    pub(super) fn resync_strips_from_tree(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        chrome.borrow_mut().retry_new_tab_records();
        self.place_new_groups();
        let current_rev = chrome.borrow().revision;
        let closing_visuals = self
            .desktop_surface_mut()
            .is_some_and(|surface| surface.has_closing_visuals());
        if current_rev == self.last_chrome_revision && !closing_visuals {
            return false;
        }
        self.last_chrome_revision = current_rev;
        let outcome = {
            let st = chrome.borrow();
            let records = st.records().clone();
            let tree = st.tree();
            let failures = st.record_creation_errors(&tree);
            let Some(surface) = self.desktop_surface_mut() else {
                return false;
            };
            surface.reconcile(
                &tree.desktop,
                &records,
                &mut crate::shell::NativeTileResolver {
                    inputs: &st.hosted_inputs,
                    failures: &failures,
                },
            )
        };
        match outcome {
            tilo_desktop::composition::ReconcileOutcome::Unchanged => false,
            tilo_desktop::composition::ReconcileOutcome::Updated => true,
            tilo_desktop::composition::ReconcileOutcome::RebuildRequired => {
                let (desktop, failures) = {
                    let st = chrome.borrow();
                    let tree = st.tree();
                    (tree.desktop.clone(), st.record_creation_errors(&tree))
                };
                self.rebuild_workspace_content(&chrome, &desktop, &failures)
            }
        }
    }

    /// Pushes the compositor's transient fullscreen state onto every
    /// strip's [`GroupRow`] so the owning strip promotes while others
    /// collapse. Idempotent. Called from [`Widget::update`],
    /// [`Widget::build_layout`], and `FrameBuilder::relayout` *before* the
    /// structure-dirty check, so a promotion/restore triggers the rebuild.
    pub fn sync_fullscreen_from_chrome(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // The expanded slot is the tree's maximized window, not a second
        // record of it: a window given back its place when the window it
        // stepped back for went away, or one maximized before a restart,
        // takes the display again as the person's own maximize does.
        {
            let mut st = chrome.borrow_mut();
            let expanded = expanded_group(&st.tree(), st.maximized_group());
            st.set_maximized_group(expanded);
        }
        // Client `xdg_toplevel.set_fullscreen` wins over a user-expanded
        // group if both are set.
        let (fs_group, single, client_fullscreen) = {
            let st = chrome.borrow();
            let client = st.fullscreen_group();
            let single = st.single_strip().then(|| st.active_strip_id()).flatten();
            (
                client.or_else(|| st.maximized_group()),
                single,
                client.is_some(),
            )
        };
        // The live client tree and this rendered tree aren't yet unified,
        // so a fullscreen id we don't host must count as "no fullscreen"
        // — otherwise every strip hides around a group that isn't here.
        let owned = fs_group.is_some_and(|gid| {
            self.group_row_ptrs
                .values()
                // SAFETY: see the per-row note below — same `group_row_ptrs`
                // contract; here we only read `group_ids()`.
                .any(|&p| unsafe { (*p).group_ids().contains(&gid) })
        });
        // Publish the presentation edge before `take_structure_dirty` can
        // rebuild the top bar. Restore is especially sensitive: rebuilding
        // while this still says `true` leaves the chrome hidden, with no
        // later structural edge to rebuild it again.
        chrome.borrow_mut().set_fullscreen_presentation(owned);
        // On a desktop spread across displays a fullscreen covers its own
        // display only: the rows of other strips step aside there and stay
        // on every other display, beside that display's own chrome.
        let covered = if owned && client_fullscreen {
            fs_group.and_then(|group| self.fullscreen_display_of(group))
        } else {
            None
        };
        chrome.borrow_mut().set_fullscreen_display(covered);
        for (&strip, &row_ptr) in &self.group_row_ptrs {
            // SAFETY: same contract as every other `group_row_ptrs` accessor —
            // the pointer comes from a `Box<GroupRow>` owned by the content
            // tree, the host outlives the content, and `&mut self` gives us
            // unaliased access for the duration of this call.
            let row: &mut GroupRow = unsafe { &mut *row_ptr };
            // Neither keeps a gutter: an expanded window fills the display.
            row.set_promotion_keeps_gutter(false);
            row.set_promotion_keeps_controls(!client_fullscreen);
            match fs_group {
                Some(gid) if owned && row.group_ids().contains(&gid) => {
                    row.set_fullscreen(Some(gid), false);
                    row.set_covered_output(None);
                }
                Some(_) if owned => {
                    row.set_fullscreen(None, covered.is_none());
                    row.set_covered_output(covered.map(|display| display.output));
                }
                _ => {
                    row.set_fullscreen(None, single.is_some_and(|single| single != strip));
                    row.set_covered_output(None);
                }
            }
        }
    }

    /// The display the fullscreen `group` covers, while the desktop is
    /// spread across displays: the one the partition places it on.
    pub(super) fn fullscreen_display_of(
        &self,
        group: GroupId,
    ) -> Option<crate::shell::chrome_state::FullscreenDisplay> {
        if !self.is_partitioned() {
            return None;
        }
        let output = self
            .chrome
            .as_ref()?
            .borrow()
            .tree()
            .desktop
            .outputs
            .display_of(group)?;
        let primary = self
            .primary_output()
            .is_some_and(|primary| primary.id == output);
        Some(crate::shell::chrome_state::FullscreenDisplay { output, primary })
    }

    /// A full-display surface — Capture Studio, the screenshot markup
    /// editor — covers the focused display: its bars sit along that
    /// display's top and bottom edges and "Screen" is that display.
    /// Across the whole desktop of a docked laptop the bars were centred on
    /// the combined width and hung from its tallest edge, which put them on
    /// the other display, partly below the panel's bottom, while the panel
    /// only greyed: Print looked broken until Escape.
    pub(super) fn place_on_focused_display(&self, taffy: &mut TaffyTree<()>, node: NodeId) {
        let Some(display) = self.focused_output() else {
            return;
        };
        let area = display.rect();
        let Ok(style) = taffy.style(node) else {
            return;
        };
        let mut style = style.clone();
        style.position = Position::Absolute;
        style.inset = TaffyRect {
            left: LengthPercentageAuto::length(f32_of_f64(area.x0 - self.rect.x0)),
            right: LengthPercentageAuto::auto(),
            top: LengthPercentageAuto::length(f32_of_f64(area.y0 - self.rect.y0)),
            bottom: LengthPercentageAuto::auto(),
        };
        style.size = Size {
            width: Dimension::length(f32_of_f64(area.width())),
            height: Dimension::length(f32_of_f64(area.height())),
        };
        let _ = taffy.set_style(node, style);
    }

    /// Whether the requested fullscreen/maximized group is represented by a
    /// live retained row. Chrome stays visible until this becomes true.
    pub(super) fn fullscreen_group_is_owned(&self) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let group = {
            let state = chrome.borrow();
            state.fullscreen_group().or_else(|| state.maximized_group())
        };
        group.is_some_and(|group| {
            self.group_row_ptrs.values().any(|&row| {
                // SAFETY: same retained-row ownership contract as
                // `sync_fullscreen_from_chrome`; this is read-only.
                unsafe { (*row).group_ids().contains(&group) }
            })
        })
    }

    pub(super) fn reconcile_active_group_off_closing(&self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Take a snapshot first so the chrome borrow is dropped before the
        // mutable reconcile borrow below — avoids RefCell aliasing if any
        // reconciler step ever calls into a chrome-borrowing helper.
        let mut closing_per_strip: Vec<(StripId, Vec<GroupId>)> =
            Vec::with_capacity(self.group_row_ptrs.len());
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: pointer was captured from a `Box<GroupRow>` we own
            // through the content tree (see `with_group_row_ptrs`). The
            // host outlives the content, and this is a read-only deref,
            // so the pointer is valid regardless of aliasing.
            let row: &GroupRow = unsafe { &*ptr };
            let closing: Vec<GroupId> = row
                .groups
                .iter()
                .filter(|g| g.is_closing())
                .map(|g| g.id)
                .collect();
            if !closing.is_empty() {
                closing_per_strip.push((strip_id, closing));
            }
        }
        if closing_per_strip.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        let mut changed = false;
        for (strip_id, closing) in closing_per_strip {
            let mut tree = st.tree_mut();
            let before = tree.desktop.active_group_for(strip_id);
            let after = tree
                .desktop
                .nudge_active_group_off_closing(strip_id, &closing);
            drop(tree);
            changed |= before != after;
        }
        if changed {
            // Closing the focused group's last tab changes the destination
            // for keyboard input immediately, before the fade finishes. Keep
            // that focus hand-off durable at the same boundary: a crash or
            // logout during the animation must not restart with the stale
            // closing group selected.
            st.tree().queue_save();
        }
    }

    /// Run desktop requests through the shared desktop controller with
    /// this device as the owner. Returns how many were accepted; chrome is
    /// only borrowed when there is a request.
    pub(super) fn dispatch_desktop_intents(
        chrome: &Chrome,
        intents: impl IntoIterator<Item = tilo_desktop::intents::DesktopIntent>,
    ) -> usize {
        let mut intents = intents.into_iter().peekable();
        if intents.peek().is_none() {
            return 0;
        }
        let mut st = chrome.borrow_mut();
        let mut sink = crate::shell::desktop_commands::ShellCommands { chrome: &mut st };
        intents
            .filter(|intent| {
                match tilo_desktop::session_commands::dispatch(&mut sink, intent.clone()) {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(?intent, %error, "desktop request refused");
                        false
                    }
                }
            })
            .count()
    }

    /// Drain every row's groups through `take`, collecting the intents it
    /// finds pending.
    pub(super) fn take_group_intents(
        &self,
        mut take: impl FnMut(&mut UiWindowGroup) -> Option<tilo_desktop::intents::DesktopIntent>,
    ) -> Vec<tilo_desktop::intents::DesktopIntent> {
        let mut intents = Vec::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs` — pointer was
            // captured from a `Box<GroupRow>` owned through the content
            // tree, the host outlives the content, and reconcile sweeps
            // run sequentially so no other path is aliasing this row.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            intents.extend(row.groups.iter_mut().filter_map(&mut take));
        }
        intents
    }

    /// Every group's pending maximize toggle, persisted through the desktop
    /// controller — without this the toggle lives only on the widget and
    /// resets on the next restart.
    pub(super) fn harvest_pending_maximize_changes(&self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // The maximized chrome's `[Full screen]` chip: the same real
        // fullscreen Super+F and F11 toggle, for that group.
        let mut fullscreen = None;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: as in `take_group_intents`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                if group.take_pending_fullscreen_request() {
                    fullscreen = Some(group.id);
                }
            }
        }
        if let Some(group) = fullscreen {
            chrome.borrow_mut().set_user_fullscreen_group(Some(group));
        }
        let intents = self.take_group_intents(|group| {
            tilo_desktop::intents::take_maximize(group)
                .map(tilo_desktop::intents::DesktopIntent::Local)
        });
        if intents.is_empty() {
            return;
        }
        Self::dispatch_desktop_intents(&chrome, intents);

        // The Expand interaction takes effect at this harvest boundary,
        // before the next layout. Mirror retained-row ownership into chrome
        // now so the top bar and wheel observe the same atomic transition.
        let fullscreen = self.fullscreen_group_is_owned();
        chrome.borrow_mut().set_fullscreen_presentation(fullscreen);
    }

    /// Mirrors the widget-side `TabBar::active_id` back into the persisted
    /// tree's `group.active_tab`/`active_group`/`active_strip` — without
    /// it, a tab click flips visible content but leaves search-scope
    /// helpers pointing at the group's initial tab. Runs after input
    /// (`flush_active_tab_writeback`) and before a search or palette
    /// scope is resolved, not every frame.
    pub(super) fn harvest_active_tab_writeback(&self) {
        // Widgets and chrome can't be borrowed at once, so collect the
        // tuples first and apply under one chrome borrow.
        struct Writeback {
            strip: StripId,
            group: GroupId,
            tab: crate::shell::state::TabId,
        }
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut writes: Vec<Writeback> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — captured from a `Box<GroupRow>` owned through
            // the content tree, host outlives the content; this is a
            // read-only deref.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                if let Some(tab) = g.take_tab_activation() {
                    writes.push(Writeback {
                        strip: strip_id,
                        group: g.id,
                        tab,
                    });
                }
            }
        }
        if writes.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        // Only an explicit pointer/keyboard/accessibility edge reaches this
        // list. A newly rebuilt retained row can temporarily disagree with
        // the model; treating that divergence as a click steals focus back
        // from an application which just opened. Focus is stamped only for
        // the active workspace: a newer workspace switch takes precedence
        // over a gesture made in the outgoing one.
        let mut selection_changed = false;
        let mut focus_writes: Vec<(WorkspaceId, StripId, GroupId)> = Vec::new();
        // A FileReference card is inert until the person explicitly selects
        // its tab.  Capture that activation while holding the tree, then
        // resolve/open it after releasing chrome so the normal document
        // launcher can apply its capability and hydration path.
        let mut activated_file_tabs = Vec::new();
        {
            let mut tree = st.tree_mut();
            let active_workspace = tree.desktop.active;
            for w in &writes {
                for ws in &mut tree.desktop.workspaces {
                    for strip in &mut ws.strips {
                        if strip.id != w.strip {
                            continue;
                        }
                        if let Some(group) =
                            strip.window_groups.iter_mut().find(|g| g.id == w.group)
                        {
                            // A stale `bar.active_id` (mid-detach
                            // frame) would write a phantom active-tab
                            // id and silently drop the group.
                            let tab_exists = group.tabs.iter().any(|t| t.id == w.tab);
                            if !tab_exists {
                                continue;
                            }
                            selection_changed |= group.active_tab != w.tab;
                            group.activate_tab(w.tab);
                            if ws.id == active_workspace {
                                focus_writes.push((ws.id, w.strip, w.group));
                                activated_file_tabs.push(w.tab);
                            }
                        }
                    }
                }
            }
        }
        // Activating a group's already-selected tab is still a focus edge.
        if selection_changed || !focus_writes.is_empty() {
            let mut tree = st.tree_mut();
            for (ws_id, strip_id, group_id) in focus_writes {
                selection_changed |= tree.desktop.active_strip.get(&ws_id) != Some(&strip_id)
                    || tree.desktop.active_group.get(&strip_id) != Some(&group_id);
                tree.desktop.active_strip.insert(ws_id, strip_id);
                tree.desktop.active_group.insert(strip_id, group_id);
                // Assistive activation has no pointer event to choose its display.
                if let Some(display) = tree.desktop.outputs.display_of(group_id) {
                    selection_changed |= tree.desktop.outputs.focused_display() != Some(display);
                    tree.desktop.outputs.set_focused(display);
                }
            }
            let activated_file_tabs: Vec<_> = activated_file_tabs
                .into_iter()
                .filter(|tab| tree.continuation_file_references.contains_key(tab))
                .collect();
            // Selection is session state, not merely paint state. Persist it
            // in the same input/frame boundary that accepted the click so a
            // compositor crash cannot restore the previously visible tab.
            if selection_changed {
                tree.queue_save();
            }
            drop(tree);
            drop(st);
            for tab in activated_file_tabs {
                let _ = crate::shell::doc_open::activate_continuity_file_tab(&chrome, tab);
            }
        }
    }

    /// Give every live window group the same share of its strip, and
    /// mirror it to the persisted tree the way a width drag does. Setting
    /// the tree alone wouldn't move anything — the laid-out width lives
    /// on the widget. Returns the ids touched.
    pub fn set_group_widths(&mut self, permille: u32) -> Vec<u64> {
        let mut touched: Vec<u64> = Vec::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                g.set_width(ui::widget::WindowWidth::Fraction(permille));
                touched.push(g.id);
            }
        }
        if touched.is_empty() {
            return touched;
        }
        if let Some(chrome) = self.chrome.clone() {
            let st = chrome.borrow_mut();
            let mut tree = st.tree_mut();
            for ws in &mut tree.desktop.workspaces {
                for strip in &mut ws.strips {
                    for g in &mut strip.window_groups {
                        if touched.contains(&g.id) {
                            g.width = crate::shell::state::WindowWidth::Fraction(permille);
                        }
                    }
                }
            }
            tree.queue_save();
        }
        // `RuntimeGroup::set_width` flags its own style dirty; the
        // frame loop's structure sweep is what rebuilds the taffy node
        // whose baked length carries the old width.
        touched
    }

    /// Set one window group's width from the window menu. Mirrors the same
    /// value to the laid-out widget and the persisted tree; returns whether
    /// the group was found.
    pub fn set_group_width(
        &mut self,
        group_id: u64,
        width: crate::shell::state::WindowWidth,
    ) -> bool {
        let ui_width = match width {
            crate::shell::state::WindowWidth::Narrow => ui::widget::WindowWidth::Narrow,
            crate::shell::state::WindowWidth::Normal => ui::widget::WindowWidth::Normal,
            crate::shell::state::WindowWidth::Wide => ui::widget::WindowWidth::Wide,
            crate::shell::state::WindowWidth::Fraction(permille) => {
                ui::widget::WindowWidth::Fraction(permille)
            }
        };
        let mut touched = false;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                if g.id == group_id {
                    g.set_width(ui_width);
                    touched = true;
                }
            }
        }
        if !touched {
            return false;
        }
        if let Some(chrome) = self.chrome.clone() {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            for ws in &mut tree.desktop.workspaces {
                for strip in &mut ws.strips {
                    for g in &mut strip.window_groups {
                        if g.id == group_id {
                            g.width = width;
                        }
                    }
                }
            }
            tree.queue_save();
        }
        true
    }

    /// Close the active tab of the focused group, through the same
    /// `TabBar::start_closing` a `✕` click runs. `false` when nothing is
    /// focused or the group has no tab left to close.
    pub fn close_focused_tab(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let focused = crate::shell::window_actions::focused_group(&chrome.borrow());
        let Some(gid) = focused else {
            return false;
        };
        self.close_tab_in_group(gid, None)
    }

    /// Close `tab` (or the active tab, when `tab` is `None`) in the group
    /// with id `gid`. Shared by the chord and the tab context menu.
    pub fn close_tab_in_group(&mut self, gid: GroupId, tab: Option<u64>) -> bool {
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                if g.id != gid {
                    continue;
                }
                let tab_id = tab.unwrap_or_else(|| g.active_tab_id());
                let Some(chrome) = self.chrome.clone() else {
                    return false;
                };
                let close = tilo_desktop::intents::SessionIntent::CloseTab {
                    group: gid,
                    tab: tab_id,
                };
                if Self::dispatch_desktop_intents(
                    &chrome,
                    [tilo_desktop::intents::DesktopIntent::Session(close)],
                ) == 0
                {
                    return false;
                }
                // A foreign client's close request can be refused. Keep its
                // live tab visible until the compositor sees destruction.
                if Self::tab_still_present(&chrome, tab_id) {
                    g.cancel_tab_close(tab_id);
                    return true;
                }
                // The durable tree/document mutation must succeed before the
                // widget begins its close animation. A visual-only close is
                // precisely what allowed the next resync to restore a window.
                let closed = match tab {
                    Some(id) => g.request_close_tab(id),
                    None => g.request_close_active_tab(),
                };
                debug_assert!(closed, "tree and rendered tab strip diverged before close");
                return closed;
            }
        }
        false
    }

    pub(super) fn tab_still_present(chrome: &Chrome, tab_id: u64) -> bool {
        chrome
            .borrow()
            .tree()
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .any(|tab| tab.id == tab_id)
    }

    /// Drain close buttons from the rendered rows into the same lifecycle
    /// transaction used by keyboard closes. This is intentionally run in
    /// the input dispatch that produced the request, not on a later frame.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "Exclusive access prevents aliases while raw row pointers are materialized as mutable rendered rows."
    )]
    pub(super) fn harvest_pending_tab_closes(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let mut intents = Vec::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                intents.extend(
                    tilo_desktop::intents::take_closes(group)
                        .into_iter()
                        .map(tilo_desktop::intents::DesktopIntent::Session),
                );
            }
        }
        let changed = Self::dispatch_desktop_intents(&chrome, intents) > 0;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: the pointers belong to this exclusively borrowed host.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                let retained: Vec<_> = chrome
                    .borrow()
                    .tree()
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| workspace.strips.iter())
                    .flat_map(|strip| strip.window_groups.iter())
                    .filter(|model| model.id == group.id)
                    .flat_map(|model| model.tabs.iter())
                    .map(|tab| tab.id)
                    .collect();
                for tab in retained {
                    group.cancel_tab_close(tab);
                }
            }
        }
        changed
    }

    pub(super) fn harvest_pending_width_changes(&self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let intents = self.take_group_intents(|group| {
            tilo_desktop::intents::take_width(group)
                .map(tilo_desktop::intents::DesktopIntent::Local)
        });
        Self::dispatch_desktop_intents(&chrome, intents);
    }

    /// Commits a completed whole-group reorder drag through the desktop
    /// controller — without this a group drag resets to the persisted order
    /// on restart.
    pub(super) fn harvest_pending_group_reorder(&self) {
        // A partitioned row orders groups display by display, so its index
        // is not the strip's: record the display the group was dropped on
        // and its neighbours there instead.
        struct PartitionedDrop {
            strip: StripId,
            group: u64,
            display: Option<u32>,
            before: Option<u64>,
            after: Option<u64>,
        }
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut intents = Vec::new();
        let mut partitioned = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            let Some(intent) = tilo_desktop::intents::take_group_reorder(strip_id, row) else {
                continue;
            };
            let tilo_desktop::intents::SessionIntent::ReorderGroup { group, .. } = intent else {
                continue;
            };
            if row.is_partitioned() {
                let ids = row.group_ids();
                let at = ids.iter().position(|id| *id == group);
                let before = at
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| ids.get(i).copied())
                    .filter(|id| row.lane_of_group(*id) == row.lane_of_group(group));
                let after = at
                    .and_then(|i| ids.get(i.saturating_add(1)).copied())
                    .filter(|id| row.lane_of_group(*id) == row.lane_of_group(group));
                partitioned.push(PartitionedDrop {
                    strip: strip_id,
                    group,
                    display: row.output_of_group(group),
                    before,
                    after,
                });
            } else {
                intents.push(tilo_desktop::intents::DesktopIntent::Session(intent));
            }
        }
        if !partitioned.is_empty() {
            let mut st = chrome.borrow_mut();
            let mut rehomed = false;
            {
                let mut tree = st.tree_mut();
                for PartitionedDrop {
                    strip: strip_id,
                    group: group_id,
                    display,
                    before,
                    after,
                } in partitioned
                {
                    // Dropped onto another display: it belongs there now.
                    if let Some(display) = display
                        && tree.rehome_group(strip_id, group_id, display)
                    {
                        rehomed = true;
                    }
                    let order: Vec<u64> = tree
                        .desktop
                        .workspaces
                        .iter()
                        .flat_map(|w| &w.strips)
                        .find(|s| s.id == strip_id)
                        .map(|s| {
                            s.window_groups
                                .iter()
                                .map(|g| g.id)
                                .filter(|id| *id != group_id)
                                .collect()
                        })
                        .unwrap_or_default();
                    let slot = before
                        .and_then(|b| order.iter().position(|id| *id == b))
                        .map(|i| i.saturating_add(1))
                        .or_else(|| after.and_then(|a| order.iter().position(|id| *id == a)));
                    let Some(slot) = slot else {
                        continue;
                    };
                    let command = tilo_desktop_model::commands::DesktopCommand::ReorderGroup {
                        strip: strip_id.raw(),
                        group: group_id,
                        before: order.get(slot).copied(),
                    };
                    if let Err(error) = tree.commit_command(&command) {
                        tracing::warn!(%error, "group reorder refused");
                    }
                }
            }
            if rehomed {
                st.bump();
            }
        }
        Self::dispatch_desktop_intents(&chrome, intents);
    }

    /// Same as [`Self::harvest_pending_group_reorder`], for an
    /// *intra-group* tab reorder.
    pub(super) fn harvest_pending_tab_reorder(&self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let intents = self.take_group_intents(|group| {
            tilo_desktop::intents::take_tab_reorder(group)
                .map(tilo_desktop::intents::DesktopIntent::Session)
        });
        Self::dispatch_desktop_intents(&chrome, intents);
    }

    /// Mirrors same-strip tab transfers and splits from the live rows into
    /// the persisted workspace tree. The row publishes only group/tab ids;
    /// the tree remains the sole owner of tile content and collaboration data.
    pub(super) fn harvest_pending_group_topology(&self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut changes = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: same contract as the other row harvesters: the content
            // tree owns this pointer and harvest passes run sequentially.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            if let Some(topology) = row.take_pending_topology_change() {
                changes.push((strip_id, topology));
            }
        }
        if changes.is_empty() {
            return;
        }
        let state = chrome.borrow_mut();
        let mut tree = state.tree_mut();
        for (strip_id, topology) in changes {
            tree.reconcile_strip_topology(strip_id, &topology);
        }
    }

    /// Drains each group's [`UiWindowGroup::take_pending_search_open`]
    /// one-shot and opens the search bar scoped to the active app — the
    /// same entry point Ctrl+K uses. The palette lives on the host, not
    /// the group.
    pub(super) fn harvest_pending_search_open(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let mut requested = None;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                if let Some(handoff) = g.take_pending_search_open() {
                    requested = Some(handoff);
                }
            }
        }
        let Some(handoff) = requested else {
            return;
        };
        // Mirror the widget-side active tab into the tree first so the scope
        // resolves to the app whose chip was clicked, not a stale pointer.
        self.harvest_active_tab_writeback();
        let scope = match handoff.reach {
            ui::widget::SearchReach::App => crate::shell::chrome_state::SearchScope::ActiveApp,
            ui::widget::SearchReach::Everywhere => crate::shell::chrome_state::SearchScope::All,
        };
        if handoff.query.is_empty() {
            // The control of a window with nothing to search in place: the
            // palette narrowed to its app, toggled as the chip always was.
            self.open_palette_with_scope(scope);
            return;
        }
        // A search widened from a window carries its query on; the palette
        // opens (never toggles closed) in the wider scope.
        let current = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().search_scope)
            .unwrap_or_default();
        if !self.palette_visible || current != scope {
            self.open_palette_with_scope(scope);
        }
        self.palette.set_query(&handoff.query);
        self.palette_visible = true;
    }

    /// Whether a window's search field holds the keyboard
    /// ([`ui::widget::WindowSearch::has_keyboard`]).
    pub(crate) fn window_search_has_keyboard(&self) -> bool {
        self.group_row_ptrs.values().any(|&ptr| {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups
                .iter()
                .any(|group| group.window_search().has_keyboard())
        })
    }

    /// Give a keyboard or input-method event to the window whose search
    /// field holds the keyboard, ahead of every chord and focused control
    /// but the desktop's own (Super) ones: the field is a text field like
    /// any other. Whether it took it.
    pub(super) fn route_window_search_input(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) || ctx.modifiers.state().super_key()
            || self.palette_visible
        {
            return false;
        }
        let mut taken = false;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                taken |= group.route_search_input(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            }
        }
        if taken {
            self.harvest_pending_search_open();
        }
        taken
    }

    /// Sibling of [`Self::harvest_pending_search_open`] for the "+" chip:
    /// opens the palette aimed at that group rather than minting an
    /// assumed tab type. [`OpenTarget::Here`] pins the result to this
    /// group even when the placement rule would open beside it.
    pub(super) fn harvest_pending_new_tab(&mut self) -> bool {
        if self.group_row_ptrs.is_empty() {
            return false;
        }
        let mut requested: Option<u64> = None;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                if let Some(id) = g.take_pending_new_tab() {
                    requested = Some(id);
                }
            }
        }
        let Some(group) = requested else {
            return false;
        };
        self.open_new_tab_palette_for_group(group);
        true
    }

    /// Configure the application picker opened by a group's `+` affordance.
    /// Split from the widget one-shot harvest so its scope/placement contract
    /// has direct regression coverage.
    pub(super) fn open_new_tab_palette_for_group(&mut self, group: GroupId) {
        // Must run before the palette's scope resolution and launch path
        // read the active pointers.
        self.harvest_active_tab_writeback();
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            st.open_target = crate::shell::chrome_state::OpenTarget::Here(group);
            // The group is the placement target and its workspace is the
            // search boundary. Machine-wide search belongs to Super+K; a
            // local "+" must not silently reach into other workspaces.
            st.search_scope = crate::shell::chrome_state::SearchScope::Workspace;
            st.search_resource_scope = None;
            st.bump();
        }
        // Narrowed to kinds that can *be* a tab, so results aren't buried
        // under notes/files/chrome actions the user has to read past.
        self.active_kind_filter = app_kind_filter();
        // Force-open, not `open_palette_with_scope`: that helper toggles
        // closed on a same-scope repeat, wrong for a "+" click.
        if !self.palette_visible {
            self.palette_visible = true;
            self.palette.set_focus(true);
            self.palette.set_query("");
        }
        // Re-run the query even when the palette was already up: the rows
        // carry a placement hint that just changed.
        self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
    }

    /// Drops a stale `OpenTarget::Here` and kind filter once the palette
    /// closes — both are one-shot aims for a single launch, and a leaked
    /// filter would silently narrow the next unrelated query.
    pub(super) fn clear_stale_open_target(&mut self) {
        if self.palette_visible {
            return;
        }
        if self.param_flow.is_some() || self.secondary_flow.is_some() {
            self.end_palette_steps();
        }
        self.expanded_kinds.clear();
        self.present_kinds.clear();
        self.palette
            .set_query_placeholder(t("host-search-placeholder"));
        self.active_kind_filter.clear();
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut st = chrome.borrow_mut();
        if st.search_resource_scope.take().is_some() {
            st.bump();
        }
        if st.open_target != crate::shell::chrome_state::OpenTarget::Beside {
            st.open_target = crate::shell::chrome_state::OpenTarget::Beside;
            st.bump();
        }
    }

    /// Test-only one-shot: retire a dismissed palette's aim and filter.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_clear_stale_open_target_for_tests(&mut self) {
        self.clear_stale_open_target();
    }

    /// Clears a typed wifi passphrase once the quick-settings popover
    /// closes, so it doesn't outlive the window it was entered in — and
    /// lets go of the field's value as soon as its prompt is gone, however
    /// it went.
    pub(super) fn clear_stale_wifi_password(&mut self) {
        if let Some(chrome) = self.chrome.clone() {
            let open = chrome.borrow().overlays.quick_settings;
            let mut st = chrome.borrow_mut();
            if !open && st.quick_settings.wifi_password.take().is_some() {
                st.bump();
            }
        }
        self.sync_wifi_password_input();
    }

    /// Frames the focused group and lights its focus marker through the
    /// shared surface, from the tree's active strip and group. Framing is
    /// edge-triggered, so free wheel-scrolling isn't fought by a per-frame
    /// snap-back.
    pub(super) fn scroll_active_group_into_view(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Already done this frame when `update` ran; the paths that lay out
        // without it (the shots driver, tests) follow the request here.
        self.follow_brought_forward();
        let reveal = self.brought_forward_reveal.take();
        let st = chrome.borrow();
        let tree = st.tree();
        if let Some(surface) = self.desktop_surface_mut() {
            if let Some(strip) = reveal {
                surface.reveal_on_next_layout(strip);
            }
            surface.sync_navigation(&tree.desktop);
        }
    }

    /// A window the person asked for came forward
    /// ([`crate::shell::tab_binding::bring_forward`]): the view goes to it.
    /// Runs before the strips are reconciled with the tree, so going to
    /// another strip slides the deck there and the strip scrolls to the
    /// window with the motion the person's own input asks for; when the
    /// window arrives after that input has passed (an app starting, another
    /// app's activation, a launch that took a while), with standard motion
    /// rather than a jump. Each request is followed once, and only while its
    /// window is still the one in front.
    pub(super) fn follow_brought_forward(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let request = {
            let st = chrome.borrow();
            let tree = st.tree();
            tree.brought_forward
                .filter(|request| request.serial > self.brought_forward_shown)
                .map(|request| (request, request.is_focused(&tree)))
        };
        let Some((request, focused)) = request else {
            return;
        };
        self.brought_forward_shown = request.serial;
        if !focused {
            return;
        }
        // A window expanded over the desktop (the Expand chip) steps back,
        // as one maximized over the target's own strip already has: it
        // would otherwise keep covering the window the person goes to.
        // Full screen is the compositor's to end, client and Super+F alike
        // ([`crate::compositor::state::State::leave_fullscreen_when_focus_moved`]).
        {
            let mut st = chrome.borrow_mut();
            if let Some(expanded) = st.maximized_group().filter(|g| *g != request.group) {
                st.set_maximized_group(None);
                let mut tree = st.tree_mut();
                crate::shell::stepped_back::step_back_for(&mut tree, expanded, request.tab);
                tree.queue_save();
            }
        }
        self.brought_forward_reveal = Some(request.strip);
        let source = ui::motion::recent_input(ui::motion::frame_now())
            .unwrap_or(ui::tokens::MotionSource::Pointer);
        self.begin_motion(source);
    }

    /// Publishes the active strip's laid-out geometry for
    /// [`crate::shell::placement`] to consult. Doesn't bump the chrome
    /// revision — this reports layout output, not a mutation to resync against.
    pub(super) fn publish_strip_viewport(&self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let focused_display = self
            .is_partitioned()
            .then(|| chrome.borrow().tree().desktop.outputs.focused_display())
            .flatten();
        // Same resolution the placement rule performs.
        let strip_id = {
            let st = chrome.borrow();
            let tree = st.tree();
            tree.desktop.active_workspace().and_then(|ws| {
                tree.desktop
                    .active_strip
                    .get(&ws.id)
                    .copied()
                    .filter(|id| ws.strips.iter().any(|s| s.id == *id))
                    .or_else(|| ws.strips.first().map(|s| s.id))
            })
        };
        let viewport = strip_id.and_then(|sid| {
            let &ptr = self.group_row_ptrs.get(&sid)?;
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            let (visible_x0, visible_x1) = row.visible_canvas_span();
            // On a partitioned strip, the focused display's windows only:
            // a new window opens where the person is looking.
            let focused = focused_display.map(|d| row.group_ids_on(d));
            Some(tilo_desktop_model::commands::StripViewport {
                strip: sid,
                groups: row
                    .group_canvas_spans()
                    .into_iter()
                    .filter(|(id, _, _)| focused.as_ref().is_none_or(|on| on.contains(id)))
                    .map(|(id, x0, x1)| tilo_desktop_model::commands::GroupSpan { id, x0, x1 })
                    .collect(),
                visible_x0,
                visible_x1,
            })
        });
        let mut state = chrome.borrow_mut();
        state.set_strip_viewport(viewport);
        state.set_viewport_size((self.rect.width(), self.rect.height()));
    }

    /// The strip other than `except` whose windows `p` is over (see
    /// [`GroupRow::covers`]).
    pub(super) fn strip_over(&self, p: Point, except: StripId) -> Option<StripId> {
        self.group_row_ptrs.iter().find_map(|(&strip_id, &ptr)| {
            // SAFETY: read-only use under the stable row-pointer contract.
            let row: &GroupRow = unsafe { &*ptr };
            (strip_id != except && row.covers(p)).then_some(strip_id)
        })
    }

    /// Test-only: mutable access to the [`UiWindowGroup`] at
    /// `(strip_id, group_idx)`.
    ///
    /// SAFETY mirrors [`Self::with_group_row_ptrs`]: the pointer is valid
    /// for the host's lifetime, and the `&mut self` borrow on this method
    /// keeps any other reconcile path from aliasing.
    pub fn group_at_for_tests(
        &mut self,
        strip_id: StripId,
        group_idx: usize,
    ) -> Option<&mut UiWindowGroup> {
        let &ptr = self.group_row_ptrs.get(&strip_id)?;
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &mut GroupRow = unsafe { &mut *ptr };
        row.groups.get_mut(group_idx)
    }

    /// Resolve native keyboard ownership through the active tab, never through
    /// an arbitrary app elsewhere in the widget tree. A stale live row refuses.
    pub(crate) fn active_app_host(&self) -> Option<&ui::widget::TileHost> {
        let chrome = self.chrome.as_ref()?;
        let (strip, group, tab) = {
            let state = chrome.borrow();
            let tree = state.tree();
            let strip = *tree.desktop.active_strip.get(&tree.desktop.active)?;
            (
                strip,
                tree.desktop.active_group_for(strip)?,
                state.active_tab_id()?,
            )
        };
        let &pointer = self.group_row_ptrs.get(&strip)?;
        // SAFETY: this row is owned by the live host tree, as in
        // active_window_control_rects. The immutable host borrow keeps it live.
        let row = unsafe { &*pointer };
        let group = row.groups.iter().find(|candidate| candidate.id == group)?;
        if group.active_tab_id() != tab {
            return None;
        }
        group.active_host()
    }

    /// Live pointer targets for the active group's maximize/restore actions.
    ///
    /// This is read-only presentation state. The developer control transport
    /// serialises it when that transport is enabled, while normal production
    /// sessions never bind the transport at all.
    pub fn active_window_bounds(&self) -> Option<ui::widget::WindowGroupBounds> {
        let chrome = self.chrome.clone()?;
        let (strip_id, group_id) = {
            let st = chrome.borrow();
            let tree = st.tree();
            let strip = tree
                .desktop
                .active_strip
                .get(&tree.desktop.active)
                .copied()?;
            (strip, tree.desktop.active_group_for(strip)?)
        };
        let &ptr = self.group_row_ptrs.get(&strip_id)?;
        // SAFETY: same ownership contract as `group_at_for_tests` above.
        let row: &GroupRow = unsafe { &*ptr };
        let group = row.groups.iter().find(|group| group.id == group_id)?;
        Some(group.presentation_bounds())
    }

    /// Compatibility view for the existing developer transport.
    pub fn active_window_control_rects(&self) -> Option<(Rect, Rect)> {
        self.active_window_bounds().map(|bounds| {
            (
                bounds.maximize.unwrap_or(Rect::ZERO),
                bounds.restore.unwrap_or(Rect::ZERO),
            )
        })
    }

    /// Test-only: runs the per-frame active-group reconcile outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_active_group_reconcile_for_tests(&mut self) {
        self.reconcile_active_group_off_closing();
    }

    /// Test-only: runs the per-frame maximize-change writeback outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_maximize_harvest_for_tests(&mut self) {
        self.harvest_pending_maximize_changes();
    }

    /// Test-only: runs the per-frame width-change writeback outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_width_harvest_for_tests(&mut self) {
        self.harvest_pending_width_changes();
    }

    /// Test-only: runs the per-frame cross-strip drag-drop router outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_cross_strip_dispatch_for_tests(&mut self) {
        self.dispatch_cross_strip_detaches();
    }

    /// Test-only: runs the per-frame whole-group reorder writeback outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_group_reorder_harvest_for_tests(&mut self) {
        self.harvest_pending_group_reorder();
    }

    /// Test-only: runs the per-frame intra-group tab reorder writeback outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_tab_reorder_harvest_for_tests(&mut self) {
        self.harvest_pending_tab_reorder();
    }

    /// Test-only: runs the per-frame decoration-search-chip handling outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_search_open_for_tests(&mut self) {
        self.harvest_pending_search_open();
    }

    /// Test-only harvest for the tab strip's "+" chip.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_pending_new_tab_for_tests(&mut self) -> bool {
        self.harvest_pending_new_tab()
    }

    /// Mirrors the widget-side active tab into the tree immediately after
    /// input dispatch. The native compositor uses this before routing the
    /// next event in a backend batch; tests use it to model the same boundary.
    pub fn flush_active_tab_writeback(&mut self) {
        self.harvest_active_tab_writeback();
    }

    /// Test-only: runs the focus-follow scroll reconcile outside `update`.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_scroll_active_into_view_for_tests(&mut self) {
        self.scroll_active_group_into_view();
    }

    pub fn group_row_mut_for_tests(&mut self, strip_id: StripId) -> Option<&mut GroupRow> {
        let &ptr = self.group_row_ptrs.get(&strip_id)?;
        // SAFETY: see field doc on `group_row_ptrs`.
        Some(unsafe { &mut *ptr })
    }

    /// Mirrors any widget-side group missing from the persisted tree into
    /// it — closes the writeback gap for
    /// [`ui::widget::GroupRow::split_tab_into_new_group`], whose drag
    /// handler creates a widget-side group with no automerge access.
    /// Without this, [`Self::resync_strips_from_tree`] would see "widget
    /// has X, tree doesn't" and evict the dragged-out group.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "both guards already end at the block that binds them, right where \
                  `strip_tree_ids` (built from them) is handed back; nothing to tighten \
                  without cloning the tree out for no real gain"
    )]
    pub(super) fn harvest_widget_only_groups_into_tree(&self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // Skip if the chrome revision moved since our snapshot (tree just
        // changed) — `resync_strips_from_tree` will catch up, and running
        // here would re-add a legitimately-removed group. Widget-side
        // splits don't bump, so they stay visible to this harvest.
        {
            let cur_rev = chrome.borrow().revision;
            if cur_rev != self.last_chrome_revision {
                return;
            }
        }
        // Collect under an immutable borrow, apply under the mutable
        // one — same shape as the other harvesters.
        let mut writes: Vec<(StripId, usize, crate::shell::state::WindowGroup)> = Vec::new();
        // The live active tab of every group in the strips being written,
        // so a source group whose active tab moved out takes the tab the
        // bar itself made active (`TabBar::remove_tab`: the neighbour that
        // slid into the slot, else the last) rather than a rule of its own.
        let mut live_active: std::collections::HashMap<GroupId, u64> =
            std::collections::HashMap::new();
        // Snapshot each strip's known tab ids under one read borrow, then
        // drop it — the unsafe row-pointer walk below doesn't touch the
        // tree, so there's no reason to hold the lock across it.
        let strip_tree_ids: Vec<(StripId, std::collections::HashSet<u64>)> = {
            let st = chrome.borrow();
            let tree = st.tree();
            let Some(ws) = tree
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == tree.desktop.active)
            else {
                return;
            };
            ws.strips
                .iter()
                .map(|strip| (strip.id, strip.window_groups.iter().map(|g| g.id).collect()))
                .collect()
        };
        for (strip_id, tree_ids) in strip_tree_ids {
            let Some(&ptr) = self.group_row_ptrs.get(&strip_id) else {
                continue;
            };
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor.
            let row: &GroupRow = unsafe { &*ptr };
            for (idx, wg) in row.groups.iter().enumerate() {
                if wg.is_closing() {
                    continue;
                }
                live_active.insert(wg.id, wg.active_tab_id());
                if !tree_ids.contains(&wg.id) {
                    writes.push((strip_id, idx, build_state_group_from_widget(wg)));
                }
            }
        }
        if writes.is_empty() {
            return;
        }
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        // Groups a move emptied, with the strip they left, for the
        // active-group repair once the workspace borrow is released.
        let mut emptied: Vec<(StripId, GroupId)> = Vec::new();
        for (strip_id, idx, group) in writes {
            let moved: std::collections::HashSet<u64> = group.tabs.iter().map(|t| t.id).collect();
            for ws in &mut tree.desktop.workspaces {
                let Some(strip) = ws.strips.iter_mut().find(|s| s.id == strip_id) else {
                    continue;
                };
                // A widget-side split *moves* tabs: they left their source
                // group when the new one appeared, so the tree's copy of the
                // source must lose them too. Left in place, the next resync
                // sees "tree has it, row does not" and re-creates each one in
                // the source as a second, hostless card (and revives a source
                // the row already pruned). Until 2026-09-17 this harvest only
                // inserted, so every in-strip drag-out duplicated its tab at
                // the next chrome revision.
                for existing in &mut strip.window_groups {
                    let before = existing.tabs.len();
                    let removed_at = existing.tabs.iter().position(|t| moved.contains(&t.id));
                    existing.tabs.retain(|t| !moved.contains(&t.id));
                    if existing.tabs.len() == before {
                        continue;
                    }
                    if moved.contains(&existing.active_tab) {
                        let live = live_active
                            .get(&existing.id)
                            .copied()
                            .filter(|id| existing.tabs.iter().any(|t| t.id == *id));
                        existing.active_tab = live.unwrap_or_else(|| {
                            removed_at
                                .and_then(|i| existing.tabs.get(i))
                                .or_else(|| existing.tabs.last())
                                .map_or(0, |t| t.id)
                        });
                    }
                    if existing.tabs.is_empty() {
                        emptied.push((strip_id, existing.id));
                    }
                }
                strip
                    .window_groups
                    .retain(|g| !emptied.iter().any(|(_, id)| *id == g.id));
                let insert_at = idx.min(strip.window_groups.len());
                strip.window_groups.insert(insert_at, group.clone());
            }
            // Focus follows the tab out of a source that is gone.
            for (emptied_strip, emptied_group) in std::mem::take(&mut emptied) {
                if tree.desktop.active_group.get(&emptied_strip) == Some(&emptied_group) {
                    tree.desktop.active_group.insert(emptied_strip, group.id);
                }
            }
        }
        // Durable like a width change: a split survives the next restart.
        tree.queue_save();
    }
}

/// The window expanded over the display: `held` while it is still
/// maximized, else the focused window of the active strip if that one is.
fn expanded_group(
    tree: &crate::shell::state::WorkspaceTree,
    held: Option<GroupId>,
) -> Option<GroupId> {
    let maximized = |id: GroupId| {
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .any(|group| group.id == id && group.maximized)
    };
    if held.is_some_and(maximized) {
        return held;
    }
    let workspace = tree.desktop.active_workspace()?;
    let strip = tree.desktop.active_strip.get(&workspace.id).copied()?;
    tree.desktop
        .active_group_for(strip)
        .filter(|group| maximized(*group))
}

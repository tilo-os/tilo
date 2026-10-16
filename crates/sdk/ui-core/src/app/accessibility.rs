// Accessibility configuration, semantic focus, announcements, and action routing.

impl App {
    fn configured_screen_reader() -> crate::screen_reader::ScreenReader {
        let mut reader = crate::screen_reader::ScreenReader::new();
        reader.configure(&settings_store::accessibility::ScreenReaderSettings::load());
        reader
    }

    /// The screen-reader flag tracks [`Self::screen_reader_enabled`].
    /// Apply a preference change a control asked for (see
    /// [`Widget::take_accessibility_request`](crate::widget::Widget::take_accessibility_request)).
    /// Returns whether one was pending.
    pub fn apply_accessibility_request(&mut self) -> bool {
        let Some(request) = self.root_widget.take_accessibility_request() else {
            return false;
        };
        match request {
            crate::a11y::AccessibilityRequest::TextScale(factor) => self.set_text_scale(factor),
            crate::a11y::AccessibilityRequest::HighContrast(on) => self.set_high_contrast(on),
            crate::a11y::AccessibilityRequest::ScreenReader(on) => {
                self.set_screen_reader_enabled(on);
            }
        }
        true
    }

    pub fn text_scale(&self) -> f32 {
        self.accessibility.text_scale
    }

    /// Layout grows to fit so text never clips; the value is clamped to a
    /// sane range by the theme.
    pub fn set_text_scale(&mut self, factor: f32) {
        let _entry = self.enter_wake_scope();
        self.accessibility.text_scale = factor;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.request_rebuild();
    }

    pub fn ui_scale(&self) -> f32 {
        self.accessibility.ui_scale
    }

    pub fn set_ui_scale(&mut self, factor: f32) {
        let _entry = self.enter_wake_scope();
        self.accessibility.ui_scale = factor;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.request_rebuild();
    }

    /// Override the host-detected input environment. Useful for platform
    /// shells and deterministic tests; normal apps do not need to call it.
    pub fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        let _entry = self.enter_wake_scope();
        if self.input.interaction == environment {
            return;
        }
        self.input.interaction = environment;
        self.root_widget.set_interaction_environment(environment);
        self.invalidate_widget_resource_cache();
        self.layout.layout_dirty = true;
    }

    pub fn interaction_environment(&self) -> crate::interaction::InteractionEnvironment {
        self.input.interaction
    }

    /// Override platform HIG conventions for a native shell or deterministic
    /// cross-platform test. Ordinary apps use the host default.
    pub fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        let _entry = self.enter_wake_scope();
        if self.input.platform == conventions {
            return;
        }
        self.input.platform = conventions;
        self.root_widget.set_platform_conventions(conventions);
        self.invalidate_widget_resource_cache();
        self.layout.layout_dirty = true;
    }

    pub fn platform_conventions(&self) -> crate::platform::PlatformConventions {
        self.input.platform
    }

    pub fn high_contrast(&self) -> bool {
        self.accessibility.a11y.high_contrast
    }

    /// Off restores the captured base theme, not a hard-coded default.
    pub fn set_high_contrast(&mut self, on: bool) {
        let _entry = self.enter_wake_scope();
        if self.accessibility.a11y.high_contrast == on {
            return;
        }
        self.accessibility.a11y.high_contrast = on;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.request_rebuild();
    }

    pub fn toggle_high_contrast(&mut self) -> bool {
        let _entry = self.enter_wake_scope();
        self.set_high_contrast(!self.accessibility.a11y.high_contrast);
        self.accessibility.a11y.high_contrast
    }

    pub fn reduce_motion(&self) -> bool {
        self.accessibility.a11y.reduce_motion
    }

    pub fn set_reduce_motion(&mut self, on: bool) {
        let _entry = self.enter_wake_scope();
        if self.accessibility.a11y.reduce_motion == on {
            return;
        }
        self.accessibility.a11y.reduce_motion = on;
        self.rebuild_theme();
    }

    /// Low-vision preferences are re-applied on top, so changing the base
    /// theme doesn't discard them.
    pub fn set_theme(&mut self, theme: Theme) {
        let _entry = self.enter_wake_scope();
        self.accessibility.base_theme = theme;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.request_rebuild();
    }

    /// Disabling stops in-flight speech and returns every narration call
    /// site to a no-op.
    pub fn set_screen_reader_enabled(&mut self, enabled: bool) {
        let _entry = self.enter_wake_scope();
        match (enabled, self.accessibility.screen_reader.is_some()) {
            (true, false) => {
                self.accessibility.screen_reader = Some(Self::configured_screen_reader());
                self.announce_current_focus();
            }
            (false, true) => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.stop();
                }
                self.accessibility.screen_reader = None;
                self.accessibility.live_region_last.clear();
            }
            _ => {}
        }
    }

    pub fn screen_reader_enabled(&self) -> bool {
        self.accessibility.screen_reader.is_some()
    }

    /// Installs a caller-provided reader (e.g. over a test `NullSpeaker` so
    /// headless tests can assert on spoken phrases).
    #[doc(hidden)]
    pub fn set_screen_reader(&mut self, reader: Option<crate::screen_reader::ScreenReader>) {
        let _entry = self.enter_wake_scope();
        self.accessibility.screen_reader = reader;
    }

    #[doc(hidden)]
    pub fn screen_reader(&self) -> Option<&crate::screen_reader::ScreenReader> {
        self.accessibility.screen_reader.as_ref()
    }

    fn announce_current_focus(&mut self) {
        let Some(id) = self.focused_node else { return };
        let node = self
            .inspect_node(id, |widget| widget.accessibility())
            .ok()
            .flatten();
        if let (Some(node), Some(sr)) = (node, self.accessibility.screen_reader.as_mut()) {
            sr.announce_focus(&node);
        }
    }

    /// Bindings deliberately avoid Tab (focus order), Alt+arrows / bare
    /// arrows (directional focus) and Ctrl+keys (undo / app shortcuts):
    ///
    /// | Keys | Action |
    /// |---|---|
    /// | `Insert + Down`  | Read all |
    /// | `Insert + Right` | Read next |
    /// | `Insert + Left`  | Read previous |
    /// | `Insert + Up`    | Where am I |
    /// | `Insert + H`     | Next heading |
    /// | `Insert + Shift + H` | Previous heading |
    /// | `Insert + F`     | Next form field |
    /// | `Insert + Shift + F` | Previous form field |
    /// | `Insert + Escape`| Stop |
    ///
    /// `Insert` itself is swallowed but reported as not-a-command, so it
    /// doesn't suppress the following keypress's normal handling.
    fn handle_screen_reader_key(&mut self, event: &WindowEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};

        if self.accessibility.screen_reader.is_none() {
            return false;
        }
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };

        if ke.logical_key == Key::Named(NamedKey::Insert) {
            self.accessibility.screen_reader_modifier_down = ke.state == ElementState::Pressed;
            return false;
        }

        if ke.state != ElementState::Pressed || !self.accessibility.screen_reader_modifier_down {
            return false;
        }

        // Handled before the named-key arm since winit delivers H/F as
        // character keys, not named keys.
        if let Key::Character(ref c) = ke.logical_key {
            let shift = self.input.modifiers.state().shift_key();
            match c.to_ascii_lowercase().as_str() {
                "h" => {
                    if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                        if shift {
                            sr.read_prev_heading(self.root_widget.as_ref());
                        } else {
                            sr.read_next_heading(self.root_widget.as_ref());
                        }
                    }
                    return true;
                }
                "f" => {
                    if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                        if shift {
                            sr.read_prev_control(self.root_widget.as_ref());
                        } else {
                            sr.read_next_control(self.root_widget.as_ref());
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }

        let Key::Named(named) = ke.logical_key else {
            return false;
        };
        match named {
            NamedKey::ArrowDown => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.read_all(self.root_widget.as_ref());
                }
                true
            }
            NamedKey::ArrowRight => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.read_next(self.root_widget.as_ref());
                }
                true
            }
            NamedKey::ArrowLeft => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.read_prev(self.root_widget.as_ref());
                }
                true
            }
            NamedKey::ArrowUp => {
                self.announce_current_focus();
                true
            }
            NamedKey::Escape => {
                if let Some(sr) = self.accessibility.screen_reader.as_mut() {
                    sr.stop();
                }
                true
            }
            _ => false,
        }
    }
    fn update_focusable_order(&mut self) {
        let _profile = tilo_resource::profiling::Phase::start("ui.focus_order");
        if self
            .actions
            .action_palette
            .as_ref()
            .is_some_and(|palette| palette.is_presented())
        {
            let node = self.layout.node_map.keys().copied().find(|&node| {
                self.inspect_node(node, |widget| widget.test_id() == Some("action-palette"))
                    .unwrap_or(false)
            });
            self.focusable_order = node.into_iter().collect();
            self.accessibility.focus_trap_active = true;
            self.focused_node = node;
            return;
        }
        if self.close_admission.dialog.is_none() {
            let restrictions = crate::history_mode::embedded::FocusRestrictions::collect(
                self.root_widget.as_ref(),
                &self.layout.taffy,
            );
            if self.history.is_some() || restrictions.has_surfaces() {
                self.update_history_focus_order(&restrictions);
                return;
            }
        }
        self.focusable_order.clear();
        let Some(root) = self.layout.root_node else {
            return;
        };

        let (order, trap_active) = self
            .with_native_presentation(|widget, _, _, taffy, _| {
                crate::focus_navigation::order(root, widget, taffy)
            })
            .unwrap_or_default();
        self.focusable_order = order;
        if let Some(next) = crate::focus_navigation::reconcile_trap(
            &mut self.accessibility.focus_trap_active,
            &mut self.accessibility.focus_before_trap,
            self.focused_node,
            &self.focusable_order,
            trap_active,
        ) {
            self.set_focused_node(next);
        }
    }

    /// The decode lives on [`crate::presence::PresenceCtx`] so the
    /// compositor reads presence the same way an app does; what stays here
    /// is what only a document holder can do (follow a peer's viewport,
    /// answer a late joiner with full state).
    fn apply_remote_presence(
        &mut self,
        user_id: String,
        key: crate::presence::PresenceKey,
        payload: crate::collaboration::PresencePayload,
    ) {
        use crate::presence::RemotePresenceEffect;
        // Keep canonical anchors intact. Resolve against each presented layout,
        // never retain an arrival-time layout id across a rebuild.
        let interaction = match &payload {
            crate::collaboration::PresencePayload::Interaction(update) => Some((**update).clone()),
            _ => None,
        };
        match self
            .presence_ctx
            .apply_remote_presence(user_id, key, payload)
        {
            RemotePresenceEffect::None | RemotePresenceEffect::PeerArrived => {}
            RemotePresenceEffect::PeerViewChanged => self.apply_follow(),
            RemotePresenceEffect::InteractionAdvanced => {
                if let Some(interaction) = interaction
                    && let Some(text) = self.presence_ctx.interaction_announcement(&interaction)
                    && let Some(reader) = self.accessibility.screen_reader.as_mut()
                {
                    let assertive = matches!(
                        interaction.phase,
                        tilo_actions::InteractionPhase::Failed { .. }
                    );
                    reader.announce(
                        text,
                        if assertive {
                            tts::Priority::Important
                        } else {
                            tts::Priority::Notification
                        },
                        assertive,
                    );
                }
            }
        }
    }

    pub(super) fn apply_undo_entry(&mut self, entry: crate::state::UndoEntry) {
        if let Some(idx) = entry.focused_node_idx {
            if let Some(&id) = self.focusable_order.get(idx) {
                self.set_focused_node(Some(id));
                if let Some(metadata) = entry.widget_metadata {
                    let _ =
                        self.mutate_node(id, |widget| crate::undo::set_state(widget, &metadata));
                    self.focus_history.stable_metadata = Some(metadata);
                }
                self.focus_history.stable_focus_idx = Some(idx);
            }
        } else {
            self.set_focused_node(None);
            self.focus_history.stable_focus_idx = None;
            self.focus_history.stable_metadata = None;
        }
        self.layout.layout_dirty = true;
        self.needs_sync = true;
        self.invalidate_widget_resource_cache();
    }

    /// Unique admitted node visits keep child reports and requests off shell history.
    fn service_history(&mut self) -> bool {
        if self.framework_input.get()
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.captures_input())
        {
            return false;
        }
        // The sweep follows every mapped node: forget widgets replaced since
        // the map was admitted first. Survivors keep what they queued for the
        // sweep after the next layout.
        self.forget_replaced_widgets();
        // SAFETY: App exclusively owns this stable live layout for the sweep.
        let mut changed = unsafe {
            crate::undo::record_dispatched_action_bound(
                &mut self.state,
                std::ptr::from_mut(self.root_widget.as_mut()),
                &self.layout.node_map,
                &self.bindings,
                &self.collaboration.local_author,
                &self.permission_ctx,
            )
        };
        // What the whole view says about history is read once per sweep, not
        // once per node (each read walks the tree, a bound status opens its
        // record): only a request taken in the sweep can change it.
        let mut embedded = crate::history_mode::embedded::has_surfaces(self.root_widget.as_ref());
        let mut local_status = crate::undo::local_undo_status(self.root_widget.as_ref());
        let mut bound_statuses: Vec<(
            crate::bound_dispatch::CallbackBinding,
            crate::undo::UndoStatus,
        )> = Vec::new();
        let mut visited = std::collections::HashSet::new();
        for (&node, pointer) in &self.layout.node_map {
            // SAFETY: this layout is unchanged, so every node pointer is live.
            if !visited.insert(unsafe { crate::widget::node_sweep_identity(*pointer) }) {
                continue;
            }
            let binding = self.bindings.binding_for(node);
            // SAFETY: no target reference escapes and this layout is unchanged.
            let request = unsafe {
                crate::bound_dispatch::mutate_bound_node(
                    &self.bindings,
                    &self.layout.node_map,
                    node,
                    |widget| crate::undo::take_undo_request(widget),
                )
            }
            .ok()
            .flatten();
            if let Some(request) = request {
                let entry = if let Some(binding) = &binding {
                    binding
                        .history_request(&mut self.state.doc, request)
                        .ok()
                        .flatten()
                } else if let Some(owner) = self.bindings.legacy_for(node) {
                    crate::bound_dispatch::legacy_history_request(
                        &owner,
                        request,
                        &self.permission_ctx,
                        &self.collaboration.local_author,
                    )
                    .ok()
                    .flatten()
                } else {
                    match request {
                        crate::undo::UndoRequest::Undo => self.state.undo_as(
                            Some(&self.collaboration.local_author),
                            None,
                            None,
                            Some(&self.actions.action_registry),
                        ),
                        crate::undo::UndoRequest::Redo => self.state.redo_as(
                            Some(&self.collaboration.local_author),
                            None,
                            None,
                            Some(&self.actions.action_registry),
                        ),
                    }
                };
                changed |= entry.is_some();
                embedded = crate::history_mode::embedded::has_surfaces(self.root_widget.as_ref());
                local_status = crate::undo::local_undo_status(self.root_widget.as_ref());
                bound_statuses.clear();
            }
            // The former eager status read flushed open transactions whenever
            // history existed. Preserve that boundary without deriving labels.
            if binding.is_none()
                && !embedded
                && (self
                    .state
                    .can_undo_as(Some(&self.collaboration.local_author))
                    || self
                        .state
                        .can_redo_as(Some(&self.collaboration.local_author)))
            {
                self.state.doc.get_heads();
            }
            // Bound histories intentionally keep completed-action labels warm
            // across pending edits and dynamically mounted controls.
            let bound_status = binding
                .as_ref()
                .map(|binding| {
                    if let Some((_, status)) = bound_statuses
                        .iter()
                        .find(|(seen, _)| seen.same_component(binding))
                    {
                        return status.clone();
                    }
                    let status = binding.undo_status(&mut self.state.doc).unwrap_or_default();
                    bound_statuses.push((binding.clone(), status.clone()));
                    status
                })
                .or_else(|| {
                    self.bindings
                        .legacy_status(node, &self.collaboration.local_author)
                });
            let mut status = || {
                bound_status.clone().unwrap_or_else(|| {
                    if let Some(local) = &local_status {
                        return match local {
                            crate::undo::LocalUndoStatus::Ready(status) => status.clone(),
                            crate::undo::LocalUndoStatus::Pending => Default::default(),
                        };
                    }
                    if embedded {
                        self.state
                            .undo_status_snapshot(Some(&self.collaboration.local_author))
                    } else {
                        self.state
                            .undo_status(Some(&self.collaboration.local_author))
                    }
                })
            };
            // SAFETY: the same unique admitted node remains live and exclusive.
            let _ = unsafe {
                crate::bound_dispatch::mutate_bound_node(
                    &self.bindings,
                    &self.layout.node_map,
                    node,
                    |widget| crate::undo::set_undo_status(widget, &mut status),
                )
            };
        }
        changed
    }

    /// Finalize the document boundary for a handled native event. Shared host
    /// controls only change presentation and must not close pending app typing.
    #[cfg(feature = "native")]
    fn finalize_handled_native_event(
        &mut self,
        event: &WindowEvent,
        is_undo_redo: bool,
        history_handled: bool,
    ) {
        let character_input = matches!(event, WindowEvent::KeyboardInput { event, .. }
            if matches!(event.logical_key, winit::keyboard::Key::Character(_)));
        if !is_undo_redo && !history_handled && !character_input {
            self.commit_action();
        }
    }

    fn commit_action(&mut self) {
        self.commit_action_with_invalidation(true);
    }

    fn commit_action_with_invalidation(&mut self, invalidate: bool) {
        if self.framework_input.get()
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.captures_input())
        {
            return;
        }
        let history_changed = self.service_history();
        if history_changed {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        // Called after every handled event, many of which (e.g. a
        // selection-drag `CursorMoved`) mutate only ephemeral widget state,
        // not the CRDT — `changed` lets those skip persisting/syncing.
        let changed = self.state.commit_action_authored_with_context(
            self.focus_history.stable_focus_idx,
            self.focus_history.stable_metadata.clone(),
            Some(self.collaboration.local_author.clone()),
            None,
        );
        self.focus_history.stable_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.focus_history.stable_metadata = self.focused_node.and_then(|id| {
            self.inspect_node(id, |widget| crate::undo::get_state(widget))
                .ok()
                .flatten()
        });
        if changed || history_changed || invalidate {
            self.invalidate_widget_resource_cache();
        }
        self.needs_sync |= changed;

        // Incremental delta, not the whole document — serialising per
        // keystroke scales with document size. `UndoLog::apply_inverse`
        // writes the inverse as new changes, so a delta carries undo/redo
        // like any other edit.
        if changed {
            self.broadcast_doc_delta();
        }
    }

    fn set_focused_node(&mut self, new_id: Option<NodeId>) {
        self.mark_semantic_tree_stale();
        self.suspend_history_focus(new_id);
        if new_id.is_some_and(|node| self.is_history_node(node)) {
            self.remember_history_content();
        }
        self.root_widget.focus_requested(new_id, &self.layout.taffy);
        if self.focused_node != new_id {
            if let Some(old) = self.focused_node {
                let _ = self.mutate_node(old, |widget| widget.set_focus(false));
            }
            self.focused_node = new_id;
            if let Some(node) = new_id {
                let _ = self.mutate_node(node, |widget| widget.set_focus(true));
                if self.accessibility.screen_reader.is_some() {
                    let access = self
                        .inspect_node(node, |widget| widget.accessibility())
                        .ok()
                        .flatten();
                    if let (Some(access), Some(reader)) =
                        (access, self.accessibility.screen_reader.as_mut())
                    {
                        reader.announce_focus(&access);
                    }
                }
            }
            self.layout.layout_dirty = true;
            self.needs_sync = true;
            self.invalidate_widget_resource_cache();
        }
    }

    /// The platform IME contract for the focused widget. A field opts in via
    /// [`Widget::ime_purpose`]; buttons and other focusable controls therefore
    /// never activate `text-input-v3` accidentally.
    #[cfg(feature = "native")]
    fn ime_window_state(&self) -> Option<ImeWindowState> {
        self.inspect_node(self.focused_node?, ImeWindowState::from_widget)
            .ok()
            .flatten()
    }

    #[cfg(feature = "native")]
    fn focused_ime_requires_deferred_ack(&self) -> bool {
        self.focused_node
            .and_then(|node| {
                self.inspect_node(node, |widget| widget.ime_requires_deferred_ack())
                    .ok()
            })
            .unwrap_or(false)
    }

    #[cfg(feature = "native")]
    fn focused_state_snapshot(&self) -> Option<(NodeId, String)> {
        self.accessibility.screen_reader.as_ref()?;
        let id = self.focused_node?;
        let node = self
            .inspect_node(id, |widget| widget.accessibility())
            .ok()
            .flatten()?;
        Some((id, crate::screen_reader::describe_state(&node)))
    }

    /// Only fires if focus did NOT move — a move is narrated separately by
    /// [`set_focused_node`].
    #[cfg(feature = "native")]
    fn announce_state_change(&mut self, pre: Option<(NodeId, String)>) {
        let Some((pre_id, pre_state)) = pre else {
            return;
        };
        if self.focused_node != Some(pre_id) {
            return;
        }
        if self.accessibility.screen_reader.is_none() {
            return;
        }
        let Some(node) = self
            .inspect_node(pre_id, |widget| widget.accessibility())
            .ok()
            .flatten()
        else {
            return;
        };
        let now = crate::screen_reader::describe_state(&node);
        if now.is_empty() || now == pre_state {
            return;
        }
        if let Some(sr) = self.accessibility.screen_reader.as_mut() {
            sr.announce_value_change(now);
        }
    }

    #[cfg(feature = "native")]
    fn text_review_snapshot(&self) -> Option<(NodeId, usize, String)> {
        self.accessibility.screen_reader.as_ref()?;
        let id = self.focused_node?;
        let (caret, text) = self
            .inspect_node(id, |widget| widget.text_review())
            .ok()
            .flatten()?;
        Some((id, caret, text))
    }

    /// Typing is left to the widget's own echo so this never double-speaks
    /// an edit.
    #[cfg(feature = "native")]
    fn announce_text_review(&mut self, pre: Option<(NodeId, usize, String)>) {
        let Some((pre_id, pre_caret, pre_text)) = pre else {
            return;
        };
        if self.focused_node != Some(pre_id) {
            return;
        }
        if self.accessibility.screen_reader.is_none() {
            return;
        }
        let Some((now_caret, now_text)) = self
            .inspect_node(pre_id, |widget| widget.text_review())
            .ok()
            .flatten()
        else {
            return;
        };
        if now_text != pre_text || now_caret == pre_caret {
            return;
        }
        let single_step = next_boundary(&now_text, pre_caret) == now_caret
            || prev_boundary(&now_text, pre_caret) == now_caret;
        if single_step {
            // Character to the caret's right; fall back left at end of text.
            let ch = now_text
                .get(now_caret..)
                .and_then(|s| s.chars().next())
                .or_else(|| {
                    now_text
                        .get(..now_caret)
                        .and_then(|s| s.chars().next_back())
                });
            if let (Some(ch), Some(sr)) = (ch, self.accessibility.screen_reader.as_mut()) {
                sr.echo_char(ch);
            }
        } else if let Some(word) = word_at(&now_text, now_caret)
            && let Some(sr) = self.accessibility.screen_reader.as_mut()
        {
            sr.echo_word(&word);
        }
    }

    /// Announces changed [live regions](crate::a11y::AccessNode::live)
    /// without moving focus.
    #[cfg(feature = "native")]
    fn scan_live_regions(&mut self) {
        fn visit(w: &dyn Widget, out: &mut Vec<(String, crate::a11y::Politeness, String)>) {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(w) else {
                return;
            };
            if let Some(node) = w.accessibility()
                && let (Some(politeness), Some(name)) = (node.live, node.name.clone())
            {
                out.push((name, politeness, crate::screen_reader::describe(&node)));
            }
            w.for_each_child(&mut |child| visit(child, out));
        }
        if self.accessibility.screen_reader.is_none() {
            return;
        }
        // A live region without an accessible name can't be tracked stably
        // across frames, so it's skipped.
        let mut seen: Vec<(String, crate::a11y::Politeness, String)> = Vec::new();
        visit(self.root_widget.as_ref(), &mut seen);

        let mut current_keys = std::collections::HashSet::new();
        let mut to_announce: Vec<(crate::a11y::Politeness, String)> = Vec::new();
        for (key, politeness, text) in seen {
            current_keys.insert(key.clone());
            let changed = self.accessibility.live_region_last.get(&key) != Some(&text);
            self.accessibility
                .live_region_last
                .insert(key, text.clone());
            // Clearing a region to empty updates the snapshot silently.
            if changed && !text.is_empty() {
                to_announce.push((politeness, text));
            }
        }
        // Drop snapshots for regions that left the tree, so a region that
        // reappears with the same text announces afresh.
        self.accessibility
            .live_region_last
            .retain(|k, _| current_keys.contains(k));

        if let Some(sr) = self.accessibility.screen_reader.as_mut() {
            for (politeness, text) in to_announce {
                let (priority, interrupt) = match politeness {
                    // Assertive cuts in; polite is queued at notification
                    // priority and never interrupts in-flight speech.
                    crate::a11y::Politeness::Assertive => (tts::Priority::Important, true),
                    crate::a11y::Politeness::Polite => (tts::Priority::Notification, false),
                };
                sr.announce(text, priority, interrupt);
            }
        }
    }

    fn navigate_focus(&mut self, direction: Direction) {
        let Some(current_id) = self.focused_node else {
            if let Some(&first) = self.focusable_order.first() {
                self.set_focused_node(Some(first));
            }
            return;
        };
        let Ok(current_rect) = self.inspect_node(current_id, |widget| widget.get_rect()) else {
            return;
        };
        let best_candidate = crate::focus_navigation::next(
            (current_id, current_rect),
            self.focusable_order.iter().filter_map(|&id| {
                self.inspect_node(id, |widget| widget.get_rect())
                    .ok()
                    .map(|rect| (id, rect))
            }),
            direction,
        );
        if let Some(new_id) = best_candidate {
            self.set_focused_node(Some(new_id));
        }
    }

    /// Gated on the focus-trap signal, not `modal_capture`: that governs only
    /// pointer click-through — gating Tab on it instead either froze focus
    /// inside a dialog or let it escape with no capture.
    #[cfg(feature = "native")]
    fn keyboard_nav_allowed(&self, modal_rect: Option<vello::kurbo::Rect>) -> bool {
        crate::focus_navigation::keyboard_nav_allowed(
            modal_rect,
            self.accessibility.focus_trap_active,
        )
    }

    /// Tab normally belongs to the host's focus order. A focused editor-like
    /// widget may explicitly claim it first (for indentation or completion),
    /// without broadening that exception to arbitrary widgets or escaping an
    /// active focus trap.
    #[cfg(feature = "native")]
    fn focused_widget_wants_tab_input(&self) -> bool {
        self.focused_node.is_some_and(|node| {
            self.inspect_node(node, |widget| widget.wants_tab_input())
                .unwrap_or(false)
        })
    }

    /// Shared native Tab admission, preserving text-input and modal capture.
    #[cfg(feature = "native")]
    fn handle_tab_navigation(
        &mut self,
        event: &WindowEvent,
        modal_rect: Option<KurboRect>,
    ) -> bool {
        if self.focused_widget_wants_tab_input() || !self.keyboard_nav_allowed(modal_rect) {
            return false;
        }
        if matches!(event, WindowEvent::KeyboardInput { event, .. }
            if event.state == winit::event::ElementState::Pressed
                && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab))
        {
            return self.focus_tab(self.input.modifiers.state().shift_key());
        }
        false
    }

    fn focus_tab(&mut self, shift: bool) -> bool {
        let len = self.focusable_order.len();
        if len == 0 {
            return false;
        }
        // Shown inside a host whose focus order continues past the app, Tab
        // from the last control (Shift+Tab from the first) leaves the app.
        if self.input.tab_order_continues_in_host {
            let at = self
                .focused_node
                .and_then(|id| self.focusable_order.iter().position(|&n| n == id));
            if at == Some(if shift { 0 } else { len - 1 }) {
                self.set_focused_node(None);
                return false;
            }
        }
        let Some(next_idx) =
            crate::focus_navigation::tab_index(&self.focusable_order, self.focused_node, shift)
        else {
            return false;
        };
        if let Some(&id) = self.focusable_order.get(next_idx) {
            self.set_focused_node(Some(id));
        }
        true
    }

    /// `scale` is the window scale factor so bounds land in physical pixels
    /// (what AT-SPI expects).
    pub fn build_a11y_tree_update(&mut self, scale: f64) -> accesskit::TreeUpdate {
        let _entry = self.enter_wake_scope();
        let exported = self.bindings.exported_nodes(&self.layout.node_map);
        if self
            .actions
            .action_palette
            .as_ref()
            .is_some_and(|p| p.is_presented())
        {
            return crate::accesskit_bridge::build_presentation_with_resources_bound(
                &self.history_presentation_roots(),
                &exported.nodes,
                exported.focus(self.focused_node),
                scale,
                &[],
            );
        }
        if let Some(history) = &self.history {
            let resources = if history.live_interactive() {
                self.collect_declared_resources()
            } else {
                vec![]
            };
            return crate::accesskit_bridge::build_presentation_with_resources_bound(
                &self.history_presentation_roots(),
                &exported.nodes,
                exported.focus(self.focused_node),
                scale,
                &resources,
            );
        }
        let mut resources = self.collect_declared_resources();
        resources.extend(self.history_local_resources());
        self.with_native_presentation(|root, nodes, focused, _, _| {
            crate::accesskit_bridge::build_tree_update_with_resources_bound(
                root, nodes, focused, scale, &resources,
            )
        })
        .unwrap_or_else(|_| {
            crate::accesskit_bridge::build_tree_update_with_resources_bound(
                self.root_widget.as_ref(),
                &self.layout.node_map,
                self.focused_node,
                scale,
                &self.collect_declared_resources(),
            )
        })
    }
    fn find_resource_action(
        resources: &[tilo_actions::Resource],
        target: accesskit::NodeId,
    ) -> Option<tilo_actions::ActionAddress> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|spec| {
                    crate::accesskit_bridge::resource_action_node_id(resource.id.as_str(), &spec.id)
                        == target
                })
                .map(|spec| tilo_actions::ActionAddress::new(resource.id.as_str(), &spec.id))
                .or_else(|| Self::find_resource_action(&resource.children, target))
        })
    }

    fn dispatch_resource_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> bool {
        use crate::a11y::Action;

        let argument = match action {
            Action::Activate | Action::ToggleCheck => Some(None),
            Action::SetValue(value) => Some(Some(value.as_str())),
            _ => None,
        };
        let Some(argument) = argument else {
            return false;
        };
        let mut resources = self.collect_declared_resources();
        resources.extend(self.history_local_resources());
        let Some(address) = Self::find_resource_action(&resources, target) else {
            return false;
        };
        self.invoke_local_address(&address, argument);
        true
    }

    /// The local actor's door onto one action: host services first, then an
    /// action that asks for confirmation through the palette, then the app's
    /// own. Assistive technology and host chrome both enter here.
    pub(super) fn invoke_local_address(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> bool {
        if let Some(result) = self.invoke_history_address_local(address, argument) {
            return result.is_ok();
        }
        let resources = self.collect_declared_resources();
        let confirmation = tilo_actions::door::resolve(&resources, address)
            .map(|spec| spec.confirmation_for_argument(argument))
            .unwrap_or_default();
        if !matches!(confirmation, tilo_actions::ConfirmationPolicy::None) {
            return self.open_action_palette_at(address);
        }
        let actor = self.collaboration.local_author.0.clone();
        let accepted = self
            .invoke_resource_action(ResourceInvocation::new(address, &actor).argument(argument))
            .is_ok();
        if accepted {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        accepted
    }

    /// Resolves both retained AccessKit nodes and synthetic semantic children
    /// (virtual rows/cells) to their owning widget. The widget gets first
    /// refusal; synthetic children additionally carry an opaque stable target
    /// produced by that widget. If a retained widget declines, `Action::Focus`
    /// moves keyboard focus and activation returns click coordinates.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered native accessibility door preserves existing resource, semantic and pointer fallback precedence"
    )]
    pub fn dispatch_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Option<(f64, f64)> {
        use crate::a11y::Action;
        use crate::accesskit_bridge::A11yTarget;

        let _entry = self.enter_wake_scope();
        // The target is resolved through the layout's map: widgets replaced
        // since it was built are forgotten first, and an action that arrives
        // before the next layout admits their replacements finds nothing.
        self.forget_replaced_widgets();

        self.refresh_control_action_availability();
        // A retained AT id is operable only while the exact target is exported
        // in the current presentation; hidden resources cannot bypass chrome.
        if !self
            .build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(id, _)| *id == target)
        {
            return None;
        }
        if self.dispatch_resource_a11y_action(target, action) {
            return None;
        }

        let exported = self.bindings.exported_nodes(&self.layout.node_map);
        let resolved = if self.history.is_some() {
            self.history_presentation_roots()
                .into_iter()
                .find_map(|root| exported.resolve(root, target))?
        } else {
            self.with_native_presentation(|root, _, _, _, _| exported.resolve(root, target))
                .ok()
                .flatten()?
        };
        let (taffy_id, key_path, ordinal_path, semantic_target, synthetic) = match resolved {
            A11yTarget::Root => return None,
            A11yTarget::Widget(id) => (id, Vec::new(), Vec::new(), None, false),
            A11yTarget::Synthetic {
                owner,
                key_path,
                ordinal_path,
                semantic_target,
            } => (owner, key_path, ordinal_path, semantic_target, true),
        };
        if let Some(target) = semantic_target
            .as_deref()
            .filter(|t| t.starts_with("ordinary-confirmation:"))
        {
            self.bindings.clear();
            self.layout.node_map.clear();
            self.focused_node = None;
            let handled = self.actions.action_palette.as_mut().is_some_and(|palette| {
                palette.perform_semantic_action(
                    target,
                    action,
                    &mut crate::widget::Edits::lend(&mut self.state.doc),
                    &self.state.root,
                )
            });
            self.finish_action_palette_input(handled);
            return None;
        }
        let framework = crate::history_mode::embedded::owns_node(
            self.root_widget.as_ref(),
            taffy_id,
            &self.layout.taffy,
        ) || self
            .history
            .as_ref()
            .is_some_and(|history| history.owns_node(taffy_id, &self.layout.taffy));
        let history_frame = crate::history_mode::frame::FrameworkInputFrame::new(
            &self.framework_input,
            framework || self.bindings.owns_document(taffy_id),
        );
        if framework {
            self.suspend_history_focus(Some(taffy_id));
        }
        // Who is acted on, as an identity that survives a rebuild the action
        // itself may cause.
        // SAFETY: the action below reaches this node through the same
        // admitted, unchanged maps.
        let mut acted = matches!(action, Action::Focus)
            .then(|| unsafe {
                crate::widget::layout::CompositionFocus::capture(
                    Some(taffy_id),
                    &self.layout.node_map,
                    &self.bindings,
                )
            })
            .flatten();
        let handled = self
            .dispatch_semantic_node(taffy_id, action, |widget, doc, root| {
                if let Some(target) = semantic_target.as_deref() {
                    widget.perform_semantic_action(
                        target,
                        action,
                        &mut crate::widget::Edits::lend(doc),
                        root,
                    )
                } else if synthetic {
                    widget.perform_action(
                        action,
                        &key_path,
                        &mut crate::widget::Edits::lend(doc),
                        root,
                    )
                } else {
                    widget.perform_action(action, &[], &mut crate::widget::Edits::lend(doc), root)
                }
            })
            .ok()?;
        drop(history_frame);
        // The action may have replaced widgets: their nodes are forgotten
        // before anything below looks a node up, and the node acted on is
        // focused only while it is still the one the map names.
        let replaced = self.forget_replaced_widgets();
        if handled && replaced {
            self.actions.focus_after_rebuild = acted.take();
        }
        if handled {
            if !replaced
                && (matches!(action, Action::Focus)
                    || (matches!(action, Action::SetSelection { .. })
                        && self
                            .inspect_node(taffy_id, |widget| widget.is_focused())
                            .unwrap_or(false)))
            {
                self.set_focused_node(Some(taffy_id));
            }
            // `Widget::perform_action` is deliberately the same path as a
            // keyboard or pointer activation.  Action-bound controls queue a
            // control intent there; drain it before publishing the next
            // AccessKit update so an AT-SPI DoAction never reports success
            // while leaving the visible state unchanged.
            if framework {
                self.refresh_framework_history(
                    self.layout.last_layout_logical.map_or(900.0, |size| size.0),
                );
            }
            if framework {
                self.service_pending_history_commands();
            } else {
                self.dispatch_control_invocations();
            }
            self.invalidate_widget_resource_cache();
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            if framework {
                self.refresh_framework_history(
                    self.layout.last_layout_logical.map_or(900.0, |size| size.0),
                );
            }
            return None;
        }

        if replaced {
            // The map that named this node is gone with the widgets the
            // action replaced: only focus resumes, once the next layout
            // admits the new composition; nothing is clicked at a guess.
            if matches!(action, Action::Focus) {
                self.actions.focus_after_rebuild = acted.take();
            }
            return None;
        }
        match action {
            Action::Focus => {
                if synthetic && !ordinal_path.is_empty() {
                    let _ = self.mutate_node(taffy_id, |widget| {
                        widget.focus_synthetic_accessibility(&ordinal_path)
                    });
                }
                self.set_focused_node(Some(taffy_id));
                None
            }
            Action::Activate | Action::ToggleCheck => {
                let rect = if synthetic {
                    self.inspect_node(taffy_id, |widget| {
                        widget.synthetic_accessibility_rect(&ordinal_path)
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| {
                        self.inspect_node(taffy_id, |widget| widget.get_rect())
                            .unwrap_or(KurboRect::ZERO)
                    })
                } else {
                    self.inspect_node(taffy_id, |widget| widget.get_rect())
                        .unwrap_or(KurboRect::ZERO)
                };
                let c = rect.center();
                Some((c.x, c.y))
            }
            _ => None,
        }
    }

    /// Complete an accessibility activation for legacy hand-hit-tested
    /// controls that do not implement `perform_action` yet. The semantic
    /// target has already resolved to this exact point; routing the usual
    /// move/press/release sequence through the widget tree preserves the same
    /// behavior as hardware input instead of reporting a false success.
    fn dispatch_semantic_click(&mut self, x: f64, y: f64) -> bool {
        self.mark_semantic_tree_stale();
        self.refresh_control_action_availability();
        if self.layout.root_node.is_none() {
            return false;
        }
        let events = [
            crate::synthetic_input::cursor_moved(x, y),
            crate::synthetic_input::mouse_press(),
            crate::synthetic_input::mouse_release(),
        ];
        let mut handled_any = false;
        for event in events {
            if self.history_controls_event(&event, 1.0) {
                handled_any |= self.dispatch_framework_history_event(&event, 1.0);
                continue;
            }
            let mut handled = false;
            let mut focus_target = None;
            self.presentation_handle_event(&event, 1.0, &mut handled, &mut focus_target);
            // A target named by the map the event saw is gone once the
            // event replaced widgets.
            if self.forget_replaced_widgets() {
                focus_target = None;
            }
            if let Some(id) = focus_target {
                self.set_focused_node(Some(id));
            }
            handled_any |= handled;
        }
        if handled_any {
            self.dispatch_control_invocations();
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        handled_any
    }

    /// Node identity is preserved for everything that recurred, so a
    /// consumer reacts to just the diff.
    ///
    /// A refresh after nothing the tree is read from changed (no input, no
    /// document or widget change, no layout, no focus move since the last
    /// refresh: [`Self::mark_semantic_tree_stale`]) is that empty diff
    /// without walking the widgets, so a frame that changed nothing costs
    /// the host's accessibility walk nothing. Any such change publishes on
    /// the refresh that follows it.
    pub fn refresh_semantic_tree(&mut self) -> crate::a11y_tree::TreeDelta {
        if self.semantic_tree_current() {
            return self.unchanged_semantic_tree();
        }
        self.accessibility.semantic_tree_builds =
            self.accessibility.semantic_tree_builds.wrapping_add(1);
        self.rebuild_semantic_tree()
    }

    /// Something the semantic tree is read from may have changed: the next
    /// [`Self::refresh_semantic_tree`] walks the widgets again.
    pub(super) fn mark_semantic_tree_stale(&mut self) {
        self.accessibility.semantic_tree_stale = true;
        self.accessibility.semantic_tree_marks =
            self.accessibility.semantic_tree_marks.wrapping_add(1);
    }

    /// The refreshes that walked the widgets and rebuilt the semantic tree,
    /// rather than finding that nothing it is read from had changed.
    #[cfg(any(test, feature = "test-support"))]
    pub fn semantic_tree_builds_for_test(&self) -> u64 {
        self.accessibility.semantic_tree_builds
    }

    /// Whether the tree already says what a walk would find: nothing it is
    /// read from changed since it was built, no sync or layout is pending
    /// that would change it, and no presentation the change tracking does
    /// not follow (the palette, the close dialog, a History review) is up.
    fn semantic_tree_current(&self) -> bool {
        !self.accessibility.semantic_tree_stale
            && !self.needs_sync
            && !self.layout.layout_dirty
            && !self.layout.structure_dirty
            && !self.semantic_tree_untracked()
    }

    /// Presentations the tree is read from without the widget, document and
    /// layout changes [`Self::mark_semantic_tree_stale`] follows: each
    /// refresh while one is up walks again, and so does the first after it.
    /// A framework History at rest is tracked: what changes it refreshes it
    /// (`refresh_framework_history`), and that invalidates the widgets.
    fn semantic_tree_untracked(&self) -> bool {
        self.close_admission.dialog.is_some()
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.review_is_open())
            || self.history_layout.is_some()
            || self.history_chrome_enabled
            || self
                .actions
                .action_palette
                .as_ref()
                .is_some_and(|palette| palette.is_presented())
    }

    /// The refresh of a tree nothing changed under: an empty diff.
    ///
    /// Debug builds prove it by walking the widgets anyway. A walk that
    /// finds a change the tracking missed fails a test outright, and is
    /// published (and logged) in any other debug build, so a missed change
    /// is never stale where it can be seen.
    fn unchanged_semantic_tree(&mut self) -> crate::a11y_tree::TreeDelta {
        if !cfg!(debug_assertions) {
            return crate::a11y_tree::TreeDelta::default();
        }
        let delta = self.rebuild_semantic_tree();
        assert!(
            delta.is_empty() || !cfg!(any(test, feature = "test-support")),
            "the semantic tree changed with nothing marking it stale: {delta:?}"
        );
        if !delta.is_empty() {
            tracing::error!(
                ?delta,
                "the semantic tree changed with nothing marking it stale"
            );
        }
        delta
    }

    /// Walk the widgets and diff what they declare against the tree.
    fn rebuild_semantic_tree(&mut self) -> crate::a11y_tree::TreeDelta {
        let _entry = self.enter_wake_scope();
        // Built from what is there now; a presentation the tracking does
        // not follow leaves it stale, so leaving it walks again.
        self.accessibility.semantic_tree_stale = self.semantic_tree_untracked();
        if let Some(palette) = self
            .actions
            .action_palette
            .as_ref()
            .filter(|p| p.is_presented())
        {
            return self.accessibility.semantic_tree.refresh_bound(
                palette,
                &self.layout.node_map,
                &self.bindings.semantic_node_names(&self.layout.node_map),
                self.focused_node,
                &[],
            );
        }
        let mut resources = self.collect_declared_resources();
        resources.extend(self.history_local_resources());
        let components = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        )
        .unwrap_or_default();
        let mut tree = std::mem::take(&mut self.accessibility.semantic_tree);
        let delta = self
            .with_native_presentation(|root, nodes, focused, _, names| {
                tree.refresh_components(root, nodes, names, focused, &resources, &components)
            })
            .unwrap_or_else(|_| {
                tree.refresh_components(
                    self.root_widget.as_ref(),
                    &self.layout.node_map,
                    &self.bindings.semantic_node_names(&self.layout.node_map),
                    self.focused_node,
                    &self.collect_declared_resources(),
                    &components,
                )
            });
        self.accessibility.semantic_tree = tree;
        delta
    }
    pub fn semantic_tree(&self) -> &crate::a11y_tree::SemanticTree {
        &self.accessibility.semantic_tree
    }

    fn invoke_bound_semantic_action(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        arg: Option<&str>,
    ) -> Result<bool, String> {
        let actor = self.collaboration.local_author.0.clone();
        let invocation = ResourceInvocation::new(&target.address.address, &actor).argument(arg);
        match self.invoke_bound_target(target, &invocation) {
            Ok(_) => Ok(true),
            Err(tilo_actions::DoorError::ConfirmationRequired) => {
                if let Some(value) = arg {
                    let arguments = if let Some(json) =
                        value.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                    {
                        crate::bound_dispatch::ControlContinuationArguments::Json(
                            serde_json::from_str(json).map_err(|error| error.to_string())?,
                        )
                    } else {
                        crate::bound_dispatch::ControlContinuationArguments::Text(value.to_owned())
                    };
                    return Ok(self.open_ordinary_confirmation(
                        crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(
                            target.clone(),
                        ),
                        target.address.address.clone(),
                        arguments,
                    ));
                }
                self.open_action_palette();
                let activated = self
                    .actions
                    .action_palette
                    .as_mut()
                    .is_some_and(|palette| palette.activate_bound(target));
                self.capture_ordinary_palette_request();
                Ok(activated)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// A [`Verb`](crate::a11y_tree::SemanticActionKind::Verb) routes through
    /// the same [`dispatch_a11y_action`](Self::dispatch_a11y_action) path an
    /// AccessKit `ActionRequest` takes; an
    /// [`App`](crate::a11y_tree::SemanticActionKind::App) affordance
    /// dispatches through [`tilo_actions::dispatch`], the same audited path
    /// the Ctrl+K palette uses. Call
    /// [`refresh_semantic_tree`](Self::refresh_semantic_tree) first so the
    /// ids are current.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the underlying app action fails to decode or
    /// dispatch (see `Self::dispatch_encoded_semantic`).
    pub fn invoke_semantic_action(
        &mut self,
        id: &crate::a11y_tree::SemanticId,
        name: &str,
        arg: Option<&str>,
    ) -> Result<bool, String> {
        use crate::a11y_tree::SemanticActionKind;

        let _entry = self.enter_wake_scope();
        // Cloned so the mutation below is free to borrow doc/registry/transport.
        let Some(action) = self.accessibility.semantic_tree.action(id, name).cloned() else {
            return Ok(false);
        };
        if let Some(target) = self.accessibility.semantic_tree.bound_target(id, name) {
            return self.invoke_bound_semantic_action(&target, arg);
        }
        match action.kind {
            SemanticActionKind::App(encoded) => {
                if self.is_history_encoded_local(&encoded) {
                    self.execute_history_encoded_local(&encoded)?;
                    Ok(true)
                } else {
                    self.dispatch_encoded_semantic(encoded)
                }
            }
            SemanticActionKind::AppDeferred { address } => {
                if arg.is_some() {
                    return Err("Deferred action accepts no argument".into());
                }
                let actor = self.collaboration.local_author.0.clone();
                match self.invoke_resource_action(ResourceInvocation::new(&address, &actor)) {
                    Ok(_) => {
                        self.needs_sync = true;
                        self.layout.layout_dirty = true;
                        Ok(true)
                    }
                    Err(tilo_actions::DoorError::ConfirmationRequired) => {
                        Ok(self.open_action_palette_at(&address))
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            SemanticActionKind::AppPrompted { .. } => {
                let arg = arg.unwrap_or("");
                let Some(encoded) = self
                    .accessibility
                    .semantic_tree
                    .build_prompted(id, name, arg)
                else {
                    return Ok(false);
                };
                if self.is_history_encoded_local(&encoded) {
                    self.execute_history_encoded_local(&encoded)?;
                    return Ok(true);
                }
                self.dispatch_encoded_semantic(encoded)
            }
            SemanticActionKind::Verb(verb) => {
                let verb = match verb {
                    crate::a11y::Action::SetValue(_) => {
                        crate::a11y::Action::SetValue(arg.unwrap_or_default().to_string())
                    }
                    other => other,
                };
                let Some((taffy_raw, semantic_target, bounds)) =
                    self.accessibility.semantic_tree.node(id).and_then(|n| {
                        n.widget_taffy_id
                            .map(|taffy| (taffy, n.semantic_target.clone(), n.bounds))
                    })
                else {
                    return Ok(false);
                };
                if let Some(semantic_target) = semantic_target {
                    if semantic_target.starts_with("ordinary-confirmation:") {
                        self.bindings.clear();
                        self.layout.node_map.clear();
                        self.focused_node = None;
                        let handled = self.actions.action_palette.as_mut().is_some_and(|palette| {
                            palette.perform_semantic_action(
                                &semantic_target,
                                &verb,
                                &mut crate::widget::Edits::lend(&mut self.state.doc),
                                &self.state.root,
                            )
                        });
                        self.finish_action_palette_input(handled);
                        return Ok(handled);
                    }
                    // Semantic owners use the same lifetime-scoped aliases as
                    // native accessibility. Resolve only current admissions;
                    // an expired alias must never fall back to a raw layout id.
                    let exported = self.bindings.exported_nodes(&self.layout.node_map);
                    let Some(crate::accesskit_bridge::A11yTarget::Widget(taffy_id)) = exported
                        .resolve(
                            self.root_widget.as_ref(),
                            accesskit::NodeId(taffy_raw.saturating_add(1)),
                        )
                    else {
                        return Ok(false);
                    };
                    let keyed_path = semantic_target
                        .strip_prefix("tilo.key-action:")
                        .and_then(|target| target.rsplit_once(':'))
                        .map(|(_, path)| {
                            path.split('/')
                                .map(str::parse::<u64>)
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .transpose()
                        .map_err(|error| format!("invalid keyed semantic action: {error}"))?;
                    let handled = self
                        .dispatch_node(taffy_id, |widget, doc, root| {
                            if let Some(key_path) = keyed_path {
                                widget.perform_action(
                                    &verb,
                                    &key_path,
                                    &mut crate::widget::Edits::lend(doc),
                                    root,
                                )
                            } else {
                                widget.perform_semantic_action(
                                    &semantic_target,
                                    &verb,
                                    &mut crate::widget::Edits::lend(doc),
                                    root,
                                )
                            }
                        })
                        .map_err(|error| format!("component dispatch refused: {error:?}"))?;
                    if handled {
                        if matches!(verb, crate::a11y::Action::Focus) {
                            self.set_focused_node(Some(taffy_id));
                        }
                        self.invalidate_widget_resource_cache();
                        self.needs_sync = true;
                        self.layout.layout_dirty = true;
                        return Ok(true);
                    }
                    // A custom-painted widget may expose stable semantic
                    // targets and geometry while intentionally reusing its
                    // production pointer hit-testing instead of duplicating a
                    // `perform_semantic_action` switch. Route the action back
                    // through that exact event path.
                    if let Some(bounds) = bounds {
                        let center = bounds.center();
                        return Ok(self.dispatch_semantic_click(center.x, center.y));
                    }
                    return Ok(false);
                }
                let ak = accesskit::NodeId(taffy_raw.saturating_add(1));
                if let Some((x, y)) = self.dispatch_a11y_action(ak, &verb) {
                    return Ok(self.dispatch_semantic_click(x, y));
                }
                Ok(true)
            }
        }
    }

    /// Byte-for-byte the same path as a Ctrl+K palette selection
    /// ([`Self::action_palette_handle_key`]).
    fn dispatch_encoded_semantic(
        &mut self,
        encoded: tilo_actions::EncodedAction,
    ) -> Result<bool, String> {
        let before = self.state.doc.get_heads();
        let action = self
            .actions
            .action_registry
            .decode(&encoded)
            .map_err(|e| e.to_string())?;
        let dispatched = tilo_actions::dispatch(
            &mut self.state.doc,
            action.as_ref(),
            encoded,
            self.actions.action_transport.as_mut(),
        )
        .map_err(|e| e.to_string())?;
        if dispatched.mutated_local() {
            self.state.record_committed_action(before, None, None);
            self.needs_sync = true;
            self.layout.layout_dirty = true;
            self.invalidate_widget_resource_cache();
        }
        Ok(true)
    }
}

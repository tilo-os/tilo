// Accessibility configuration, semantic focus, announcements, and action routing.

impl App {
    fn screen_reader_env_enabled() -> bool {
        for key in ["TILO_A11Y_SCREEN_READER", "TILO_SCREEN_READER"] {
            if let Ok(v) = std::env::var(key) {
                let v = v.trim();
                if !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false") {
                    return true;
                }
            }
        }
        false
    }

    fn persisted_accessibility_bool(key: &str) -> bool {
        settings_store::load_section("accessibility")
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    fn configure_screen_reader(reader: &mut crate::screen_reader::ScreenReader) {
        let section = settings_store::load_section("accessibility");
        let rate_pct = section
            .get("screen_reader_rate")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(100)
            .clamp(50, 200);
        // Always in 50..=200, so this always fits u8 losslessly.
        let rate = f32::from(u8::try_from(rate_pct).unwrap_or(100)) / 100.0;
        reader.set_rate(rate);
        let voice = section
            .get("screen_reader_voice")
            .and_then(serde_json::Value::as_str)
            .and_then(|voice| match voice {
                "english_us" => Some("en-us"),
                "english_gb" => Some("en-gb"),
                "german" => Some("de"),
                "french" => Some("fr"),
                "spanish" => Some("es"),
                _ => None,
            });
        if let Some(voice) = voice {
            reader.set_voice(voice);
        }
    }

    fn configured_screen_reader() -> crate::screen_reader::ScreenReader {
        let mut reader = crate::screen_reader::ScreenReader::new();
        Self::configure_screen_reader(&mut reader);
        reader
    }

    /// Shared truthy-env-var gate for the low-vision flags: non-empty, not
    /// `"0"`/`"false"` (case-insensitive).
    fn bool_env(keys: &[&str]) -> bool {
        for key in keys {
            if let Ok(v) = std::env::var(key) {
                let v = v.trim();
                if !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false") {
                    return true;
                }
            }
        }
        false
    }

    fn high_contrast_env_enabled() -> bool {
        Self::bool_env(&["TILO_A11Y_HIGH_CONTRAST", "TILO_HIGH_CONTRAST"])
    }

    fn reduce_motion_env_enabled() -> bool {
        Self::bool_env(&["TILO_A11Y_REDUCE_MOTION", "TILO_REDUCE_MOTION"])
    }

    /// A bad value is ignored rather than crashing the app at startup.
    fn text_scale_env() -> Option<f32> {
        for key in ["TILO_A11Y_TEXT_SCALE", "TILO_TEXT_SCALE"] {
            if let Ok(v) = std::env::var(key)
                && let Ok(f) = v.trim().parse::<f32>()
                && f.is_finite()
                && f > 0.0
            {
                return Some(f);
            }
        }
        None
    }

    /// The shell mirrors the user's UI-scale preference into spawned
    /// sandboxes via this var so an app comes up already scaled.
    fn ui_scale_env() -> Option<f32> {
        for key in ["TILO_UI_SCALE", "TILO_A11Y_UI_SCALE"] {
            if let Ok(v) = std::env::var(key)
                && let Ok(f) = v.trim().parse::<f32>()
                && f.is_finite()
                && f > 0.0
            {
                return Some(f);
            }
        }
        None
    }

    /// The screen-reader flag tracks [`Self::screen_reader_enabled`].
    pub fn a11y_settings(&self) -> crate::a11y::A11ySettings {
        let mut s = self.accessibility.a11y;
        s.screen_reader = self.accessibility.screen_reader.is_some();
        s
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
        self.layout.structure_dirty = true;
    }

    pub fn ui_scale(&self) -> f32 {
        self.accessibility.ui_scale
    }

    pub fn set_ui_scale(&mut self, factor: f32) {
        let _entry = self.enter_wake_scope();
        self.accessibility.ui_scale = factor;
        self.rebuild_theme();
        self.layout.layout_dirty = true;
        self.layout.structure_dirty = true;
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
        self.layout.structure_dirty = true;
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
        self.layout.structure_dirty = true;
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

        if let Key::Named(NamedKey::Insert) = ke.logical_key {
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
        fn collect_focusable(
            node_id: NodeId,
            widget: &dyn Widget,
            taffy: &TaffyTree<()>,
            order: &mut Vec<NodeId>,
        ) {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
                return;
            };
            if widget.is_focusable() && widget.is_enabled() {
                order.push(node_id);
            }
            let children = widget.children();
            if let Ok(child_nodes) = taffy.children(node_id) {
                for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
                    collect_focusable(child_node, *child_widget, taffy, order);
                }
            }
        }
        // The deepest trapping widget wins (a modal opened from within one).
        fn find_trap_focusables(
            node_id: NodeId,
            widget: &dyn Widget,
            taffy: &TaffyTree<()>,
        ) -> Option<Vec<NodeId>> {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
                return None;
            };
            let children = widget.children();
            let child_nodes = taffy.children(node_id).ok();
            // Recurse first so a nested trap takes precedence.
            if let Some(child_nodes) = &child_nodes {
                for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
                    if let Some(inner) = find_trap_focusables(child_node, *child_widget, taffy) {
                        return Some(inner);
                    }
                }
            }
            if widget.traps_focus() {
                let mut order = Vec::new();
                collect_focusable(node_id, widget, taffy, &mut order);
                return Some(order);
            }
            None
        }

        self.focusable_order.clear();
        let Some(root) = self.layout.root_node else {
            return;
        };

        let (trap_active, order) = self
            .with_native_presentation(|widget, _, _, taffy| {
                let trap = find_trap_focusables(root, widget, taffy);
                if let Some(order) = trap {
                    (true, order)
                } else {
                    let mut order = Vec::new();
                    collect_focusable(root, widget, taffy, &mut order);
                    (false, order)
                }
            })
            .unwrap_or_default();
        self.focusable_order = order;
        match (self.accessibility.focus_trap_active, trap_active) {
            (false, true) => {
                // Modal just opened: remember prior focus, move into the trap.
                self.accessibility.focus_before_trap = self.focused_node;
                if let Some(&first) = self.focusable_order.first() {
                    self.set_focused_node(Some(first));
                } else {
                    // No focusable content — drop focus so Tab can't reach
                    // the content behind the backdrop.
                    self.set_focused_node(None);
                }
            }
            (true, false) => {
                // Modal closed: restore only if that element still exists.
                let restore = self
                    .accessibility
                    .focus_before_trap
                    .take()
                    .filter(|id| self.focusable_order.contains(id));
                self.set_focused_node(restore);
            }
            (true, true) => {
                // Still trapped: pull focus back if a rebuild dropped it
                // outside the modal.
                let inside = self
                    .focused_node
                    .is_some_and(|id| self.focusable_order.contains(&id));
                if !inside {
                    self.set_focused_node(self.focusable_order.first().copied());
                }
            }
            (false, false) => {}
        }
        self.accessibility.focus_trap_active = trap_active;
    }

    /// The decode lives on [`crate::presence::PresenceCtx`] so the
    /// compositor reads presence the same way an app does; what stays here
    /// is what only a document holder can do (follow a peer's viewport,
    /// answer a late joiner with full state).
    fn apply_remote_presence(
        &mut self,
        user_id: String,
        key: crate::presence::PresenceKey,
        mut payload: crate::collaboration::PresencePayload,
    ) {
        use crate::presence::RemotePresenceEffect;
        // Domain actions arrive without UI coordinates by design. If this
        // frame exposes the same address through one or more controls, bind
        // the interaction to the preferred live semantic route before the
        // monotonic presence store sees it. Every later phase resolves by the
        // same address, so identity remains stable while menus/toolbars move.
        if let crate::collaboration::PresencePayload::Interaction(update) = &mut payload
            && matches!(&update.anchor, tilo_actions::InteractionAnchor::Resource)
        {
            let address = tilo_actions::ActionAddress::new(
                update.resource.id.clone(),
                update.action.id.clone(),
            );
            if let Some(route) = self.preferred_action_ui_route(&address)
                && let Some(target) = route.path.last()
            {
                update.anchor = tilo_actions::InteractionAnchor::SemanticControl {
                    id: target.as_str().to_string(),
                };
            }
        }
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

    fn apply_undo_entry(&mut self, entry: crate::state::UndoEntry) {
        if let Some(idx) = entry.focused_node_idx {
            if let Some(&id) = self.focusable_order.get(idx) {
                self.set_focused_node(Some(id));
                if let Some(metadata) = entry.widget_metadata {
                    let _ = self.mutate_node(id, |widget| widget.set_state(&metadata));
                    self.history.stable_metadata = Some(metadata);
                }
                self.history.stable_focus_idx = Some(idx);
            }
        } else {
            self.set_focused_node(None);
            self.history.stable_focus_idx = None;
            self.history.stable_metadata = None;
        }
        self.layout.layout_dirty = true;
        self.needs_sync = true;
        self.invalidate_widget_resource_cache();
    }

    /// Unique admitted node visits keep child reports and requests off shell history.
    fn service_history(&mut self) -> bool {
        // SAFETY: App exclusively owns this stable live layout for the sweep.
        let mut changed = unsafe {
            crate::undo::record_dispatched_action_bound(
                &mut self.state,
                std::ptr::from_mut(self.root_widget.as_mut()),
                &self.layout.node_map,
                &self.bindings,
                &self.collaboration.local_author,
            )
        };
        let mut visited = std::collections::HashSet::new();
        for (&node, pointer) in &self.layout.node_map {
            if !visited.insert(pointer.cast::<()>()) {
                continue;
            }
            let binding = self.bindings.binding_for(node);
            // SAFETY: no target reference escapes and this layout is unchanged.
            let request = unsafe {
                crate::bound_dispatch::mutate_bound_node(
                    &self.bindings,
                    &self.layout.node_map,
                    node,
                    |widget| widget.take_undo_request(),
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
                } else {
                    match request {
                        crate::undo::UndoRequest::Undo => {
                            self.state
                                .undo_as(Some(&self.collaboration.local_author), None, None)
                        }
                        crate::undo::UndoRequest::Redo => {
                            self.state
                                .redo_as(Some(&self.collaboration.local_author), None, None)
                        }
                    }
                };
                changed |= entry.is_some();
            }
            let status = if let Some(binding) = binding {
                binding.undo_status(&mut self.state.doc).unwrap_or_default()
            } else {
                self.state
                    .undo_status(Some(&self.collaboration.local_author))
            };
            // SAFETY: the same unique admitted node remains live and exclusive.
            let _ = unsafe {
                crate::bound_dispatch::mutate_bound_node(
                    &self.bindings,
                    &self.layout.node_map,
                    node,
                    |widget| widget.set_undo_status(&status),
                )
            };
        }
        changed
    }

    /// Finalize the document boundary for a handled native event. Shared host
    /// controls only change presentation and must not close pending app typing.
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
        if self.service_history() {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        // Called after every handled event, many of which (e.g. a
        // selection-drag `CursorMoved`) mutate only ephemeral widget state,
        // not the CRDT — `changed` lets those skip persisting/syncing.
        let context = self.root_widget.take_commit_context();
        let changed = self.state.commit_action_authored_with_context(
            self.history.stable_focus_idx,
            self.history.stable_metadata.clone(),
            Some(self.collaboration.local_author.clone()),
            context,
        );
        self.history.stable_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.history.stable_metadata = self.focused_node.and_then(|id| {
            self.inspect_node(id, |widget| widget.get_state())
                .ok()
                .flatten()
        });
        self.invalidate_widget_resource_cache();

        // Incremental delta, not the whole document — serialising per
        // keystroke scales with document size. `UndoLog::apply_inverse`
        // writes the inverse as new changes, so a delta carries undo/redo
        // like any other edit.
        if changed {
            self.broadcast_doc_delta();
        }
    }

    fn set_focused_node(&mut self, new_id: Option<NodeId>) {
        if new_id.is_some_and(|node| self.is_history_node(node)) {
            self.remember_history_content();
        }
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
    fn ime_window_state(&self) -> Option<ImeWindowState> {
        let id = self.focused_node?;
        let purpose = self
            .inspect_node(id, |widget| widget.ime_purpose())
            .ok()
            .flatten()?;
        let rect = self
            .inspect_node(id, |widget| widget.ime_cursor_rect())
            .ok()
            .flatten()?;
        let bounded = |v: f64| {
            if v.is_finite() {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "value is clamped to [0.0, u32::MAX] on the line above, so the cast is exact"
                )]
                {
                    v.max(0.0).min(f64::from(u32::MAX)).round() as u32
                }
            } else {
                0
            }
        };
        Some(ImeWindowState {
            purpose,
            x: bounded(rect.x0),
            y: bounded(rect.y0),
            width: bounded(rect.width()).max(1),
            height: bounded(rect.height()).max(1),
        })
    }

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
        let current_center = current_rect.center();
        let mut best_candidate = None;
        let mut best_score = f64::MAX;
        for &candidate_id in &self.focusable_order {
            if candidate_id == current_id {
                continue;
            }
            if let Ok(candidate_rect) = self.inspect_node(candidate_id, |widget| widget.get_rect())
            {
                let candidate_center = candidate_rect.center();
                let vector = vello::kurbo::Vec2::new(
                    candidate_center.x - current_center.x,
                    candidate_center.y - current_center.y,
                );
                let (primary_dist, secondary_dist, in_direction) = match direction {
                    Direction::Up => (-vector.y, vector.x.abs(), vector.y < -1.0),
                    Direction::Down => (vector.y, vector.x.abs(), vector.y > 1.0),
                    Direction::Left => (-vector.x, vector.y.abs(), vector.x < -1.0),
                    Direction::Right => (vector.x, vector.y.abs(), vector.x > 1.0),
                };
                if in_direction {
                    let score = primary_dist + (secondary_dist * 2.0);
                    if score < best_score {
                        best_score = score;
                        best_candidate = Some(candidate_id);
                    }
                }
            }
        }
        if let Some(new_id) = best_candidate {
            self.set_focused_node(Some(new_id));
        }
    }

    /// Gated on the focus-trap signal, not `modal_capture`: that governs only
    /// pointer click-through — gating Tab on it instead either froze focus
    /// inside a dialog or let it escape with no capture.
    fn keyboard_nav_allowed(&self, modal_rect: Option<vello::kurbo::Rect>) -> bool {
        modal_rect.is_none() || self.accessibility.focus_trap_active
    }

    /// Tab normally belongs to the host's focus order. A focused editor-like
    /// widget may explicitly claim it first (for indentation or completion),
    /// without broadening that exception to arbitrary widgets or escaping an
    /// active focus trap.
    fn focused_widget_wants_tab_input(&self) -> bool {
        self.focused_node.is_some_and(|node| {
            self.inspect_node(node, |widget| widget.wants_tab_input())
                .unwrap_or(false)
        })
    }

    /// Shared native Tab admission, preserving text-input and modal capture.
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

    /// Native Enter/Space fallback, after the focused widget has declined input.
    fn activate_focused_control_key(&mut self, event: &WindowEvent) -> bool {
        if !matches!(event, WindowEvent::KeyboardInput { event, .. }
            if event.state == winit::event::ElementState::Pressed
                && matches!(event.logical_key, winit::keyboard::Key::Named(
                    winit::keyboard::NamedKey::Enter | winit::keyboard::NamedKey::Space)))
        {
            return false;
        }
        let Some(fid) = self.focused_node else {
            return false;
        };
        let did = self
            .dispatch_node(fid, |widget, doc, root| {
                widget.perform_action(&crate::a11y::Action::Activate, doc, root)
            })
            .unwrap_or(false);
        if did {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        did
    }

    fn focus_tab(&mut self, shift: bool) -> bool {
        let len = self.focusable_order.len();
        if len == 0 {
            return false;
        }
        let current_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        let next_idx = match current_idx {
            Some(idx) => {
                if shift {
                    if idx == 0 {
                        len.saturating_sub(1)
                    } else {
                        idx.saturating_sub(1)
                    }
                } else {
                    let next = idx.saturating_add(1);
                    if next >= len { 0 } else { next }
                }
            }
            None => 0,
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
        let mut resources = self.collect_declared_resources();
        resources.extend(self.history_local_resources());
        self.with_native_presentation(|root, nodes, focused, _| {
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
        if self
            .invoke_history_address_local(&address, argument)
            .is_some()
        {
            return true;
        }
        let confirmation = tilo_actions::door::resolve(&resources, &address)
            .map(|spec| spec.contract.confirmation.clone())
            .unwrap_or_default();
        if !matches!(confirmation, tilo_actions::ConfirmationPolicy::None) {
            let _ = self.open_action_palette_at(&address);
            return true;
        }
        let actor = crate::collaboration::local_user_id().0;
        if self
            .invoke_resource_action(&address, argument, &actor, false, false, None, None, None)
            .is_ok()
        {
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
        true
    }

    /// Resolves both retained AccessKit nodes and synthetic semantic children
    /// (virtual rows/cells) to their owning widget. The widget gets first
    /// refusal; synthetic children additionally carry an opaque stable target
    /// produced by that widget. If a retained widget declines, `Action::Focus`
    /// moves keyboard focus and activation returns click coordinates.
    pub fn dispatch_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Option<(f64, f64)> {
        use crate::a11y::Action;
        use crate::accesskit_bridge::{A11yTarget, resolve_node_id_bound};

        let _entry = self.enter_wake_scope();

        self.refresh_control_action_availability();
        if self.dispatch_resource_a11y_action(target, action) {
            return None;
        }

        let resolved = self
            .with_native_presentation(|root, nodes, _, _| {
                resolve_node_id_bound(root, nodes, target)
            })
            .ok()
            .flatten()?;
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
        let handled = self
            .dispatch_node(taffy_id, |widget, doc, root| {
                if let Some(target) = semantic_target.as_deref() {
                    widget.perform_semantic_action(target, action, doc, root)
                } else if synthetic {
                    widget.perform_action_at(action, &key_path, doc, root)
                } else {
                    widget.perform_action(action, doc, root)
                }
            })
            .ok()?;
        if handled {
            if matches!(action, Action::Focus) {
                self.set_focused_node(Some(taffy_id));
            }
            // `Widget::perform_action` is deliberately the same path as a
            // keyboard or pointer activation.  Action-bound controls queue a
            // control intent there; drain it before publishing the next
            // AccessKit update so an AT-SPI DoAction never reports success
            // while leaving the visible state unchanged.
            self.dispatch_control_invocations();
            self.invalidate_widget_resource_cache();
            self.needs_sync = true;
            self.layout.layout_dirty = true;
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
        self.refresh_control_action_availability();
        if self.layout.root_node.is_none() {
            return false;
        }
        let events = [
            WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: PhysicalPosition::new(x, y),
            },
            WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        ];
        let mut handled_any = false;
        for event in events {
            let mut handled = false;
            let mut focus_target = None;
            self.presentation_handle_event(&event, 1.0, &mut handled, &mut focus_target);
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
    pub fn refresh_semantic_tree(&mut self) -> crate::a11y_tree::TreeDelta {
        let _entry = self.enter_wake_scope();
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
            .with_native_presentation(|root, nodes, focused, _| {
                tree.refresh_components(root, nodes, focused, &resources, &components)
            })
            .unwrap_or_else(|_| {
                tree.refresh_components(
                    self.root_widget.as_ref(),
                    &self.layout.node_map,
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
        match self.invoke_bound_target(target, arg, &actor, false, false, None, None, None) {
            Ok(_) => Ok(true),
            Err(tilo_actions::DoorError::ConfirmationRequired) => {
                self.open_action_palette();
                Ok(self
                    .actions
                    .action_palette
                    .as_mut()
                    .is_some_and(|palette| palette.activate_bound(target)))
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
    /// dispatch (see [`Self::dispatch_encoded_semantic`]).
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
                    let taffy_id = NodeId::from(taffy_raw);
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
                                widget.perform_action_at(&verb, &key_path, doc, root)
                            } else {
                                widget.perform_semantic_action(&semantic_target, &verb, doc, root)
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

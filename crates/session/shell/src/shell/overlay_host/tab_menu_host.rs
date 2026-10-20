//! The tab context menu as the host opens, routes and acts on it.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

/// The tab right-click menu: request harvest, routing, actions, semantics
/// and paint. Everything a person, a script or a reader does with the
/// menu passes through here, so the three routes cannot disagree.
impl ShellOverlayHost {
    /// Drains every tab bar's right-click request
    /// ([`UiWindowGroup::take_tab_context_menu`]) and opens the menu for
    /// it. Run synchronously inside the pointer dispatch that produced the
    /// request, so the menu is up — and in the accessibility tree — when
    /// that event returns; `update` runs it again as a fallback.
    pub(super) fn harvest_pending_tab_context_menus(&mut self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        // Every bar is drained (a request left behind would open a menu on
        // some later, unrelated event). One pointer press reaches one bar,
        // so more than one request means a stale one survived; the request
        // at the pointer's current position is the one just made, and the
        // rest are dropped — deterministic, not HashMap order.
        let mut requests: Vec<(TabMenuTarget, usize, Point, String)> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for g in &mut row.groups {
                if let Some((tab_id, point)) = g.take_tab_context_menu() {
                    let title = g
                        .tabs()
                        .iter()
                        .find(|t| t.id == tab_id)
                        .map(|t| t.title.clone())
                        .unwrap_or_default();
                    let target = TabMenuTarget {
                        strip: strip_id,
                        group: g.id,
                        tab: tab_id,
                    };
                    requests.push((target, g.tab_count(), point, title));
                }
            }
        }
        let cursor = self.last_cursor;
        let nearest = requests.into_iter().min_by(|a, b| {
            let da = cursor.map_or(0.0, |c| c.distance(a.2));
            let db = cursor.map_or(0.0, |c| c.distance(b.2));
            da.total_cmp(&db)
                .then_with(|| a.0.strip.raw().cmp(&b.0.strip.raw()))
                .then_with(|| a.0.group.cmp(&b.0.group))
        });
        let Some((target, tab_count, point, title)) = nearest else {
            return;
        };
        self.layers.tab_menu.open(
            target,
            tab_count,
            point,
            title,
            self.surface_area(Some(point)),
        );
    }

    /// Open the shell menu at the focused tab, independent of the pointer.
    /// Resolve the durable focus first; a stale or unlaid-out tab cannot open
    /// a menu at a made-up location. The menu is local presentation only.
    pub fn open_focused_tab_context_menu(&mut self) -> bool {
        if self.onboarding.is_some() || self.content.wants_pointer_capture() {
            return false;
        }
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let target = {
            let st = chrome.borrow();
            if st.session_locked() {
                return false;
            }
            let tree = st.tree();
            let Some(strip) = tree.desktop.active_strip.get(&tree.desktop.active).copied() else {
                return false;
            };
            let Some(group) = tree.desktop.active_group_for(strip) else {
                return false;
            };
            drop(tree);
            let Some(tab) = crate::shell::window_actions::active_tab_id(&st) else {
                return false;
            };
            drop(st);
            TabMenuTarget { strip, group, tab }
        };
        let Some(&ptr) = self.group_row_ptrs.get(&target.strip) else {
            return false;
        };
        // SAFETY: see field doc on `group_row_ptrs`; only read the live row.
        let row: &GroupRow = unsafe { &*ptr };
        let Some(group) = row.groups.iter().find(|group| group.id == target.group) else {
            return false;
        };
        let Some((index, tab)) = group
            .tabs()
            .iter()
            .enumerate()
            .find(|(_, tab)| tab.id == target.tab)
        else {
            return false;
        };
        let Some(rect) = group.tab_rects().get(index).copied() else {
            return false;
        };
        // Compact bars may collapse an inactive pill, and keyboard focus can
        // change before the next layout expands it. Its laid-out bar remains
        // a real anchor; do not discard that opening key between frames.
        let rect = if rect.area() > 0.0 {
            rect
        } else {
            group.bar_rect()
        };
        if rect.area() <= 0.0 {
            return false;
        }
        let title = tab.title.clone();
        let tab_count = group.tab_count();
        let at = Point::new(rect.x0, rect.y1);
        let display = self.surface_area(Some(at));
        self.layers
            .tab_menu
            .open(target, tab_count, at, title, display);
        self.layers.tab_menu.focus_first_action();
        true
    }

    /// Abandon tab gestures and cross-strip feedback, preserving durable state.
    pub fn cancel_tab_drags(&mut self) -> bool {
        let mut cancelled = false;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: live rows are visited sequentially under the field's
            // stable-pointer contract, without retaining a borrowed row.
            cancelled |= unsafe { &mut *ptr }.cancel_tab_drag();
        }
        if cancelled {
            for &ptr in self.group_row_ptrs.values() {
                // SAFETY: the same sequential stable-row contract as above.
                unsafe { &mut *ptr }.set_cross_strip_tab_drag_hover(None);
            }
        }
        cancelled
    }

    /// A menu whose tab or group has gone has nothing left to act on.
    pub(super) fn prune_stale_tab_context_menu(&mut self) {
        let rows = &self.group_row_ptrs;
        self.layers.tab_menu.prune(|target| {
            rows.get(&target.strip).is_some_and(|&ptr| {
                // SAFETY: see field doc on `group_row_ptrs`.
                let row: &GroupRow = unsafe { &*ptr };
                row.groups
                    .iter()
                    .any(|g| g.id == target.group && g.tabs().iter().any(|t| t.id == target.tab))
            })
        });
    }

    /// Run one tab-menu action against its tab; `true` when it was
    /// performed. Shared by the pointer and keyboard selection and by the
    /// accessibility route, so every door reaches the same operation — and
    /// the same operation the `tab.*` actions in the registry run
    /// (`window_actions::close_tab` / `split_tab`), so a script and a
    /// right-click cannot disagree.
    pub(super) fn perform_tab_menu_action(
        &mut self,
        target: TabMenuTarget,
        action: TabMenuAction,
    ) -> bool {
        match action {
            // The `✕` and the keyboard chord's own durable lifecycle: tree
            // and document first, then the widget's close animation.
            TabMenuAction::CloseTab => self.close_tab_in_group(target.group, Some(target.tab)),
            TabMenuAction::SplitRight | TabMenuAction::MoveToNewGroup => {
                let Some(chrome) = self.chrome.clone() else {
                    return false;
                };
                let placement = match action {
                    TabMenuAction::SplitRight => {
                        crate::shell::window_actions::SplitPlacement::Right
                    }
                    _ => crate::shell::window_actions::SplitPlacement::StripEnd,
                };
                // Tree first: cache + replicated document + chrome bump, so
                // the split is durable before anything moves on screen and
                // an unrelated revision between here and the next frame
                // cannot evict it.
                let new_group = {
                    let mut st = chrome.borrow_mut();
                    match crate::shell::window_actions::split_tab(&mut st, target.tab, placement) {
                        Ok(id) => id,
                        Err(err) => {
                            st.push_warning_notification(
                                t("host-tab-move-failed"),
                                err.user_message(),
                            );

                            return false;
                        }
                    }
                };
                // Then move the live tab host into a group carrying the
                // tree's id, so the resync the bump triggers finds the group
                // already present and keeps the person's window state
                // (terminal scrollback, open document) instead of building a
                // fresh host. If the host cannot be moved the resync builds
                // one — correct, just colder.
                if let Some(&ptr) = self.group_row_ptrs.get(&target.strip) {
                    // SAFETY: see field doc on `group_row_ptrs`.
                    let row: &mut GroupRow = unsafe { &mut *ptr };
                    let src = row.groups.iter().position(|g| g.id == target.group);
                    let detached = src
                        .and_then(|i| row.groups.get_mut(i))
                        .and_then(|g| g.detach_tab(target.tab));
                    if let (Some(src), Some((desc, host))) = (src, detached) {
                        let at = match action {
                            TabMenuAction::SplitRight => src.saturating_add(1),
                            _ => row.groups.len(),
                        };
                        let mut group = UiWindowGroup::from_detached(
                            new_group,
                            desc,
                            host,
                            ui::widget::WindowWidth::Normal,
                        );
                        group.start_spawn_animation();
                        row.insert_group(group, at);
                    }
                }
                true
            }
        }
    }

    /// Route `event` through the stacked layers in `band` and carry out
    /// what they asked for before the event finishes dispatching.
    pub(super) fn route_layer_band(
        &mut self,
        band: Band,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.layers.listens(band) {
            return false;
        }
        let shortcut = match event {
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                self.configured_shortcut_action(&key.logical_key, *ctx.modifiers)
            }
            _ => None,
        };
        let outputs = self.layer_outputs();
        let mut cx = LayerCx::new(self.layer_geometry(&outputs), self.last_cursor, ctx)
            .with_shortcut(shortcut);
        let consumed = self.layers.route(band, event, &mut cx);
        for intent in cx.into_intents() {
            self.apply_layer_intent(intent);
        }
        consumed
    }

    /// Carry out one [`LayerIntent`]; `true` when it was performed.
    pub(super) fn apply_layer_intent(&mut self, intent: LayerIntent) -> bool {
        match intent {
            LayerIntent::TabMenu { target, action } => {
                let performed = self.perform_tab_menu_action(target, action);
                let at = self.layers.tab_menu.anchor_point();
                let display = self.surface_area(Some(at));
                self.layers
                    .tab_menu
                    .settle(target, action, performed, display);
                performed
            }
            LayerIntent::Session(action) => {
                self.layers.power_menu.close();
                self.execute_session_action(action);
                true
            }
            LayerIntent::FocusWindow(address) => self.focus_switcher_window(address),
            LayerIntent::Dictation(request) => {
                self.layers.dictation.enqueue(request);
                self.bump_chrome();
                true
            }
            LayerIntent::DictationSetup => {
                if !self.layers.dictation.is_model_setup() {
                    return false;
                }
                self.layers
                    .dictation
                    .status(crate::compositor::dictation_status::DictationStatus::Idle);
                self.layers.dictation.request_cancel();
                if let Some(chrome) = self.chrome.clone() {
                    self.open_settings(
                        &chrome,
                        Some("settings://accessibility#a11y.dictation_language"),
                    );
                    true
                } else {
                    false
                }
            }
            LayerIntent::Repaint => {
                self.bump_chrome();
                true
            }
        }
    }

    /// Whether the tab menu is up. For tests and the bench state snapshot.
    pub fn tab_context_menu_is_open(&self) -> bool {
        self.layers.tab_menu.is_open()
    }

    /// Painted rect of the open tab menu, `None` when closed.
    pub fn tab_context_menu_rect(&self) -> Option<Rect> {
        self.layers.tab_menu.rect()
    }
}

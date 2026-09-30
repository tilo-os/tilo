//! Retained browser workspace: each tab owns one live engine endpoint and its chrome.

use crate::BrowserWidget;
use crate::browser_tab_strip::BrowserTabStrip;
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::style_helpers::{auto, length};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::{
    AppKind, AppWidget, ContextMenu, Continuable, FloatingSurfaceProvider, FullscreenProvider,
    IconDescriptor, MenuItem, TabDescriptor, TaffyTreeExt, Widget, WindowCloseAdmission,
};
use ui::widget::{AppModel, ModelPath};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

const FULLSCREEN_HINT_DURATION: Duration = Duration::from_secs(3);

fn fullscreen_hint_visible(deadline: Option<Instant>, now: Instant) -> bool {
    deadline.is_some_and(|deadline| now < deadline)
}

struct Tab {
    id: u64,
    widget: Box<BrowserWidget>,
}
#[derive(Clone, Copy)]
enum TabMenuAction {
    Duplicate,
    TogglePin,
    ToggleMute,
    Close,
    CloseOthers,
    CloseRight,
    Reopen,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExitState {
    Normal,
    Choice,
    Cancelling,
    Ready,
}

struct RetainedEvent<'a> {
    event: &'a WindowEvent,
    scale: f64,
    modifiers: &'a winit::event::Modifiers,
    state: &'a mut AutoCommit,
    path: &'a ObjId,
    handled: &'a mut bool,
    focus: &'a mut Option<NodeId>,
    node: NodeId,
    taffy: &'a TaffyTree<()>,
}

struct Recovery {
    snapshot: crate::session::Snapshot,
    panel: Box<dyn Widget>,
}

/// A bounded collection of retained documents in one shared browser runtime.
#[allow(
    clippy::struct_excessive_bools,
    reason = "layout invalidation, address focus, session initialization and native fullscreen observations are independent flags; exit admission already uses ExitState"
)]
pub struct BrowserTabs {
    tabs: Vec<Tab>,
    // Host node bindings remain alive until the next structure rebuild.
    retired: Vec<Box<dyn Widget>>,
    active_id: u64,
    next_id: u64,
    bar: Box<BrowserTabStrip>,
    closed: Vec<String>,
    rect: Rect,
    dirty: bool,
    focus_new_address: bool,
    buffered_address: bool,
    layout_active_id: Option<u64>,
    layout_immersive: Option<bool>,
    address_focus_node: Option<NodeId>,
    cursor: Point,
    tab_menu: ContextMenu,
    tab_menu_target: Option<u64>,
    tab_menu_actions: Vec<Option<TabMenuAction>>,
    session: Option<crate::session::SessionStore>,
    saved_session: Option<crate::session::Snapshot>,
    session_initialized: bool,
    recovery: Option<Recovery>,
    recovery_choice: ui::LocalState<Option<bool>>,
    exit_state: ExitState,
    exit_choice: ui::LocalState<Option<bool>>,
    exit_panel: Option<Box<dyn Widget>>,
    close_intent: bool,
    window_fullscreen_active: bool,
    observed_active_content_fullscreen: bool,
    fullscreen_hint_deadline: Option<Instant>,
    fullscreen_hint: Option<Box<dyn Widget>>,
}

impl BrowserTabs {
    pub fn new(mut widget: BrowserWidget) -> Self {
        widget.set_tab_identity(1);
        let bar = BrowserTabStrip::new(vec![TabDescriptor::new(1, "New tab")], 1);
        Self {
            tabs: vec![Tab {
                id: 1,
                widget: Box::new(widget),
            }],
            retired: Vec::new(),
            active_id: 1,
            next_id: 2,
            bar: Box::new(bar),
            closed: Vec::new(),
            rect: Rect::ZERO,
            dirty: true,
            focus_new_address: false,
            buffered_address: false,
            layout_active_id: None,
            layout_immersive: None,
            address_focus_node: None,
            cursor: Point::ORIGIN,
            tab_menu: ContextMenu::new(),
            tab_menu_target: None,
            tab_menu_actions: Vec::new(),
            session: None,
            saved_session: None,
            session_initialized: false,
            recovery: None,
            recovery_choice: ui::LocalState::new(None),
            exit_state: ExitState::Normal,
            exit_choice: ui::LocalState::new(None),
            exit_panel: None,
            close_intent: false,
            window_fullscreen_active: false,
            observed_active_content_fullscreen: false,
            fullscreen_hint_deadline: None,
            fullscreen_hint: None,
        }
    }
    fn immersive(&self) -> bool {
        self.window_fullscreen_active && self.active().content_fullscreen()
    }
    fn make_fullscreen_hint() -> Box<dyn Widget> {
        use ui::a11y::Politeness;
        use ui::widget::{Container, Text};
        // Theme-driven: the hint pill takes the theme's own surface and body
        // ink instead of a fixed near-black fill with white text, so it reads
        // on the light palette as well (HIG §7 theme-driven, pixel-clean).
        Box::new(
            Container::new()
                .with_no_padding()
                .with_height(48.0)
                .with_child(Box::new(
                    Container::new()
                        .with_centered_max_width(240.0)
                        .with_background()
                        .with_child(Box::new(
                            Text::new("Esc to exit full screen")
                                .with_id("browser.fullscreen-hint")
                                .with_live(Politeness::Polite)
                                .with_size(14.0),
                        )),
                )),
        )
    }
    fn clear_fullscreen_hint(&mut self) {
        self.fullscreen_hint_deadline = None;
        self.fullscreen_hint = None;
    }
    fn start_fullscreen_hint(&mut self) {
        self.fullscreen_hint_deadline = Some(ui::time::now() + FULLSCREEN_HINT_DURATION);
        self.fullscreen_hint = Some(Self::make_fullscreen_hint());
    }
    fn reconcile_active_content_fullscreen(&mut self) {
        let content_fullscreen = self.active().content_fullscreen();
        if content_fullscreen == self.observed_active_content_fullscreen {
            return;
        }
        self.observed_active_content_fullscreen = content_fullscreen;
        if content_fullscreen && self.window_fullscreen_active {
            self.start_fullscreen_hint();
        } else {
            self.clear_fullscreen_hint();
        }
        self.dirty = true;
    }
    fn make_exit_panel(&self, cancelling: bool) -> Box<dyn Widget> {
        use ui::widget::{Button, Column, Container, Row, Text};
        let status = self
            .active()
            .download_exit_status()
            .unwrap_or_else(|| "Waiting for native transfer cleanup".into());
        let mut column = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new(if cancelling {
                    "Cancelling downloads before quitting…"
                } else {
                    "Downloads are still running"
                })
                .with_size(18.0),
            ))
            .push(Box::new(Text::new(&status).with_size(13.0)));
        if !cancelling {
            let keep = self.exit_choice.writer();
            let cancel = self.exit_choice.writer();
            column = column.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new("browser.download-exit.keep", "Keep downloading", None)
                            .on_effect(move || keep.set(Some(false))),
                    ))
                    .push(Box::new(
                        Button::new(
                            "browser.download-exit.cancel",
                            "Cancel downloads and quit",
                            None,
                        )
                        .on_effect(move || cancel.set(Some(true))),
                    )),
            ));
        }
        Box::new(
            Container::new()
                .with_background()
                .with_child(Box::new(column)),
        )
    }
    /// Attach the normal-session store and prepare startup recovery.
    ///
    /// # Errors
    /// Returns an error if session loading, restoration or startup persistence fails.
    pub fn with_session(mut self, store: crate::session::SessionStore) -> Result<Self, String> {
        let saved = match store.load() {
            Ok(saved) => saved,
            Err(error) => {
                tracing::warn!(%error, "Ignoring invalid browser session");
                None
            }
        };
        self.session = Some(store);
        match saved {
            Some(snapshot) if !snapshot.clean_exit => {
                self.active_mut()
                    .prepare_session_uri("about:blank".into(), true);
                self.recovery = Some(Recovery {
                    snapshot,
                    panel: self.make_recovery_panel(),
                });
            }
            Some(snapshot) => {
                self.restore_snapshot(&snapshot)?;
                self.persist_session(false)?;
            }
            None => {
                self.persist_session(false)?;
            }
        }
        self.session_initialized = true;
        Ok(self)
    }
    fn make_recovery_panel(&self) -> Box<dyn Widget> {
        use ui::widget::{Button, Column, Container, Row, Text};
        let restore = self.recovery_choice.writer();
        let fresh = self.recovery_choice.writer();
        Box::new(Container::new().with_background().with_child(Box::new(Column::new()
            .push(Box::new(Text::new("Restore your browser session?").with_size(24.0)))
            .push(Box::new(Text::new("The browser did not close cleanly. Restore your saved tabs, or start fresh.")))
            .push(Box::new(Row::new()
                .push(Box::new(Button::new("browser.session.restore", "Restore", None).on_effect(move || restore.set(Some(true)))))
                .push(Box::new(Button::new("browser.session.fresh", "Start fresh", None).on_effect(move || fresh.set(Some(false))))))))))
    }
    fn session_snapshot(&self, clean: bool) -> crate::session::Snapshot {
        crate::session::Snapshot {
            version: 1,
            tabs: self
                .tabs
                .iter()
                .map(|tab| crate::session::SavedTab {
                    url: if crate::session::safe_url(tab.widget.persisted_url()) {
                        tab.widget.persisted_url().to_owned()
                    } else {
                        "about:blank".into()
                    },
                    pinned: self.bar.is_pinned(tab.id),
                })
                .collect(),
            active: self
                .tabs
                .iter()
                .position(|tab| tab.id == self.active_id)
                .unwrap_or(0),
            clean_exit: clean,
        }
    }
    fn persist_session(&mut self, clean: bool) -> Result<(), String> {
        if self.recovery.is_some() {
            return Ok(());
        }
        let snapshot = self.session_snapshot(clean);
        if self.saved_session.as_ref() != Some(&snapshot) {
            if let Some(store) = &self.session {
                store.save(&snapshot)?;
            }
            self.saved_session = Some(snapshot);
        }
        Ok(())
    }
    fn restore_snapshot(&mut self, snapshot: &crate::session::Snapshot) -> Result<(), String> {
        snapshot.validate()?;
        let mut restored = Vec::new();
        // Reuse the already-created engine for the active page. Other endpoints
        // start at about:blank and defer their URL until selected.
        let active = snapshot.active;
        let active_tab = snapshot
            .tabs
            .get(active)
            .ok_or_else(|| "Saved browser session has no active tab".to_string())?;
        self.active_mut()
            .prepare_session_uri(active_tab.url.clone(), false);
        for (index, saved) in snapshot.tabs.iter().enumerate() {
            if index == active {
                continue;
            }
            let engine = self.active().engine_handle().create_tab()?;
            engine.set_visible(false);
            let mut widget = BrowserWidget::with_engine_mode(
                engine,
                self.active().is_headless(),
                self.active().browsing_mode(),
            );
            widget.inherit_workspace_state(self.active());
            widget.share_history_pause(self.active());
            widget.prepare_session_uri(saved.url.clone(), true);
            let id = self.next_id;
            self.next_id += 1;
            widget.set_tab_identity(id);
            restored.push((
                index,
                Tab {
                    id,
                    widget: Box::new(widget),
                },
            ));
        }
        let active_tab = self.tabs.remove(0);
        restored.push((active, active_tab));
        restored.sort_by_key(|(index, _)| *index);
        self.tabs = restored.into_iter().map(|(_, tab)| tab).collect();
        let descriptors = self
            .tabs
            .iter()
            .map(|tab| TabDescriptor::new(tab.id, tab.widget.tab_title()))
            .collect();
        *self.bar = BrowserTabStrip::new(descriptors, self.active_id);
        for (tab, saved) in self.tabs.iter().zip(&snapshot.tabs) {
            self.bar.set_pinned(tab.id, saved.pinned);
        }
        self.dirty = true;
        Ok(())
    }
    fn choose_recovery(&mut self, restore: bool) -> Result<(), String> {
        let Some(recovery) = self.recovery.as_ref() else {
            return Ok(());
        };
        let mut chosen = if restore {
            recovery.snapshot.clone()
        } else {
            crate::session::Snapshot {
                version: 1,
                tabs: vec![crate::session::SavedTab {
                    url: "about:blank".into(),
                    pinned: false,
                }],
                active: 0,
                clean_exit: false,
            }
        };
        chosen.clean_exit = false;
        if let Some(store) = &self.session {
            store.save(&chosen)?;
        }
        self.restore_snapshot(&chosen)?;
        if let Some(recovery) = self.recovery.take() {
            // Retained layout nodes can still refer to this panel until rebuild.
            self.retired.push(recovery.panel);
        }
        self.saved_session = Some(chosen);
        self.dirty = true;
        Ok(())
    }
    fn hold_input_until_layout(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::keyboard::ModifiersState,
    ) -> bool {
        // A switch can be followed by another input before the host rebuilds.
        // Retained compositions still contain NodeIds from their previous tree.
        if self.layout_active_id != Some(self.active_id)
            || self.layout_immersive != Some(self.immersive())
        {
            if self.layout_active_id != Some(self.active_id)
                && self.focus_new_address
                && let WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == ElementState::Pressed
                && !modifiers.control_key()
                && !modifiers.super_key()
            {
                let replace = !self.buffered_address;
                if self
                    .active_mut()
                    .buffer_tab_address(&key.logical_key, replace)
                {
                    self.buffered_address = true;
                }
            }
            return true;
        }
        false
    }
    fn handle_retained_event(&mut self, context: RetainedEvent<'_>) {
        let RetainedEvent {
            event,
            scale,
            modifiers,
            state,
            path,
            handled,
            focus,
            node,
            taffy,
        } = context;
        let Ok(children) = taffy.children(node) else {
            return;
        };
        if self.layout_immersive == Some(true) {
            if let Some(page) = children.first() {
                self.active_mut().handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor: scale,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target: focus,
                        node_id: *page,
                        taffy,
                    },
                );
            }
            return;
        }
        if let Some(panel) = self.exit_panel.as_mut() {
            if let Some(panel_node) = children.get(1) {
                panel.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor: scale,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target: focus,
                        node_id: *panel_node,
                        taffy,
                    },
                );
            }
            return;
        }
        if let Some(bar) = children.first() {
            self.bar.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: scale,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target: focus,
                    node_id: *bar,
                    taffy,
                },
            );
        }
        if let Some(id) = self.bar.take_close() {
            self.close(id);
            *handled = true;
            return;
        }
        if self.bar.take_new_tab().is_some() {
            if let Err(error) = self.open(None, false) {
                tracing::warn!(%error, "Cannot open tab");
            }
            *handled = true;
            return;
        }
        if let Some((from, to)) = self.bar.take_reorder() {
            self.reorder(from, to);
        }
        if let Some((id, point)) = self.bar.take_context_menu() {
            self.open_tab_menu(id, point);
            *handled = true;
            return;
        }
        let selected = self.bar.active_id();
        if selected != self.active_id {
            self.activate(selected);
            *handled = true;
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Middle,
            ..
        } = event
            && let Some(index) = self
                .bar
                .tab_rects()
                .iter()
                .position(|rect| rect.contains(self.cursor))
            && let Some(id) = self.bar.visible_tab_id(index)
        {
            self.close(id);
            *handled = true;
            return;
        }
        if !*handled && let Some(page) = children.get(1) {
            self.active_mut().handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: scale,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target: focus,
                    node_id: *page,
                    taffy,
                },
            );
        }
    }
    #[cfg(feature = "wpe")]
    pub fn content_rect_handle(&self) -> std::sync::Arc<std::sync::Mutex<Option<[f32; 4]>>> {
        self.active().content_rect_handle()
    }
    fn active(&self) -> &BrowserWidget {
        // A workspace always contains its active tab, including after last close.
        #[allow(clippy::unwrap_used, reason = "workspace retains an active tab")]
        &self
            .tabs
            .iter()
            .find(|tab| tab.id == self.active_id)
            .unwrap()
            .widget
    }
    fn active_mut(&mut self) -> &mut BrowserWidget {
        #[allow(clippy::unwrap_used, reason = "workspace retains an active tab")]
        &mut self
            .tabs
            .iter_mut()
            .find(|tab| tab.id == self.active_id)
            .unwrap()
            .widget
    }
    pub fn engine_handle(&self) -> ui::webview::SharedEngine {
        self.active().engine_handle()
    }
    /// Open another tab, optionally without activating it.
    ///
    /// # Errors
    /// Returns an error if the tab limit is reached or the engine cannot create a tab.
    pub fn open(&mut self, url: Option<String>, background: bool) -> Result<(), String> {
        if self.tabs.len() >= 32 {
            return Err("At most 32 tabs can be open".into());
        }
        let focus_address = url.is_none();
        let engine = self.active().engine_handle().create_tab()?;
        engine.set_visible(false);
        let headless = self.active().is_headless();
        let mut widget =
            BrowserWidget::with_engine_mode(engine, headless, self.active().browsing_mode())
                .with_initial_uri(url.unwrap_or_else(|| "about:blank".into()));
        widget.share_history_pause(self.active());
        widget.inherit_workspace_state(self.active());
        let id = self.next_id;
        self.next_id += 1;
        widget.set_tab_identity(id);
        self.tabs.push(Tab {
            id,
            widget: Box::new(widget),
        });
        self.bar.tabs_mut().push(TabDescriptor::new(id, "Loading…"));
        if !background {
            self.activate(id);
            self.focus_new_address = focus_address;
            self.buffered_address = false;
        }
        self.dirty = true;
        Ok(())
    }
    fn activate(&mut self, id: u64) {
        if id == self.active_id || !self.tabs.iter().any(|tab| tab.id == id) {
            return;
        }
        self.active_mut().exit_content_fullscreen();
        self.active_mut().deactivate_credentials();
        self.active().engine_handle().set_visible(false);
        self.active_id = id;
        self.address_focus_node = None;
        self.bar.set_active(id);
        self.active().engine_handle().set_visible(true);
        self.active_mut().exit_content_fullscreen();
        let window_fullscreen_active = self.window_fullscreen_active;
        self.active_mut()
            .set_window_fullscreen_active(window_fullscreen_active);
        self.observed_active_content_fullscreen = false;
        self.clear_fullscreen_hint();
        self.active_mut().refresh_shared_history();
        self.active_mut().activate_session_uri();
        self.dirty = true;
    }
    fn close(&mut self, id: u64) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return;
        };
        if !tab.widget.engine_handle().request_close() {
            // TabBar optimistically removes its pill for close animation.
            // Native admission is asynchronous: restore the visible tab until
            // its closed event arrives, including when a download blocks it.
            if !self.bar.tabs().iter().any(|tab| tab.id == id) {
                let index = self.tabs.iter().position(|tab| tab.id == id).unwrap_or(0);
                let title = tab.widget.tab_title();
                self.bar
                    .tabs_mut()
                    .insert(index, TabDescriptor::new(id, title));
            }
            self.bar.finalize_close(id);
            self.bar.set_active(self.active_id);
            self.dirty = true;
            return;
        }
        self.finish_close(id);
    }
    fn finish_close(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        // Allocate a replacement before releasing the final process owner.
        if self.tabs.len() == 1 && self.open(None, false).is_err() {
            return;
        }
        if self.active_id == id
            && let Some(next) = self
                .tabs
                .get(index + 1)
                .or_else(|| self.tabs.iter().find(|tab| tab.id != id))
        {
            self.activate(next.id);
        }
        let tab = self.tabs.remove(index);
        if self.closed.len() == 20 {
            self.closed.remove(0);
        }
        self.closed.push(tab.widget.tab_url().to_owned());
        self.retired.push(tab.widget);
        self.bar.remove_tab(id);
        self.bar.finalize_close(id);
        self.bar.set_active(self.active_id);
        self.dirty = true;
    }
    fn reopen(&mut self) -> Result<(), String> {
        if let Some(url) = self.closed.last().cloned() {
            self.open(Some(url), false)?;
            self.closed.pop();
        }
        Ok(())
    }
    fn set_pinned(&mut self, id: u64, pinned: bool) {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        self.bar.set_pinned(id, pinned);
        let pinned_count = self
            .tabs
            .iter()
            .filter(|tab| self.bar.is_pinned(tab.id))
            .count();
        let to = if pinned {
            pinned_count.saturating_sub(1)
        } else {
            pinned_count
        };
        if from != to && to < self.tabs.len() {
            self.reorder(from, to);
        }
    }
    fn duplicate(&mut self, id: u64) -> Result<(), String> {
        let url = self
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(|tab| tab.widget.tab_url().to_owned())
            .ok_or_else(|| "Tab no longer exists".to_string())?;
        self.open(Some(url), false)
    }
    fn set_muted(&mut self, id: u64, muted: bool) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.widget.set_tab_muted(muted);
        }
    }
    fn close_others(&mut self, id: u64) {
        let candidates: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| tab.id != id && !self.bar.is_pinned(tab.id))
            .map(|tab| tab.id)
            .collect();
        for candidate in candidates {
            self.close(candidate);
        }
    }
    fn close_to_right(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let candidates: Vec<_> = self
            .tabs
            .iter()
            .skip(index + 1)
            .filter(|tab| !self.bar.is_pinned(tab.id))
            .map(|tab| tab.id)
            .collect();
        for candidate in candidates {
            self.close(candidate);
        }
    }
    fn open_tab_menu(&mut self, id: u64, point: Point) {
        self.tab_menu = ContextMenu::new();
        self.tab_menu_actions.clear();
        self.tab_menu_target = Some(id);
        let pinned = self.bar.is_pinned(id);
        let muted = self
            .tabs
            .iter()
            .find(|t| t.id == id)
            .is_some_and(|t| t.widget.tab_muted());
        let items = [
            ("Duplicate", TabMenuAction::Duplicate),
            (
                if pinned { "Unpin" } else { "Pin" },
                TabMenuAction::TogglePin,
            ),
            (
                if muted { "Unmute" } else { "Mute" },
                TabMenuAction::ToggleMute,
            ),
            ("Close", TabMenuAction::Close),
            ("Close Others", TabMenuAction::CloseOthers),
            ("Close to Right", TabMenuAction::CloseRight),
            ("Reopen Closed Tab", TabMenuAction::Reopen),
        ];
        for (label, action) in items {
            self.tab_menu.push(MenuItem::action(label, || {}));
            self.tab_menu_actions.push(Some(action));
        }
        self.tab_menu.open_anchored(
            Rect::new(point.x, point.y, point.x + 1.0, point.y + 1.0),
            self.rect,
        );
    }
    fn apply_tab_menu(&mut self, index: usize) {
        let Some(action) = self.tab_menu_actions.get(index).and_then(|a| *a) else {
            return;
        };
        let Some(id) = self.tab_menu_target else {
            return;
        };
        match action {
            TabMenuAction::Duplicate => {
                let _ = self.duplicate(id);
            }
            TabMenuAction::TogglePin => self.set_pinned(id, !self.bar.is_pinned(id)),
            TabMenuAction::ToggleMute => {
                let muted = self
                    .tabs
                    .iter()
                    .find(|t| t.id == id)
                    .is_some_and(|t| t.widget.tab_muted());
                self.set_muted(id, !muted);
            }
            TabMenuAction::Close => self.close(id),
            TabMenuAction::CloseOthers => self.close_others(id),
            TabMenuAction::CloseRight => self.close_to_right(id),
            TabMenuAction::Reopen => {
                let _ = self.reopen();
            }
        }
    }
    fn reorder(&mut self, from: usize, to: usize) {
        if to < self.tabs.len()
            && from != to
            && let Some(source) = self.tabs.get(from)
        {
            let pinned = self.bar.is_pinned(source.id);
            let pinned_count = self
                .tabs
                .iter()
                .filter(|tab| self.bar.is_pinned(tab.id))
                .count();
            let to = if pinned {
                to.min(pinned_count.saturating_sub(1))
            } else {
                to.max(pinned_count)
            };
            if from == to {
                return;
            }
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
            self.bar.reorder(from, to);
            self.dirty = true;
        }
    }
    fn cycle(&mut self, backwards: bool) {
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == self.active_id)
            .unwrap_or(0);
        let next = if backwards {
            (index + self.tabs.len() - 1) % self.tabs.len()
        } else {
            (index + 1) % self.tabs.len()
        };
        if let Some(tab) = self.tabs.get(next) {
            self.activate(tab.id);
        }
    }
    fn shortcut(&mut self, key: &Key, modifiers: winit::keyboard::ModifiersState) -> bool {
        let platform = modifiers.super_key() || modifiers.control_key();
        let shift = modifiers.shift_key();
        match key {
            Key::Character(c) if platform && shift && c.eq_ignore_ascii_case("n") => {
                self.active().request_private_window();
            }
            Key::Character(c) if platform && c.eq_ignore_ascii_case("t") => {
                let result = if shift {
                    self.reopen()
                } else {
                    self.open(None, false)
                };
                if let Err(error) = result {
                    tracing::warn!(%error, "Cannot open tab");
                }
            }
            Key::Character(c) if platform && c.eq_ignore_ascii_case("w") => {
                self.close(self.active_id);
            }
            Key::Named(NamedKey::Tab) if modifiers.control_key() => self.cycle(shift),
            Key::Character(c)
                if platform && shift && matches!(c.as_str(), "[" | "]" | "{" | "}") =>
            {
                self.cycle(matches!(c.as_str(), "[" | "{"));
            }
            Key::Character(c)
                if platform
                    && c.len() == 1
                    && c.parse::<usize>().is_ok_and(|n| (1..=9).contains(&n)) =>
            {
                let n = c.parse::<usize>().unwrap_or(1);
                let tab = if n == 9 {
                    self.tabs.last()
                } else {
                    self.tabs.get(n - 1)
                };
                if let Some(tab) = tab {
                    self.activate(tab.id);
                }
            }
            _ => return false,
        }
        true
    }
}

impl Widget for BrowserTabs {
    ui::provides!(
        dyn ui::widget::FullscreenProvider,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::widget::Continuable,
        dyn ui::widget::controls::ActionControls,
        dyn ui::search::capability::SearchSource,
        dyn ui::component_lifecycle::ComponentLifecycle
    );

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        if self.fullscreen_hint_deadline.is_some()
            && !fullscreen_hint_visible(self.fullscreen_hint_deadline, ui::time::now())
        {
            self.clear_fullscreen_hint();
            self.dirty = true;
        }
        if self.recovery.is_some() {
            if let Some(choice) = self.recovery_choice.get() {
                self.recovery_choice.set(None);
                if let Err(error) = self.choose_recovery(choice) {
                    tracing::error!(%error, "Cannot restore browser session");
                }
            }
            if self.recovery.is_some() {
                return;
            }
        }
        if let Some(cancel) = self.exit_choice.get() {
            self.exit_choice.set(None);
            if cancel {
                self.exit_state = ExitState::Cancelling;
                self.active().cancel_downloads_for_exit();
                self.exit_panel = Some(self.make_exit_panel(true));
            } else {
                self.exit_state = ExitState::Normal;
                self.exit_panel = None;
            }
            self.dirty = true;
        }
        // Accessibility dispatch has no following pointer event to drain the
        // TabBar intents. Apply it at the same synchronization boundary.
        if let Some(id) = self.bar.take_close() {
            self.close(id);
        }
        if self.bar.take_new_tab().is_some()
            && let Err(error) = self.open(None, false)
        {
            tracing::warn!(%error, "Cannot open tab");
        }
        if self.bar.active_id() != self.active_id {
            self.activate(self.bar.active_id());
        }
        if let Some(id) = self.bar.take_toggle_mute() {
            let muted = self
                .tabs
                .iter()
                .find(|tab| tab.id == id)
                .is_some_and(|tab| tab.widget.tab_muted());
            self.set_muted(id, !muted);
        }
        let mut requests = Vec::new();
        let mut closed = Vec::new();
        for tab in &mut self.tabs {
            tab.widget.sync(state, path);
            if tab.widget.take_native_closed() {
                closed.push(tab.id);
            }
            requests.extend(tab.widget.engine_handle().take_new_tab_requests());
            let title = tab.widget.tab_title();
            self.bar.set_visual(tab.id, tab.widget.tab_visual());
            if let Some(desc) = self
                .bar
                .tabs_mut()
                .iter_mut()
                .find(|desc| desc.id == tab.id)
                && desc.title != title
            {
                desc.title = title;
                self.dirty = true;
            }
        }
        for id in closed {
            self.finish_close(id);
        }
        for (url, background) in requests {
            if let Err(error) = self.open(Some(url), background) {
                tracing::warn!(%error, "Cannot open link tab");
            }
        }
        // A late native entry notification must not hide the window-close
        // choices or cancellation progress while the exit flow owns the UI.
        if self.exit_state != ExitState::Normal {
            self.active_mut().exit_content_fullscreen();
        }
        self.reconcile_active_content_fullscreen();
        if self.exit_state == ExitState::Cancelling {
            if self.active().download_exit_cleanup_complete() {
                self.exit_state = ExitState::Ready;
                self.exit_panel = None;
                self.close_intent = true;
            } else {
                self.exit_panel = Some(self.make_exit_panel(true));
            }
            self.dirty = true;
        }
        if let Err(error) = self.persist_session(false) {
            tracing::error!(%error, "Cannot save browser session");
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.retired.clear();
        if let Some(recovery) = self.recovery.as_mut() {
            self.layout_active_id = None;
            self.layout_immersive = None;
            let panel = recovery.panel.build_layout(taffy, theme, nodes);
            let root = taffy.parent_node(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Style::default()
                },
                &[panel],
            );
            nodes.insert(root, std::ptr::from_mut::<dyn Widget>(self));
            return root;
        }
        if self.immersive() {
            let page = self.active_mut().build_layout(taffy, theme, nodes);
            self.layout_active_id = Some(self.active_id);
            self.layout_immersive = Some(true);
            let mut children = vec![page];
            if let Some(hint) = self.fullscreen_hint.as_mut() {
                let hint = hint.build_layout(taffy, theme, nodes);
                let mut style = taffy.style(hint).cloned().unwrap_or_default();
                style.position = taffy::Position::Absolute;
                style.inset = taffy::Rect {
                    left: length(0.0_f32),
                    right: length(0.0_f32),
                    top: length(24.0_f32),
                    bottom: auto(),
                };
                let _ = taffy.set_style(hint, style);
                children.push(hint);
            }
            let root = taffy.parent_node(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Style::default()
                },
                &children,
            );
            nodes.insert(root, std::ptr::from_mut::<dyn Widget>(self));
            return root;
        }
        let bar = self.bar.build_layout(taffy, theme, nodes);
        let panel = self
            .exit_panel
            .as_mut()
            .map(|panel| panel.build_layout(taffy, theme, nodes));
        let page = self.active_mut().build_layout(taffy, theme, nodes);
        self.layout_active_id = Some(self.active_id);
        self.layout_immersive = Some(false);
        let focus_address = self.focus_new_address || self.address_focus_node.is_some();
        self.address_focus_node = None;
        let buffered = self.buffered_address;
        if focus_address && self.active_mut().focus_tab_address_with_caret(buffered) {
            self.buffered_address = false;
            self.focus_new_address = false;
            self.address_focus_node = nodes.iter().find_map(|(id, widget)| {
                // Nodes were built immediately above and all widgets are retained.
                let widget = unsafe { &**widget };
                (widget.test_id() == Some("browser.url_input")).then_some(*id)
            });
        }
        let root = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..Style::default()
            },
            &panel.map_or_else(|| vec![bar, page], |panel| vec![bar, panel, page]),
        );
        nodes.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        if let Ok(children) = taffy.children(node) {
            if let Some(recovery) = self.recovery.as_mut() {
                if let Some(panel) = children.first() {
                    recovery
                        .panel
                        .apply_layout(taffy, *panel, self.rect.origin());
                }
                return;
            }
            if self.layout_immersive == Some(true) {
                if let Some(page) = children.first() {
                    let origin = self.rect.origin();
                    self.active_mut().apply_layout(taffy, *page, origin);
                }
                if let Some(hint) = self.fullscreen_hint.as_mut()
                    && let Some(node) = children.get(1)
                {
                    hint.apply_layout(taffy, *node, self.rect.origin());
                }
                return;
            }
            if let Some(bar) = children.first() {
                self.bar.apply_layout(taffy, *bar, self.rect.origin());
            }
            let page_index = if self.exit_panel.is_some() { 2 } else { 1 };
            if let Some(panel) = self.exit_panel.as_mut()
                && let Some(node) = children.get(1)
            {
                panel.apply_layout(taffy, *node, self.rect.origin());
            }
            if let Some(page) = children.get(page_index) {
                let origin = self.rect.origin();
                self.active_mut().apply_layout(taffy, *page, origin);
            }
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(recovery) = self.recovery.as_mut() {
            recovery.panel.draw(scene, transform, tr, theme, presence);
            return;
        }
        if self.layout_immersive == Some(true) {
            self.active_mut()
                .draw(scene, transform, tr, theme, presence);
            if let Some(hint) = self.fullscreen_hint.as_mut() {
                hint.draw(scene, transform, tr, theme, presence);
            }
            return;
        }
        self.bar.draw(scene, transform, tr, theme, presence);
        if let Some(panel) = self.exit_panel.as_mut() {
            panel.draw(scene, transform, tr, theme, presence);
        }
        self.active_mut()
            .draw(scene, transform, tr, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if *ctx.handled {
            return;
        }
        if let Some(recovery) = self.recovery.as_mut() {
            if let Ok(children) = ctx.taffy.children(ctx.node_id)
                && let Some(panel) = children.first()
            {
                recovery
                    .panel
                    .handle_event(event, &mut ctx.reborrow(ctx.path, *panel));
            }
            return;
        }
        if matches!(
            event,
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && key.logical_key == Key::Named(NamedKey::Escape)
        ) && self.active().content_fullscreen()
        {
            self.active_mut().exit_content_fullscreen();
            self.observed_active_content_fullscreen = false;
            self.clear_fullscreen_hint();
            self.dirty = true;
            *ctx.handled = true;
            return;
        }
        if self.tab_menu.is_open() {
            self.tab_menu.handle_event(event, ctx);
            if let Some(index) = self.tab_menu.take_selection() {
                self.apply_tab_menu(index);
                self.dirty = true;
            }
            if *ctx.handled {
                return;
            }
        }
        if self.hold_input_until_layout(event, ctx.modifiers.state()) {
            *ctx.handled = true;
            return;
        }
        if let Some(address) = self.address_focus_node.take() {
            *ctx.focus_target = Some(address);
        }
        if matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::ModifiersChanged(_)
        ) {
            let m = ctx.modifiers.state();
            self.active()
                .engine_handle()
                .set_modifiers(ui::webview::KeyModifiers {
                    ctrl: m.control_key(),
                    shift: m.shift_key(),
                    alt: m.alt_key(),
                    meta: m.super_key(),
                });
        }
        for tab in &mut self.tabs {
            tab.widget.set_window_scale(ctx.scale_factor);
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if self.exit_state == ExitState::Choice
                && key.state == ElementState::Pressed
                && key.logical_key == Key::Named(NamedKey::Escape)
            {
                self.exit_state = ExitState::Normal;
                self.exit_panel = None;
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
            if key.state == ElementState::Pressed
                && self.shortcut(&key.logical_key, ctx.modifiers.state())
            {
                *ctx.handled = true;
                return;
            }
        }
        self.handle_retained_event(RetainedEvent {
            event,
            scale: ctx.scale_factor,
            modifiers: ctx.modifiers,
            state: ctx.state,
            path: ctx.path,
            handled: ctx.handled,
            focus: ctx.focus_target,
            node: ctx.node_id,
            taffy: ctx.taffy,
        });
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        if let Some(recovery) = self.recovery.as_mut() {
            return recovery
                .panel
                .perform_semantic_action(target, action, state, path);
        }
        if matches!(action, ui::a11y::Action::Activate)
            && let Some(index) = target
                .strip_prefix("tab-menu:")
                .and_then(|value| value.parse::<usize>().ok())
        {
            self.apply_tab_menu(index);
            self.tab_menu.close();
            self.dirty = true;
            return true;
        }
        if self
            .exit_panel
            .as_mut()
            .is_some_and(|panel| panel.perform_semantic_action(target, action, state, path))
        {
            return true;
        }
        if self
            .bar
            .perform_semantic_action(target, action, state, path)
        {
            self.activate(self.bar.active_id());
            return true;
        }
        self.active_mut()
            .perform_semantic_action(target, action, state, path)
    }
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.recovery.is_some() {
            return;
        }
        self.active_mut()
            .draw_overlay(scene, transform, tr, theme, presence);
        self.tab_menu.draw(scene, transform, tr, theme, presence);
    }
    fn poll_resource_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        self.tabs.iter_mut().find_map(|tab| {
            tab.widget
                .poll_resource_operation(invocation, cancellation_requested, doc, path)
        })
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(recovery) = self.recovery.as_ref() {
            return vec![recovery.panel.as_ref()];
        }
        if self.immersive() {
            return std::iter::once(self.active() as &dyn Widget)
                .chain(self.fullscreen_hint.as_deref())
                .collect();
        }
        let mut children: Vec<&dyn Widget> = vec![self.bar.as_ref()];
        if let Some(panel) = self.exit_panel.as_ref() {
            children.push(panel.as_ref());
        }
        children.push(self.active());
        children
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.recovery.is_some() {
            let mut children: Vec<&mut dyn Widget> = Vec::new();
            if let Some(recovery) = self.recovery.as_mut() {
                children.push(recovery.panel.as_mut());
            }
            return children;
        }
        if self.immersive() {
            let active_id = self.active_id;
            let (tabs, hint) = (&mut self.tabs, &mut self.fullscreen_hint);
            let mut children: Vec<&mut dyn Widget> = Vec::new();
            if let Some(tab) = tabs.iter_mut().find(|tab| tab.id == active_id) {
                children.push(tab.widget.as_mut());
            }
            if let Some(hint) = hint.as_mut() {
                children.push(hint.as_mut());
            }
            return children;
        }
        let active_id = self.active_id;
        let mut children: Vec<&mut dyn Widget> = vec![self.bar.as_mut()];
        if let Some(panel) = self.exit_panel.as_mut() {
            children.push(panel.as_mut());
        }
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
            children.push(tab.widget.as_mut());
        }
        children
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if self.recovery.is_some() {
            return Some(AccessNode::new(Role::Group).with_name("Browser session recovery"));
        }
        let menu_children = if self.tab_menu.is_open() {
            self.tab_menu_actions
                .iter()
                .enumerate()
                .filter_map(|(index, action)| {
                    let label = match (*action)? {
                        TabMenuAction::Duplicate => "Duplicate",
                        TabMenuAction::TogglePin => {
                            if self
                                .tab_menu_target
                                .is_some_and(|id| self.bar.is_pinned(id))
                            {
                                "Unpin"
                            } else {
                                "Pin"
                            }
                        }
                        TabMenuAction::ToggleMute => {
                            if self
                                .tab_menu_target
                                .and_then(|id| self.tabs.iter().find(|tab| tab.id == id))
                                .is_some_and(|tab| tab.widget.tab_muted())
                            {
                                "Unmute"
                            } else {
                                "Mute"
                            }
                        }
                        TabMenuAction::Close => "Close",
                        TabMenuAction::CloseOthers => "Close Others",
                        TabMenuAction::CloseRight => "Close to Right",
                        TabMenuAction::Reopen => "Reopen Closed Tab",
                    };
                    Some(
                        AccessNode::new(Role::MenuItem)
                            .with_name(label)
                            .with_action_target(format!("tab-menu:{index}"))
                            .with_hashed_key(format!("tab-menu:{index}")),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut children = Vec::new();
        if self.fullscreen_hint.is_some() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Esc to exit full screen")
                    .with_hashed_key("browser.fullscreen-hint-status"),
            );
        }
        if self.tab_menu.is_open() {
            children.push(
                AccessNode::new(Role::Menu)
                    .with_name("Tab actions")
                    .with_state(AccessState {
                        expanded: Some(true),
                        ..Default::default()
                    })
                    .with_children(menu_children),
            );
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name("Browser tabs")
                .with_children(children),
        )
    }
    fn take_structure_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.dirty);
        let page = self.active_mut().take_structure_dirty();
        dirty || page || self.bar.take_structure_dirty()
    }
    fn needs_animation_frame(&self) -> bool {
        fullscreen_hint_visible(self.fullscreen_hint_deadline, ui::time::now())
            || self.bar.needs_animation_frame()
            || self.active().needs_animation_frame()
    }
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.bar.set_motion_reduced(reduced);
        for tab in &mut self.tabs {
            tab.widget.set_motion_reduced(reduced);
        }
    }
    fn modal_capture(&self) -> Option<Rect> {
        if let Some(panel) = self.exit_panel.as_ref() {
            Some(panel.get_rect())
        } else if self.tab_menu.is_open() {
            Some(self.tab_menu.get_rect())
        } else {
            self.active().modal_capture()
        }
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.active().cursor_shape(point)
    }
    fn name(&self) -> &'static str {
        "BrowserTabs"
    }
    fn test_id(&self) -> Option<&str> {
        Some("browser.workspace")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::component_lifecycle::ComponentLifecycle for BrowserTabs {
    fn on_window_close_requested(&mut self) -> WindowCloseAdmission {
        match self.exit_state {
            ExitState::Choice | ExitState::Cancelling => WindowCloseAdmission::Defer,
            ExitState::Normal if self.active().download_exit_requires_choice() => {
                // The immersive layout omits window-level dialogs. Leave page
                // fullscreen before asking what to do with active downloads.
                self.active_mut().exit_content_fullscreen();
                self.observed_active_content_fullscreen = false;
                self.clear_fullscreen_hint();
                self.exit_state = ExitState::Choice;
                self.exit_panel = Some(self.make_exit_panel(false));
                self.dirty = true;
                WindowCloseAdmission::Defer
            }
            ExitState::Normal | ExitState::Ready => WindowCloseAdmission::Allow,
        }
    }
    fn take_window_close_intent(&mut self) -> bool {
        std::mem::take(&mut self.close_intent)
    }
}

impl ui::search::capability::SearchSource for BrowserTabs {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        self.active().search_providers()
    }
}

impl ui::widget::controls::ActionControls for BrowserTabs {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        if let Some(recovery) = self.recovery.as_mut() {
            return ui::widget::controls::take_control_invocation(recovery.panel.as_mut());
        }
        if let Some(invocation) = self
            .exit_panel
            .as_mut()
            .and_then(|panel| ui::widget::controls::take_control_invocation(panel.as_mut()))
        {
            return Some(invocation);
        }
        self.active_mut().take_control_invocation()
    }
}

impl Continuable for BrowserTabs {
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        root: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        self.active().continuation_metadata(state, root)
    }
    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        self.active_mut()
            .restore_continuation_metadata(metadata, state, root)
    }
}

impl FullscreenProvider for BrowserTabs {
    fn fullscreen_intent(&self) -> Option<bool> {
        Some(self.active().content_fullscreen())
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        ui::fullscreen::FullscreenRequest::mirror(ui::fullscreen::FullscreenTarget::Largest)
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        self.window_fullscreen_active = active;
        self.active_mut().set_window_fullscreen_active(active);
        if active && self.active().content_fullscreen() {
            self.observed_active_content_fullscreen = true;
            self.start_fullscreen_hint();
        } else {
            if !active {
                self.active_mut().exit_content_fullscreen();
                self.observed_active_content_fullscreen = false;
            }
            self.clear_fullscreen_hint();
        }
        self.dirty = true;
    }
}

impl FloatingSurfaceProvider for BrowserTabs {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.tabs.iter_mut().find_map(|tab| {
            FloatingSurfaceProvider::take_floating_surface_request(&mut *tab.widget)
        })
    }

    fn take_capture_studio_request(&mut self) -> Option<ui::widget::CaptureMode> {
        FloatingSurfaceProvider::take_capture_studio_request(self.active_mut())
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.tabs
            .iter_mut()
            .flat_map(|tab| {
                FloatingSurfaceProvider::take_floating_surface_source_updates(&mut *tab.widget)
            })
            .collect()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.tabs
            .iter()
            .flat_map(|tab| FloatingSurfaceProvider::active_floating_surface_ids(&*tab.widget))
            .collect()
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        for tab in &mut self.tabs {
            FloatingSurfaceProvider::on_floating_surface_host_message(&mut *tab.widget, message);
        }
    }
}

impl Drop for BrowserTabs {
    fn drop(&mut self) {
        if self.session_initialized
            && !std::thread::panicking()
            && let Err(error) = self.persist_session(true)
        {
            tracing::error!(%error, "Cannot mark browser session clean");
        }
        self.active_mut().clear_history_on_workspace_exit();
    }
}

impl AppWidget for BrowserTabs {
    fn app_kind(&self) -> AppKind {
        AppKind::Browser
    }
    fn app_title(&self) -> &str {
        self.active().app_title()
    }
    fn app_icon(&self) -> IconDescriptor {
        self.active().app_icon()
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Browser)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrowserWidget;
    use std::sync::Arc;
    use ui::webview::test_engine::TestEngine;
    use ui::webview::{FrameSlot, SharedEngine, WebViewCommand, WebViewEngine, WebViewEvent};
    #[derive(Default)]
    struct FullscreenEngine {
        events: std::sync::Mutex<Vec<WebViewEvent>>,
        commands: std::sync::Mutex<Vec<WebViewCommand>>,
        children: std::sync::Mutex<Vec<Arc<FullscreenEngine>>>,
        frames: FrameSlot,
    }
    impl FullscreenEngine {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                events: std::sync::Mutex::default(),
                commands: std::sync::Mutex::default(),
                children: std::sync::Mutex::default(),
                frames: Arc::new(std::sync::Mutex::new(None)),
            })
        }
        fn emit(&self, event: WebViewEvent) {
            self.events.lock().unwrap().push(event);
        }
    }
    impl WebViewEngine for FullscreenEngine {
        fn create_tab(&self) -> Result<SharedEngine, String> {
            let child = Self::new();
            self.children.lock().unwrap().push(child.clone());
            Ok(child)
        }
        fn send(&self, command: WebViewCommand) {
            self.commands.lock().unwrap().push(command);
        }
        fn frames(&self) -> FrameSlot {
            self.frames.clone()
        }
        fn drain_events(&self) -> Vec<WebViewEvent> {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
    }
    struct Engine(Arc<TestEngine>);
    impl WebViewEngine for Engine {
        fn create_tab(&self) -> Result<SharedEngine, String> {
            Ok(Arc::new(Self(TestEngine::new())))
        }
        fn send(&self, c: WebViewCommand) {
            self.0.send(c);
        }
        fn frames(&self) -> FrameSlot {
            self.0.frames()
        }
        fn drain_events(&self) -> Vec<WebViewEvent> {
            self.0.drain_events()
        }
    }
    fn workspace() -> BrowserTabs {
        BrowserTabs::new(BrowserWidget::with_engine(
            Arc::new(Engine(TestEngine::new())),
            true,
        ))
    }
    fn fullscreen_workspace() -> (BrowserTabs, Arc<FullscreenEngine>) {
        let engine = FullscreenEngine::new();
        (
            BrowserTabs::new(BrowserWidget::with_engine(engine.clone(), true)),
            engine,
        )
    }
    fn key_event_with_state(named: NamedKey, state: ElementState) -> WindowEvent {
        let mut key: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        key.logical_key = Key::Named(named);
        key.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        key.location = winit::keyboard::KeyLocation::Standard;
        key.state = state;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: key,
            is_synthetic: false,
        }
    }
    fn key_event(named: NamedKey) -> WindowEvent {
        key_event_with_state(named, ElementState::Pressed)
    }
    fn retained_layout(tabs: &mut BrowserTabs) -> (TaffyTree<()>, NodeId) {
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = tabs.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        tree.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(1000.0),
                height: taffy::AvailableSpace::Definite(700.0),
            },
        )
        .unwrap();
        tabs.apply_layout(&tree, root, Point::ORIGIN);
        (tree, root)
    }
    fn dispatch_key(
        tabs: &mut BrowserTabs,
        tree: &TaffyTree<()>,
        root: NodeId,
        named: NamedKey,
        state: ElementState,
    ) -> bool {
        let mut handled = false;
        tabs.handle_event(
            &key_event_with_state(named, state),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut AutoCommit::new(),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: root,
                taffy: tree,
            },
        );
        handled
    }
    fn layout_rects(tabs: &mut BrowserTabs) -> HashMap<String, Rect> {
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = tabs.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        tree.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(1000.0),
                height: taffy::AvailableSpace::Definite(700.0),
            },
        )
        .unwrap();
        tabs.apply_layout(&tree, root, Point::ORIGIN);
        nodes
            .values()
            .filter_map(|widget| unsafe {
                let widget = &**widget;
                widget
                    .test_id()
                    .map(|id| (id.to_owned(), widget.get_rect()))
            })
            .collect()
    }

    /// Content fullscreen ownership, hint lifetime and exit routes.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn content_fullscreen_owner_hint_and_exit_follow_the_root_window() {
        // Phase `active_content_fullscreen_uses_root_window_contract_and_hides_both_chrome_layers`.
        {
            let (mut tabs, engine) = fullscreen_workspace();
            engine.emit(WebViewEvent::FullscreenChanged(true));
            tabs.sync(&mut AutoCommit::new(), &automerge::ROOT);
            assert_eq!(tabs.fullscreen_intent(), Some(true));

            tabs.on_fullscreen_changed(true);
            let rects = layout_rects(&mut tabs);
            assert!(rects.contains_key("browser.surface"), "{rects:?}");
            assert!(rects.contains_key("browser.fullscreen-hint"), "{rects:?}");
            assert!(!rects.contains_key("browser.tab-strip"), "{rects:?}");
            assert!(!rects.contains_key("browser.url_input"), "{rects:?}");
            assert_eq!(rects["browser.surface"], Rect::new(0.0, 0.0, 1000.0, 700.0));
            assert!(rects["browser.fullscreen-hint"].height() > 0.0);
            assert!(rects["browser.fullscreen-hint"].y0 >= 24.0);
            assert!(tabs.needs_animation_frame());

            let mut tree = TaffyTree::new();
            let node = tree.new_leaf(Style::default()).unwrap();
            let mut handled = false;
            tabs.handle_event(
                &key_event(NamedKey::Escape),
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: &mut AutoCommit::new(),
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut None,
                    node_id: node,
                    taffy: &tree,
                },
            );
            assert!(handled);
            assert_eq!(tabs.fullscreen_intent(), Some(false));
            assert_eq!(
                engine
                    .commands
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|command| matches!(command, WebViewCommand::ExitFullscreen))
                    .count(),
                1
            );
            tabs.on_fullscreen_changed(false);
            let rects = layout_rects(&mut tabs);
            assert!(rects.contains_key("browser.tab-strip"), "{rects:?}");
            assert!(rects.contains_key("browser.url_input"), "{rects:?}");
            assert!(!rects.contains_key("browser.fullscreen-hint"), "{rects:?}");
        }

        // Phase `ordinary_os_fullscreen_keeps_browser_chrome_and_has_no_hint`.
        {
            let (mut tabs, _engine) = fullscreen_workspace();
            tabs.on_fullscreen_changed(true);
            assert_eq!(tabs.fullscreen_intent(), Some(false));
            let rects = layout_rects(&mut tabs);
            assert!(rects.contains_key("browser.tab-strip"), "{rects:?}");
            assert!(rects.contains_key("browser.url_input"), "{rects:?}");
            assert!(!rects.contains_key("browser.fullscreen-hint"), "{rects:?}");
        }

        // Phase `page_entry_while_window_is_already_fullscreen_starts_hint_and_blocks_stale_tree_events`.
        {
            let (mut tabs, engine) = fullscreen_workspace();
            tabs.on_fullscreen_changed(true);
            let (tree, root) = retained_layout(&mut tabs);
            assert!(tabs.fullscreen_hint.is_none());

            engine.emit(WebViewEvent::FullscreenChanged(true));
            tabs.sync(&mut AutoCommit::new(), &automerge::ROOT);

            assert!(tabs.fullscreen_hint.is_some());
            assert!(tabs.needs_animation_frame());
            assert!(dispatch_key(
                &mut tabs,
                &tree,
                root,
                NamedKey::Enter,
                ElementState::Released,
            ));
        }

        // Phase `escape_release_is_blocked_until_immersive_tree_is_rebuilt`.
        {
            let (mut tabs, engine) = fullscreen_workspace();
            engine.emit(WebViewEvent::FullscreenChanged(true));
            tabs.sync(&mut AutoCommit::new(), &automerge::ROOT);
            tabs.on_fullscreen_changed(true);
            let (tree, root) = retained_layout(&mut tabs);

            assert!(dispatch_key(
                &mut tabs,
                &tree,
                root,
                NamedKey::Escape,
                ElementState::Pressed,
            ));
            assert!(dispatch_key(
                &mut tabs,
                &tree,
                root,
                NamedKey::Escape,
                ElementState::Released,
            ));
            assert_eq!(
                engine
                    .commands
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|command| matches!(command, WebViewCommand::ExitFullscreen))
                    .count(),
                1
            );
        }

        // Phase `tab_switch_and_active_close_exit_the_outgoing_fullscreen_owner`.
        {
            let (mut tabs, engine) = fullscreen_workspace();
            engine.emit(WebViewEvent::FullscreenChanged(true));
            tabs.sync(&mut AutoCommit::new(), &automerge::ROOT);
            tabs.on_fullscreen_changed(true);
            tabs.open(Some("https://next.test/".into()), true).unwrap();
            let replacement = tabs.tabs[1].id;
            tabs.activate(replacement);
            assert_eq!(tabs.fullscreen_intent(), Some(false));
            assert!(
                engine
                    .commands
                    .lock()
                    .unwrap()
                    .contains(&WebViewCommand::ExitFullscreen)
            );
            assert!(!tabs.needs_animation_frame());

            let child = engine.children.lock().unwrap()[0].clone();
            child.emit(WebViewEvent::FullscreenChanged(true));
            tabs.sync(&mut AutoCommit::new(), &automerge::ROOT);
            tabs.on_fullscreen_changed(true);
            tabs.finish_close(replacement);
            assert_eq!(tabs.fullscreen_intent(), Some(false));
            assert!(
                child
                    .commands
                    .lock()
                    .unwrap()
                    .contains(&WebViewCommand::ExitFullscreen)
            );
        }

        // Phase `fullscreen_hint_lifetime_is_exactly_three_seconds`.
        {
            let entered = ui::time::now();
            let deadline = entered + FULLSCREEN_HINT_DURATION;
            assert!(fullscreen_hint_visible(Some(deadline), entered));
            assert!(fullscreen_hint_visible(
                Some(deadline),
                deadline
                    .checked_sub(std::time::Duration::from_nanos(1))
                    .unwrap()
            ));
            assert!(!fullscreen_hint_visible(Some(deadline), deadline));
            assert!(!fullscreen_hint_visible(None, entered));
            assert_eq!(FULLSCREEN_HINT_DURATION, std::time::Duration::from_secs(3));
        }
    }

    /// Crash recovery prompt and clean session restore.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "keeps the four pointer/keyboard and Restore/Fresh routes in one shared scenario"
    )]
    fn crash_recovery_and_clean_restore_keep_session_state() {
        // Phase `crash_recovery_waits_for_explicit_choice_and_survives_prompt_close`.
        {
            use crate::session::{SavedTab, SessionMode, SessionStore, Snapshot};
            let dir = tempfile::tempdir().unwrap();
            let dirty = Snapshot {
                version: 1,
                tabs: vec![SavedTab {
                    url: "https://crash.test/".into(),
                    pinned: false,
                }],
                active: 0,
                clean_exit: false,
            };
            let store = SessionStore::open(dir.path(), SessionMode::Normal)
                .unwrap()
                .unwrap();
            store.save(&dirty).unwrap();
            let mut tabs = workspace().with_session(store).unwrap();
            tabs.sync(&mut AutoCommit::new(), &automerge::ROOT);
            assert!(tabs.recovery.is_some());
            drop(tabs);
            let store = SessionStore::open(dir.path(), SessionMode::Normal)
                .unwrap()
                .unwrap();
            assert_eq!(store.load().unwrap(), Some(dirty));
            let mut tabs = workspace().with_session(store).unwrap();
            // Exercise the exact action-bound button operation used by the host.
            let result = tabs
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        ui::tilo_actions::OperationKind::Effect,
                        "tilo.ui.control.invoke",
                        &serde_json::to_vec("browser.session.fresh").unwrap(),
                    ),
                    &mut AutoCommit::new(),
                    &automerge::ROOT,
                )
                .unwrap();
            assert!(result.is_some());
            tabs.sync(&mut AutoCommit::new(), &automerge::ROOT);
            assert!(tabs.recovery.is_none());
            assert_eq!(tabs.session_snapshot(false).tabs[0].url, "about:blank");
            drop(tabs);
            let store = SessionStore::open(dir.path(), SessionMode::Normal)
                .unwrap()
                .unwrap();
            let fresh = store.load().unwrap().unwrap();
            assert!(fresh.clean_exit);
            assert_eq!(fresh.tabs.len(), 1);
            assert_eq!(fresh.tabs[0].url, "about:blank");
        }

        // Phase `recovery_layout_has_workspace_bounds_and_root_identity`.
        {
            let (_dir, mut tabs) = recovery_fixture();
            let (tree, root, nodes) = recovery_layout(&mut tabs);
            assert_eq!(tabs.rect, Rect::new(0.0, 0.0, 800.0, 620.0));
            assert_eq!(unsafe { (&*nodes[&root]).name() }, tabs.name());
            assert_eq!(tree.children(root).unwrap().len(), 1);
        }

        // Phase `recovery_choices_accept_pointer_and_keyboard_through_workspace`.
        {
            for keyboard in [false, true] {
                for restore in [false, true] {
                    let (_dir, mut tabs) = recovery_fixture();
                    let (tree, root, nodes) = recovery_layout(&mut tabs);
                    let id = if restore {
                        "browser.session.restore"
                    } else {
                        "browser.session.fresh"
                    };
                    let (&button_node, &button) = nodes
                        .iter()
                        .find(|(_, w)| unsafe { (&***w).test_id() == Some(id) })
                        .unwrap();
                    let center = unsafe { (&*button).get_rect() }.center();
                    let mut doc = AutoCommit::new();
                    let mut focus = None;
                    let device_id = unsafe { std::mem::zeroed() };
                    let events = if keyboard {
                        focus = Some(button_node);
                        unsafe {
                            (&mut *button).set_focus(true);
                        }
                        let mut key: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
                        key.logical_key = Key::Named(NamedKey::Enter);
                        key.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                            winit::keyboard::NativeKeyCode::Unidentified,
                        );
                        key.location = winit::keyboard::KeyLocation::Standard;
                        key.state = ElementState::Pressed;
                        vec![WindowEvent::KeyboardInput {
                            device_id,
                            event: key,
                            is_synthetic: false,
                        }]
                    } else {
                        vec![
                            WindowEvent::CursorMoved {
                                device_id,
                                position: winit::dpi::PhysicalPosition::new(center.x, center.y),
                            },
                            WindowEvent::MouseInput {
                                device_id,
                                state: ElementState::Pressed,
                                button: winit::event::MouseButton::Left,
                            },
                            WindowEvent::MouseInput {
                                device_id,
                                state: ElementState::Released,
                                button: winit::event::MouseButton::Left,
                            },
                        ]
                    };
                    for event in events {
                        let mut handled = false;
                        tabs.handle_event(
                            &event,
                            &mut ui::widget::EventCtx {
                                scale_factor: 1.0,
                                modifiers: &winit::event::Modifiers::default(),
                                state: &mut doc,
                                path: &automerge::ROOT,
                                handled: &mut handled,
                                focus_target: &mut focus,
                                node_id: root,
                                taffy: &tree,
                            },
                        );
                        if keyboard {
                            assert!(
                                !handled,
                                "workspace must allow the host activation-key fallback"
                            );
                            // The host's unconsumed Enter path activates its focused node.
                            assert!(unsafe {
                                (&mut *nodes[&focus.unwrap()]).perform_action(
                                    &ui::a11y::Action::Activate,
                                    &mut doc,
                                    &automerge::ROOT,
                                )
                            });
                        }
                    }
                    let invocation = ui::widget::controls::take_control_invocation(&mut tabs)
                        .expect("ordinary input must emit the button's canonical action");
                    assert_eq!(invocation.address.resource, format!("tilo.control:{id}"));
                    tabs.start_operation(
                        &ui::widget::ResourceOperation::local(
                            ui::tilo_actions::InvocationId::new(),
                            ui::tilo_actions::OperationKind::Effect,
                            "tilo.ui.control.invoke",
                            &serde_json::to_vec(id).unwrap(),
                        ),
                        &mut doc,
                        &automerge::ROOT,
                    )
                    .unwrap()
                    .unwrap();
                    tabs.sync(&mut doc, &automerge::ROOT);
                    assert!(tabs.recovery.is_none());
                    assert_eq!(
                        tabs.session_snapshot(false).tabs[0].url,
                        if restore {
                            "https://restored.test/"
                        } else {
                            "about:blank"
                        }
                    );
                }
            }
        }

        // Phase `clean_session_restores_order_pin_active_and_committed_urls`.
        {
            use crate::session::{SavedTab, SessionMode, SessionStore, Snapshot};
            let dir = tempfile::tempdir().unwrap();
            let saved = Snapshot {
                version: 1,
                tabs: vec![
                    SavedTab {
                        url: "https://alpha.test/".into(),
                        pinned: true,
                    },
                    SavedTab {
                        url: "https://beta.test/second".into(),
                        pinned: false,
                    },
                    SavedTab {
                        url: "https://gamma.test/".into(),
                        pinned: false,
                    },
                ],
                active: 1,
                clean_exit: true,
            };
            let store = SessionStore::open(dir.path(), SessionMode::Normal)
                .unwrap()
                .unwrap();
            store.save(&saved).unwrap();
            let mut tabs = workspace().with_session(store).unwrap();
            assert_eq!(tabs.session_snapshot(true), saved);
            assert_eq!(tabs.tabs.len(), 3);
            tabs.active_mut()
                .buffer_tab_address(&Key::Character("draft must not persist".into()), true);
            assert_eq!(tabs.session_snapshot(true), saved);
            tabs.close(tabs.tabs[2].id);
            drop(tabs);
            let store = SessionStore::open(dir.path(), SessionMode::Normal)
                .unwrap()
                .unwrap();
            let restored = store.load().unwrap().unwrap();
            assert!(restored.clean_exit);
            assert_eq!(restored.tabs.len(), 2);
        }
    }

    fn recovery_fixture() -> (tempfile::TempDir, BrowserTabs) {
        use crate::session::{SavedTab, SessionMode, SessionStore, Snapshot};
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), SessionMode::Normal)
            .unwrap()
            .unwrap();
        store
            .save(&Snapshot {
                version: 1,
                tabs: vec![SavedTab {
                    url: "https://restored.test/".into(),
                    pinned: true,
                }],
                active: 0,
                clean_exit: false,
            })
            .unwrap();
        (dir, workspace().with_session(store).unwrap())
    }

    fn recovery_layout(
        tabs: &mut BrowserTabs,
    ) -> (TaffyTree<()>, NodeId, HashMap<NodeId, *mut dyn Widget>) {
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = tabs.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        tree.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(800.0),
                height: taffy::AvailableSpace::Definite(620.0),
            },
        )
        .unwrap();
        tabs.apply_layout(&tree, root, Point::ORIGIN);
        (tree, root, nodes)
    }

    /// Close admission and download-exit lifecycle.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "keeps the local engine fixture and complete download-exit lifecycle together"
    )]
    fn tab_and_window_close_wait_for_native_owners_and_explicit_download_choice() {
        // Phase `native_close_admission_keeps_tab_visible_until_closed_event`.
        {
            struct ClosingEngine {
                page: Arc<TestEngine>,
                events: std::sync::Mutex<Vec<WebViewEvent>>,
            }
            impl WebViewEngine for ClosingEngine {
                fn create_tab(&self) -> Result<SharedEngine, String> {
                    Ok(Arc::new(Engine(TestEngine::new())))
                }
                fn send(&self, command: WebViewCommand) {
                    self.page.send(command);
                }
                fn frames(&self) -> FrameSlot {
                    self.page.frames()
                }
                fn drain_events(&self) -> Vec<WebViewEvent> {
                    std::mem::take(&mut *self.events.lock().unwrap())
                }
                fn request_close(&self) -> bool {
                    false
                }
            }
            let engine = Arc::new(ClosingEngine {
                page: TestEngine::new(),
                events: std::sync::Mutex::default(),
            });
            let mut tabs = BrowserTabs::new(BrowserWidget::with_engine(engine.clone(), true));
            let mut doc = AutoCommit::new();
            tabs.bar.start_closing(1);
            tabs.sync(&mut doc, &automerge::ROOT);
            assert_eq!(tabs.bar.tabs().len(), 1);
            assert_eq!(tabs.bar.active_id(), 1);
            assert!(!tabs.bar.has_closing());
            assert_eq!(tabs.tabs.len(), 1);
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::TabCloseBlocked);
            tabs.sync(&mut doc, &automerge::ROOT);
            assert_eq!(tabs.active_id, 1);
            assert!(tabs.retired.is_empty());
            tabs.close(1);
            engine.events.lock().unwrap().push(WebViewEvent::TabClosed);
            tabs.sync(&mut doc, &automerge::ROOT);
            assert_eq!(tabs.tabs.len(), 1);
            assert_ne!(tabs.active_id, 1);
        }

        // Phase `fullscreen_download_close_exposes_both_exit_choices`.
        {
            let (mut tabs, engine) = fullscreen_workspace();
            let mut doc = AutoCommit::new();
            engine.emit(WebViewEvent::FullscreenChanged(true));
            engine.emit(WebViewEvent::DownloadRuntime {
                admitted: 1,
                detached: 0,
                accepting: true,
                error: String::new(),
            });
            tabs.sync(&mut doc, &automerge::ROOT);
            tabs.on_fullscreen_changed(true);
            let _immersive_layout = retained_layout(&mut tabs);
            assert_eq!(tabs.fullscreen_intent(), Some(true));

            assert_eq!(
                ui::component_lifecycle::on_window_close_requested(&mut tabs),
                WindowCloseAdmission::Defer
            );
            assert_eq!(tabs.fullscreen_intent(), Some(false));
            assert!(tabs.fullscreen_hint.is_none());
            assert!(tabs.take_structure_dirty());
            let rects = layout_rects(&mut tabs);
            for id in ["browser.download-exit.keep", "browser.download-exit.cancel"] {
                let rect = rects.get(id).expect("visible fullscreen exit choice");
                assert!(rect.width() > 0.0 && rect.height() > 0.0);
            }
            let commands = engine.commands.lock().unwrap();
            assert!(commands.contains(&WebViewCommand::ExitFullscreen));
            assert!(!commands.contains(&WebViewCommand::CancelAllDownloads));
            drop(commands);
            assert!(!ui::component_lifecycle::take_window_close_intent(
                &mut tabs
            ));
            invoke_exit_choice(&mut tabs, "browser.download-exit.keep", &mut doc);
            tabs.sync(&mut doc, &automerge::ROOT);
            assert!(tabs.exit_panel.is_none());
            assert!(!ui::component_lifecycle::take_window_close_intent(
                &mut tabs
            ));
            assert!(
                !engine
                    .commands
                    .lock()
                    .unwrap()
                    .contains(&WebViewCommand::CancelAllDownloads)
            );
        }

        // Phase `download_exit_flow_rejects_late_fullscreen_entry`.
        {
            for cancelling in [false, true] {
                let (mut tabs, engine) = fullscreen_workspace();
                let mut doc = AutoCommit::new();
                engine.emit(WebViewEvent::FullscreenChanged(true));
                engine.emit(WebViewEvent::DownloadRuntime {
                    admitted: 1,
                    detached: 0,
                    accepting: true,
                    error: String::new(),
                });
                tabs.sync(&mut doc, &automerge::ROOT);
                tabs.on_fullscreen_changed(true);
                assert_eq!(
                    ui::component_lifecycle::on_window_close_requested(&mut tabs),
                    WindowCloseAdmission::Defer
                );
                if cancelling {
                    invoke_exit_choice(&mut tabs, "browser.download-exit.cancel", &mut doc);
                    tabs.sync(&mut doc, &automerge::ROOT);
                }
                engine.emit(WebViewEvent::FullscreenChanged(true));
                tabs.sync(&mut doc, &automerge::ROOT);
                assert_eq!(
                    tabs.fullscreen_intent(),
                    Some(false),
                    "cancelling={cancelling}"
                );
                let rects = layout_rects(&mut tabs);
                assert!(tabs.exit_panel.as_ref().unwrap().get_rect().height() > 0.0);
                if !cancelling {
                    assert!(rects.contains_key("browser.download-exit.keep"));
                    assert!(rects.contains_key("browser.download-exit.cancel"));
                }
                assert!(!ui::component_lifecycle::take_window_close_intent(
                    &mut tabs
                ));
            }
        }

        // Phase `window_close_requires_an_explicit_choice_and_waits_for_native_owner_cleanup`.
        {
            struct ExitEngine {
                commands: std::sync::Mutex<Vec<WebViewCommand>>,
                events: std::sync::Mutex<Vec<WebViewEvent>>,
                frames: FrameSlot,
            }
            impl WebViewEngine for ExitEngine {
                fn create_tab(&self) -> Result<SharedEngine, String> {
                    Err("unused".into())
                }
                fn send(&self, command: WebViewCommand) {
                    self.commands.lock().unwrap().push(command);
                }
                fn frames(&self) -> FrameSlot {
                    self.frames.clone()
                }
                fn drain_events(&self) -> Vec<WebViewEvent> {
                    std::mem::take(&mut *self.events.lock().unwrap())
                }
            }
            let engine = Arc::new(ExitEngine {
                commands: std::sync::Mutex::default(),
                events: std::sync::Mutex::new(vec![WebViewEvent::DownloadUpdated {
                    id: 11,
                    state: "progress".into(),
                    path: "/tmp/file".into(),
                    name: "file".into(),
                    url: "https://downloads.test/file".into(),
                    retryable: true,
                    received: 10,
                    total: 20,
                    error: String::new(),
                }]),
                frames: Arc::new(std::sync::Mutex::new(None)),
            });
            let mut tabs = BrowserTabs::new(BrowserWidget::with_engine(engine.clone(), true));
            let mut doc = AutoCommit::new();
            tabs.sync(&mut doc, &automerge::ROOT);
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::DownloadRuntime {
                    admitted: 1,
                    detached: 0,
                    accepting: true,
                    error: String::new(),
                });
            tabs.sync(&mut doc, &automerge::ROOT);

            assert_eq!(
                ui::component_lifecycle::on_window_close_requested(&mut tabs),
                ui::widget::WindowCloseAdmission::Defer
            );
            assert!(tabs.exit_panel.is_some(), "both exit choices are visible");

            invoke_exit_choice(&mut tabs, "browser.download-exit.keep", &mut doc);
            tabs.sync(&mut doc, &automerge::ROOT);
            assert!(!ui::component_lifecycle::take_window_close_intent(
                &mut tabs
            ));
            assert!(
                !engine
                    .commands
                    .lock()
                    .unwrap()
                    .contains(&WebViewCommand::CancelAllDownloads)
            );
            assert_eq!(
                ui::component_lifecycle::on_window_close_requested(&mut tabs),
                ui::widget::WindowCloseAdmission::Defer,
                "keeping downloads does not arm a delayed automatic quit"
            );

            engine.events.lock().unwrap().extend([
                WebViewEvent::DownloadUpdated {
                    id: 11,
                    state: "complete".into(),
                    path: "/tmp/file".into(),
                    name: String::new(),
                    url: String::new(),
                    retryable: false,
                    received: 20,
                    total: 20,
                    error: String::new(),
                },
                WebViewEvent::DownloadRuntime {
                    admitted: 0,
                    detached: 0,
                    accepting: true,
                    error: String::new(),
                },
            ]);
            tabs.sync(&mut doc, &automerge::ROOT);
            invoke_exit_choice(&mut tabs, "browser.download-exit.cancel", &mut doc);
            tabs.sync(&mut doc, &automerge::ROOT);
            assert!(
                engine
                    .commands
                    .lock()
                    .unwrap()
                    .contains(&WebViewCommand::CancelAllDownloads)
            );
            assert!(!ui::component_lifecycle::take_window_close_intent(
                &mut tabs
            ));
            assert_eq!(
                ui::component_lifecycle::on_window_close_requested(&mut tabs),
                ui::widget::WindowCloseAdmission::Defer,
                "repeated close cannot bypass native cancellation"
            );

            engine.events.lock().unwrap().extend([
                WebViewEvent::DownloadUpdated {
                    id: 12,
                    state: "progress".into(),
                    path: "/tmp/new".into(),
                    name: "new".into(),
                    url: "https://downloads.test/new".into(),
                    retryable: true,
                    received: 1,
                    total: 2,
                    error: String::new(),
                },
                WebViewEvent::DownloadRuntime {
                    admitted: 1,
                    detached: 0,
                    accepting: true,
                    error: String::new(),
                },
            ]);
            tabs.sync(&mut doc, &automerge::ROOT);
            assert!(
                !ui::component_lifecycle::take_window_close_intent(&mut tabs),
                "new admission precedes cancel barrier"
            );
            engine.events.lock().unwrap().extend([
                WebViewEvent::DownloadUpdated {
                    id: 12,
                    state: "cancelled".into(),
                    path: String::new(),
                    name: String::new(),
                    url: String::new(),
                    retryable: false,
                    received: 0,
                    total: 0,
                    error: String::new(),
                },
                WebViewEvent::DownloadRuntime {
                    admitted: 0,
                    detached: 1,
                    accepting: false,
                    error: String::new(),
                },
            ]);
            tabs.sync(&mut doc, &automerge::ROOT);
            assert!(
                !ui::component_lifecycle::take_window_close_intent(&mut tabs),
                "physical owner is still closing"
            );
            engine
                .events
                .lock()
                .unwrap()
                .push(WebViewEvent::DownloadRuntime {
                    admitted: 0,
                    detached: 0,
                    accepting: false,
                    error: String::new(),
                });
            tabs.sync(&mut doc, &automerge::ROOT);
            assert!(ui::component_lifecycle::take_window_close_intent(&mut tabs));
            assert!(
                !ui::component_lifecycle::take_window_close_intent(&mut tabs),
                "approval is one shot"
            );
        }

        // Phase `admitted_close_releases_cached_favicon_and_visual_state`.
        {
            let mut tabs = workspace();
            for _ in 0..12 {
                tabs.open(Some("https://icon.test/".into()), false).unwrap();
                let id = tabs.active_id;
                tabs.bar.set_fixture_visual(id, None, true, true, false);
                tabs.close(id);
                assert!(tabs.bar.visual_count() <= tabs.tabs.len());
            }
        }
    }

    fn invoke_exit_choice(tabs: &mut BrowserTabs, id: &str, doc: &mut AutoCommit) {
        let (_tree, _root, nodes) = recovery_layout(tabs);
        let &button = nodes
            .values()
            .find(|widget| unsafe { (&***widget).test_id() == Some(id) })
            .expect("exit choice button");
        assert!(unsafe {
            (&mut *button).perform_action(&ui::a11y::Action::Activate, doc, &automerge::ROOT)
        });
        let invocation = ui::widget::controls::take_control_invocation(tabs)
            .expect("exit choice must reach the root action broker");
        assert_eq!(invocation.address.resource, format!("tilo.control:{id}"));
        assert!(
            tabs.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    ui::tilo_actions::OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec(id).unwrap()
                ),
                doc,
                &automerge::ROOT
            )
            .unwrap()
            .is_some()
        );
    }
    /// Accessibility tree and tab intents of the workspace.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn workspace_accessibility_exports_and_applies_tab_intents() {
        // Phase `accessibility_tab_intents_are_applied_without_an_extra_pointer_event`.
        {
            let mut tabs = workspace();
            let mut doc = AutoCommit::new();
            assert!(tabs.bar.perform_semantic_action(
                "tab-new",
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            tabs.sync(&mut doc, &automerge::ROOT);
            assert_eq!(tabs.tabs.len(), 2);
            assert!(tabs.bar.perform_semantic_action(
                "tab-close:2",
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            tabs.sync(&mut doc, &automerge::ROOT);
            assert_eq!(tabs.tabs.len(), 1);
        }

        // Phase `accessibility_exports_tabs_new_tab_and_open_context_menu_actions`.
        {
            use ui::a11y::Role;
            let mut tabs = workspace();
            tabs.open(Some("https://two.test".into()), false).unwrap();
            let strip = tabs.bar.accessibility().expect("tab strip semantics");
            assert_eq!(strip.role, Role::TabBar);
            assert!(strip.children.iter().any(|node| node.role == Role::Tab));
            assert!(strip.children.iter().any(|node| {
                node.role == Role::Button && node.name.as_deref() == Some("New tab")
            }));

            tabs.open_tab_menu(2, Point::new(20.0, 20.0));
            let workspace = tabs.accessibility().expect("workspace semantics");
            let menu = workspace
                .children
                .iter()
                .find(|node| node.role == Role::Menu)
                .expect("open tab menu");
            assert!(menu.children.iter().any(|node| {
                node.role == Role::MenuItem && node.name.as_deref() == Some("Mute")
            }));
            let mute_target = menu
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Mute"))
                .and_then(|node| node.action_target.clone())
                .expect("mute action route");
            let mut doc = AutoCommit::new();
            assert!(tabs.perform_semantic_action(
                &mute_target,
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert!(
                tabs.tabs
                    .iter()
                    .find(|tab| tab.id == 2)
                    .unwrap()
                    .widget
                    .tab_muted()
            );
        }

        // Phase `composed_accesskit_tree_accepts_workspace_and_retained_strip_on_first_update`.
        {
            let mut tabs = workspace();
            tabs.open(Some("https://two.test".into()), false).unwrap();
            let mut tree = TaffyTree::new();
            let mut nodes = HashMap::new();
            let root = tabs.build_layout(&mut tree, &Theme::dark(), &mut nodes);
            tree.compute_layout(
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(620.0),
                },
            )
            .unwrap();
            tabs.apply_layout(&tree, root, Point::ORIGIN);
            let update = ui::accesskit_bridge::build_tree_update(&tabs, &nodes, None, 1.0);
            let _consumer = accesskit_consumer::Tree::new(update, true);
        }

        // Phase `workspace_spinner_respects_loading_and_reduced_motion`.
        {
            let mut tabs = workspace();
            tabs.bar.set_fixture_visual(1, None, true, false, false);
            Widget::set_motion_reduced(&mut tabs, false);
            assert!(Widget::needs_animation_frame(&tabs));
            Widget::set_motion_reduced(&mut tabs, true);
            assert!(!Widget::needs_animation_frame(&tabs));
        }
    }

    /// Switching, reordering, closing, pinning and tab limits.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "checks interactions between pinning, tab actions and overflow in one retained workspace"
    )]
    fn tab_management_keeps_documents_identity_and_a_usable_workspace() {
        // Phase `switching_retains_independent_documents_and_reorder_keeps_identity`.
        {
            let mut tabs = workspace();
            let first = tabs.active_id;
            let first_engine = tabs.active().engine_handle();
            tabs.open(Some("https://two.test".into()), false).unwrap();
            let second = tabs.active_id;
            assert_ne!(first, second);
            assert!(!Arc::ptr_eq(&first_engine, &tabs.active().engine_handle()));
            tabs.activate(first);
            assert!(Arc::ptr_eq(&first_engine, &tabs.active().engine_handle()));
            tabs.reorder(0, 1);
            assert_eq!(tabs.active_id, first);
            assert_eq!(tabs.tabs[1].id, first);
        }

        // Phase `reorder_keeps_layout_widget_addresses_stable`.
        {
            let mut tabs = workspace();
            tabs.open(None, true).unwrap();
            let before = std::ptr::from_ref(tabs.active());
            tabs.reorder(0, 1);
            assert_eq!(std::ptr::from_ref(tabs.active()), before);
        }

        // Phase `close_reopen_and_last_tab_preserve_a_usable_workspace`.
        {
            let mut tabs = workspace();
            tabs.open(Some("https://two.test".into()), false).unwrap();
            let closed = tabs.active_id;
            tabs.close(closed);
            assert_eq!(tabs.tabs.len(), 1);
            tabs.reopen().unwrap();
            assert_eq!(tabs.tabs.len(), 2);
            assert_ne!(tabs.active_id, closed);
            assert_eq!(tabs.active().tab_url(), "https://two.test");
            let ids: Vec<_> = tabs.tabs.iter().map(|tab| tab.id).collect();
            for id in ids {
                tabs.close(id);
            }
            assert_eq!(tabs.tabs.len(), 1);
        }

        // Phase `background_tabs_do_not_steal_selection_and_limit_is_bounded`.
        {
            let mut tabs = workspace();
            let first = tabs.active_id;
            for _ in 1..32 {
                tabs.open(None, true).unwrap();
            }
            assert_eq!(tabs.active_id, first);
            assert!(tabs.open(None, true).is_err());
            assert_eq!(tabs.tabs.len(), 32);
        }

        // Phase `pin_partition_duplicate_close_others_mute_and_overflow`.
        {
            let mut tabs = workspace();
            tabs.open(Some("https://one.test".into()), false).unwrap();
            let one = tabs.active_id;
            tabs.open(Some("https://two.test".into()), false).unwrap();
            let two = tabs.active_id;
            tabs.set_pinned(two, true);
            assert_eq!(
                tabs.tabs[0].id, two,
                "pinned tabs form the leading partition"
            );
            tabs.duplicate(one).unwrap();
            assert_eq!(tabs.active().tab_url(), "https://one.test");
            tabs.set_muted(two, true);
            assert!(
                tabs.tabs
                    .iter()
                    .find(|tab| tab.id == two)
                    .unwrap()
                    .widget
                    .tab_muted()
            );
            tabs.close_others(one);
            assert!(tabs.tabs.iter().any(|tab| tab.id == one));
            assert!(
                tabs.tabs.iter().any(|tab| tab.id == two),
                "pinned tabs are protected"
            );
            for _ in tabs.tabs.len()..32 {
                tabs.open(None, true).unwrap();
            }
            tabs.activate(*tabs.tabs.last().map(|tab| &tab.id).unwrap());
            tabs.bar.debug_layout(800.0);
            assert!(tabs.bar.overflow_targets_meet_minimum_size());
            let semantics = tabs.bar.accessibility().expect("overflow semantics");
            assert!(
                semantics
                    .children
                    .iter()
                    .any(|node| node.action_target.as_deref() == Some("tab-overflow-before"))
            );
            assert!(
                semantics
                    .children
                    .iter()
                    .any(|node| node.action_target.as_deref() == Some("tab-overflow-after"))
            );
            let previous = semantics
                .children
                .iter()
                .find(|node| node.action_target.as_deref() == Some("tab-overflow-before"))
                .expect("previous overflow control");
            assert!(
                !previous.state.disabled,
                "pinned prefix must not hide earlier normal tabs"
            );
            let before = tabs.bar.active_id();
            let mut doc = AutoCommit::new();
            assert!(tabs.bar.perform_semantic_action(
                "tab-overflow-before",
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_ne!(tabs.bar.active_id(), before);
            tabs.bar.set_active(before);
            let mut tree = TaffyTree::new();
            let node = tabs
                .bar
                .build_layout(&mut tree, &Theme::dark(), &mut HashMap::new());
            tree.set_style(
                node,
                Style {
                    size: Size {
                        width: Dimension::length(800.0),
                        height: Dimension::length(40.0),
                    },
                    ..Style::default()
                },
            )
            .unwrap();
            tree.compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
            )
            .unwrap();
            tabs.bar.apply_layout(&tree, node, Point::ORIGIN);
            assert!(tabs.bar.active_is_visible());
            assert!(
                tabs.bar
                    .active_rect()
                    .is_some_and(|rect| rect.width() >= 118.0 && rect.contains(rect.center())),
                "active overflow tab is actually drawn and hittable"
            );
            assert!(
                tabs.bar.new_tab_rect().width() >= 28.0,
                "new-tab control remains fixed and hittable"
            );
            assert!(
                tabs.bar.controls_are_outside_tabs(),
                "overflow buttons and new-tab control use reserved geometry outside tab pills"
            );
            let visible_media = tabs.bar.visible_ids()[2];
            tabs.bar
                .set_fixture_visual(visible_media, None, false, true, false);
            assert!(
                tabs.bar.media_control_precedes_title(visible_media),
                "media control occupies the reserved leading glyph slot before title text"
            );
            let all_ids: Vec<_> = tabs.tabs.iter().map(|tab| tab.id).collect();
            for id in &all_ids {
                tabs.bar.set_pinned(*id, true);
            }
            let last = *all_ids.last().unwrap();
            tabs.bar.set_active(last);
            tabs.bar.debug_layout(800.0);
            assert!(
                tabs.bar.visible_ids().len() <= 17 && tabs.bar.visible_ids().contains(&last),
                "all-pinned overflow stays bounded and retains active"
            );
            let before = tabs.bar.active_id();
            tabs.bar.overflow_step(false);
            assert_ne!(tabs.bar.active_id(), before);
            assert!(
                tabs.bar.active_is_visible(),
                "pointer scroll path reaches and reveals hidden pinned tabs"
            );
        }

        // Phase `resources_are_owned_only_by_the_page`.
        {
            let tabs = workspace();
            assert!(ui::resources::resources(&tabs, &AutoCommit::new()).is_empty());
            assert!(!ui::resources::resources(tabs.active(), &AutoCommit::new()).is_empty());
        }
    }
    /// New tab typing, focus and stale layout nodes.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn new_tab_input_survives_before_layout_and_rebuilds() {
        // Phase `immediate_new_tab_typing_and_enter_survive_before_first_layout`.
        {
            fn address(widget: &dyn Widget) -> Option<(String, String)> {
                if widget.test_id() == Some("browser.url_input") {
                    return widget
                        .text_review()
                        .map(|(_, text)| (text, ui::undo::get_state(widget).unwrap()));
                }
                widget.children().iter().find_map(|child| address(*child))
            }

            let mut tabs = workspace();
            let mut tree = TaffyTree::new();
            let root = tabs.build_layout(&mut tree, &Theme::dark(), &mut HashMap::new());
            tabs.open(None, false).unwrap();
            let mut doc = AutoCommit::new();
            let url = "http://127.0.0.1:63921/";
            let keys = url
                .chars()
                .map(|c| Key::Character(c.to_string().into()))
                .chain(std::iter::once(Key::Named(NamedKey::Enter)));
            for logical_key in keys {
                let mut key: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
                key.logical_key = logical_key;
                key.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                );
                key.location = winit::keyboard::KeyLocation::Standard;
                key.state = ElementState::Pressed;
                tabs.handle_event(
                    &WindowEvent::KeyboardInput {
                        device_id: unsafe { std::mem::zeroed() },
                        event: key,
                        is_synthetic: false,
                    },
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &winit::event::Modifiers::default(),
                        state: &mut doc,
                        path: &automerge::ROOT,
                        handled: &mut false,
                        focus_target: &mut None,
                        node_id: root,
                        taffy: &tree,
                    },
                );
            }
            assert!(
                ui::widget::controls::take_control_invocation(tabs.active_mut()).is_some(),
                "Enter must queue the complete address even before layout"
            );
            let mut new_tree = TaffyTree::new();
            tabs.build_layout(&mut new_tree, &Theme::dark(), &mut HashMap::new());
            let (text, caret) = address(&tabs).unwrap();
            assert_eq!(text, url);
            assert_eq!(
                caret,
                url.len().to_string(),
                "buffered prefix must not be selected and replaced by remaining typing"
            );
        }

        // Phase `new_tab_address_is_focused_after_layout`.
        {
            fn focused(widget: &dyn Widget) -> bool {
                (widget.test_id() == Some("browser.url_input") && widget.is_focused())
                    || widget.children().iter().any(|child| focused(*child))
            }

            let mut tabs = workspace();
            tabs.open(None, false).unwrap();
            let mut doc = AutoCommit::new();
            tabs.sync(&mut doc, &automerge::ROOT);
            let mut tree = TaffyTree::new();
            tabs.build_layout(&mut tree, &Theme::dark(), &mut HashMap::new());
            assert!(focused(&tabs));
        }

        // Phase `switching_before_rebuild_does_not_dispatch_stale_layout_nodes`.
        {
            let mut tabs = workspace();
            let mut tree = TaffyTree::new();
            for _ in 0..128 {
                tree.new_leaf(Style::default()).unwrap();
            }
            tabs.build_layout(&mut tree, &Theme::dark(), &mut HashMap::new());
            tabs.open(None, false).unwrap();
            let mut tree = TaffyTree::new();
            let root = tabs.build_layout(&mut tree, &Theme::dark(), &mut HashMap::new());
            tabs.activate(1);
            tabs.handle_event(
                &WindowEvent::Focused(true),
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: &mut AutoCommit::new(),
                    path: &automerge::ROOT,
                    handled: &mut false,
                    focus_target: &mut None,
                    node_id: root,
                    taffy: &tree,
                },
            );
        }
    }

    /// Background navigation, picture-in-picture and shared history pause.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn background_tabs_keep_shared_state_and_invocation_routes() {
        // Phase `history_pause_is_shared_by_existing_and_new_tabs`.
        {
            let mut tabs = workspace();
            tabs.open(None, true).unwrap();
            tabs.active_mut().debug_show_history_paused();
            assert!(tabs.tabs.iter().all(|tab| tab.widget.history_paused()));
            tabs.open(None, false).unwrap();
            assert!(tabs.active().history_paused());
        }

        // Phase `background_navigation_keeps_its_invocation_route`.
        {
            let mut tabs = workspace();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            tabs.sync(&mut doc, &root);
            let resources = ui::resources::resources(tabs.active(), &doc);
            let page = resources[0]
                .children
                .iter()
                .find(|r| r.id.0 == "browser.page")
                .unwrap();
            let navigate = page
                .actions
                .iter()
                .find(|action| action.id == "navigate")
                .unwrap();
            let args =
                serde_json::from_value(serde_json::json!({"url": "https://one.test"})).unwrap();
            let encoded = navigate.build_json(&args).unwrap();
            let invocation = ui::tilo_actions::InvocationId::new();
            tabs.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    navigate.kind,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut doc,
                &root,
            )
            .unwrap();
            tabs.open(None, false).unwrap();
            tabs.sync(&mut doc, &root);
            assert!(matches!(
                tabs.poll_resource_operation(invocation, false, &doc, &root),
                Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }

        // Phase `pip_host_adoption_and_close_route_to_original_background_tab`.
        {
            let mut tabs = workspace();
            let first = tabs.active_id;
            tabs.active_mut().debug_show_active_pip();
            let id = tabs.active().active_floating_surface_ids()[0].clone();
            tabs.open(None, false).unwrap();
            assert_ne!(tabs.active_id, first);
            tabs.on_floating_surface_host_message(
                &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                    id: id.clone(),
                    rect: [0.0, 0.0, 400.0, 300.0],
                },
            );
            assert!(tabs.active().active_floating_surface_ids().is_empty());
            assert_eq!(tabs.active_floating_surface_ids(), vec![id.clone()]);
            tabs.active_mut().debug_show_active_pip();
            let second = tabs.active().active_floating_surface_ids()[0].clone();
            assert_ne!(id, second);
            tabs.on_floating_surface_host_message(
                &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id },
            );
            assert_eq!(tabs.active_floating_surface_ids(), vec![second]);
        }

        // Phase `background_pip_delivers_request_and_completes_invocation`.
        {
            let engine = TestEngine::new();
            engine.set_media_element(Some(ui::webview::MediaElementInfo {
                element_id: "media:one".into(),
                viewport_rect: [0.1, 0.1, 0.8, 0.8],
                intrinsic_size: [1920, 1080],
                label: "Video".into(),
            }));
            let mut tabs =
                BrowserTabs::new(BrowserWidget::with_engine(Arc::new(Engine(engine)), true));
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            tabs.sync(&mut doc, &root);
            let mut tree = TaffyTree::new();
            let node = tree
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(900.0),
                        height: Dimension::length(600.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            tree.compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::MaxContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .unwrap();
            tabs.active_mut().apply_layout(&tree, node, Point::ORIGIN);
            let resources = ui::resources::resources(tabs.active(), &doc);
            let page = resources[0]
                .children
                .iter()
                .find(|r| r.id.0 == "browser.page")
                .unwrap();
            let pip = page
                .actions
                .iter()
                .find(|action| action.id == "picture-in-picture")
                .unwrap();
            let invocation = ui::tilo_actions::InvocationId::new();
            tabs.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    pip.kind,
                    pip.operation_type.as_deref().unwrap(),
                    br#"{"operation":"toggle-picture-in-picture"}"#,
                ),
                &mut doc,
                &root,
            )
            .unwrap();
            tabs.open(None, false).unwrap();
            tabs.sync(&mut doc, &root);
            let request = tabs
                .take_floating_surface_request()
                .expect("background tab request is delivered");
            assert_eq!(request.id, "browser.media-pip.1");
            assert!(matches!(
                tabs.poll_resource_operation(invocation, false, &doc, &root),
                Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }
    }
}

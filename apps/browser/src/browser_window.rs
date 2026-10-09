//! The Browser window: one page with its chrome, and the window-level states
//! around it — content full screen and the choice to keep or cancel
//! downloads when the window closes.
//!
//! A window shows exactly one page. Another page is another Browser instance
//! in a tab of the desktop's window group: a link opened in a new tab, a
//! page's `window.open`, Ctrl+T, the "Open in new tab" action — all ask the
//! desktop through [`ui::shell_tabs`], and reopening, reordering, dragging and
//! restoring those tabs is the desktop's, not the Browser's.

use crate::BrowserWidget;
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::style_helpers::{auto, length};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
#[cfg(test)]
use ui::widget::Edits;
use ui::widget::{
    AppKind, AppWidget, Continuable, FloatingSurfaceProvider, FullscreenProvider, IconDescriptor,
    TaffyTreeExt, Widget, WindowCloseAdmission,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExitState {
    Normal,
    Choice,
    Cancelling,
    Ready,
}

/// The root of a Browser window.
#[allow(
    clippy::struct_excessive_bools,
    reason = "layout invalidation, address focus, close admission and native fullscreen observations are independent flags; exit admission already uses ExitState"
)]
pub struct BrowserWindow {
    page: Box<BrowserWidget>,
    rect: Rect,
    dirty: bool,
    /// A window opened without a page starts in the address field.
    focus_address: bool,
    address_focus_node: Option<NodeId>,
    layout_immersive: Option<bool>,
    exit_state: ExitState,
    exit_choice: ui::LocalState<Option<bool>>,
    exit_panel: Option<Box<dyn Widget>>,
    close_intent: bool,
    window_fullscreen_active: bool,
    observed_content_fullscreen: bool,
    fullscreen_hint_deadline: Option<Instant>,
    fullscreen_hint: Option<Box<dyn Widget>>,
}

impl BrowserWindow {
    /// A window showing `page`. `focus_address` puts the caret in the
    /// address field, for a window opened without a page to show.
    pub fn new(page: BrowserWidget, focus_address: bool) -> Self {
        Self {
            page: Box::new(page),
            rect: Rect::ZERO,
            dirty: true,
            focus_address,
            address_focus_node: None,
            layout_immersive: None,
            exit_state: ExitState::Normal,
            exit_choice: ui::LocalState::new(None),
            exit_panel: None,
            close_intent: false,
            window_fullscreen_active: false,
            observed_content_fullscreen: false,
            fullscreen_hint_deadline: None,
            fullscreen_hint: None,
        }
    }

    pub fn engine_handle(&self) -> ui::webview::SharedEngine {
        self.page.engine_handle()
    }

    /// Present the document's bookmarks, preferences and site exceptions.
    pub(crate) fn present_state(&mut self, state: &crate::resource::BrowserState) {
        self.page.present_state(state);
    }

    /// Submit the page's bookmark and preference edits through `dispatcher`.
    pub(crate) fn attach_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Browser>,
    ) {
        self.page.attach_dispatcher(dispatcher);
    }

    /// Keep the page's downloads in this device's settings.
    pub(crate) fn attach_downloads(
        &mut self,
        store: ui::storage::Settings<crate::downloads::Stored>,
    ) {
        self.page.attach_downloads(store);
    }

    /// Keep the page's visits in the person's browsing history.
    pub(crate) fn attach_history(
        &mut self,
        record: ui::framework::PersonRecord<crate::history::BrowsingHistory>,
    ) {
        self.page.attach_history(record);
    }

    /// Fill the page's forms from the person's address book.
    pub(crate) fn attach_address_book(
        &mut self,
        book: ui::framework::PersonRecord<crate::autofill::AddressBook>,
    ) {
        self.page.attach_address_book(book);
    }

    /// The page this window shows.
    pub(crate) fn page(&self) -> &BrowserWidget {
        &self.page
    }

    /// The page this window shows, to act on.
    pub(crate) fn page_mut(&mut self) -> &mut BrowserWidget {
        &mut self.page
    }

    /// Stage a screenshot scenario on the page.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.page.stage(scenario);
        self.dirty = true;
    }

    fn immersive(&self) -> bool {
        self.window_fullscreen_active && self.page.content_fullscreen()
    }

    fn make_fullscreen_hint() -> Box<dyn Widget> {
        use ui::a11y::Politeness;
        use ui::widget::{Container, Text};
        // Theme-driven: the hint pill takes the theme's own surface and body
        // ink, so it reads on every palette (HIG §7 theme-driven).
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
                                .with_role(ui::tokens::TextRole::Body),
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

    fn reconcile_content_fullscreen(&mut self) {
        let content_fullscreen = self.page.content_fullscreen();
        if content_fullscreen == self.observed_content_fullscreen {
            return;
        }
        self.observed_content_fullscreen = content_fullscreen;
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
            .page
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
                .with_role(ui::tokens::TextRole::Heading),
            ))
            .push(Box::new(
                Text::new(&status).with_role(ui::tokens::TextRole::Caption),
            ));
        if !cancelling {
            let keep = self.exit_choice.writer();
            let cancel = self.exit_choice.writer();
            column = column.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new("browser.download-exit.keep", "Keep downloading")
                            .on_effect(move || keep.set(Some(false))),
                    ))
                    .push(Box::new(
                        Button::new("browser.download-exit.cancel", "Cancel downloads and quit")
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

    /// Ctrl+T, Ctrl+W and Ctrl+Shift+T are the desktop's tab chords. The
    /// page never sees them, so a site cannot keep a person in its tab.
    fn is_desktop_tab_chord(
        event: &WindowEvent,
        modifiers: winit::keyboard::ModifiersState,
    ) -> bool {
        matches!(event, WindowEvent::KeyboardInput { event: key, .. }
            if key.state == ElementState::Pressed
                && ui::shell_tabs::chord_request(
                    &key.logical_key,
                    modifiers.control_key() || modifiers.super_key(),
                    modifiers.shift_key(),
                    modifiers.alt_key(),
                )
                .is_some())
    }

    fn shortcut(&self, key: &Key, modifiers: winit::keyboard::ModifiersState) -> bool {
        let platform = modifiers.super_key() || modifiers.control_key();
        match key {
            Key::Character(c)
                if platform && modifiers.shift_key() && c.eq_ignore_ascii_case("n") =>
            {
                self.page.request_private_window();
                true
            }
            _ => false,
        }
    }
}

impl Widget for BrowserWindow {
    ui::provides!(
        dyn ui::widget::FullscreenProvider,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::widget::Continuable,
        dyn ui::application_view::ApplicationViewState,
        dyn ui::widget::controls::ActionControls,
        dyn ui::component_lifecycle::ComponentLifecycle
    );
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.fullscreen_hint_deadline.is_some()
            && !fullscreen_hint_visible(self.fullscreen_hint_deadline, ui::time::now())
        {
            self.clear_fullscreen_hint();
            self.dirty = true;
        }
        if let Some(cancel) = self.exit_choice.get() {
            self.exit_choice.set(None);
            if cancel {
                self.exit_state = ExitState::Cancelling;
                self.page.cancel_downloads_for_exit();
                self.exit_panel = Some(self.make_exit_panel(true));
            } else {
                self.exit_state = ExitState::Normal;
                self.exit_panel = None;
            }
            self.dirty = true;
        }
        self.page.sync(state, path);
        if self.page.take_native_closed() {
            // The page closed itself (`window.close()`): so does its tab.
            ui::shell_tabs::close_this();
        }
        // A link opened in a new tab, a page's `window.open`: another page
        // is another Browser in a desktop tab.
        for (url, background) in self.page.engine_handle().take_new_tab_requests() {
            self.page.open_beside(url, background);
        }
        // A late native entry notification must not hide the window-close
        // choices or cancellation progress while the exit flow owns the UI.
        if self.exit_state != ExitState::Normal {
            self.page.exit_content_fullscreen();
        }
        self.reconcile_content_fullscreen();
        if self.exit_state == ExitState::Cancelling {
            if self.page.download_exit_cleanup_complete() {
                self.exit_state = ExitState::Ready;
                self.exit_panel = None;
                self.close_intent = true;
            } else {
                self.exit_panel = Some(self.make_exit_panel(true));
            }
            self.dirty = true;
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let full = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Style::default()
        };
        if self.immersive() {
            let page = self.page.build_layout(taffy, theme, nodes);
            self.layout_immersive = Some(true);
            let mut children = vec![page];
            if let Some(hint) = self.fullscreen_hint.as_mut() {
                let hint = hint.build_layout(taffy, theme, nodes);
                let mut style = taffy.style(hint).cloned().unwrap_or_default();
                style.position = taffy::Position::Absolute;
                style.inset = taffy::Rect {
                    left: length(0.0_f32),
                    right: length(0.0_f32),
                    top: length(theme.tokens.spacing.s6),
                    bottom: auto(),
                };
                let _ = taffy.set_style(hint, style);
                children.push(hint);
            }
            let root = taffy.parent_node(full, &children);
            nodes.insert(root, std::ptr::from_mut::<dyn Widget>(self));
            return root;
        }
        let panel = self
            .exit_panel
            .as_mut()
            .map(|panel| panel.build_layout(taffy, theme, nodes));
        let page = self.page.build_layout(taffy, theme, nodes);
        self.layout_immersive = Some(false);
        if std::mem::take(&mut self.focus_address) && self.page.focus_address_with_caret() {
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
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..full
            },
            &panel.map_or_else(|| vec![page], |panel| vec![panel, page]),
        );
        nodes.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = ui::widget::placed(layout, origin);
        let Ok(children) = taffy.children(node) else {
            return;
        };
        let origin = self.rect.origin();
        if self.layout_immersive == Some(true) {
            if let Some(page) = children.first() {
                self.page.apply_layout(taffy, *page, origin);
            }
            if let Some(hint) = self.fullscreen_hint.as_mut()
                && let Some(node) = children.get(1)
            {
                hint.apply_layout(taffy, *node, origin);
            }
            return;
        }
        let page_index = usize::from(self.exit_panel.is_some());
        if let Some(panel) = self.exit_panel.as_mut()
            && let Some(node) = children.first()
        {
            panel.apply_layout(taffy, *node, origin);
        }
        if let Some(page) = children.get(page_index) {
            self.page.apply_layout(taffy, *page, origin);
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
        if self.layout_immersive == Some(true) {
            self.page.draw(scene, transform, tr, theme, presence);
            if let Some(hint) = self.fullscreen_hint.as_mut() {
                hint.draw(scene, transform, tr, theme, presence);
            }
            return;
        }
        if let Some(panel) = self.exit_panel.as_mut() {
            panel.draw(scene, transform, tr, theme, presence);
        }
        self.page.draw(scene, transform, tr, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if *ctx.handled {
            return;
        }
        let modifiers = ctx.modifiers.state();
        if Self::is_desktop_tab_chord(event, modifiers) {
            return;
        }
        if matches!(
            event,
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && key.logical_key == Key::Named(NamedKey::Escape)
        ) && self.page.content_fullscreen()
        {
            self.page.exit_content_fullscreen();
            self.observed_content_fullscreen = false;
            self.clear_fullscreen_hint();
            self.dirty = true;
            *ctx.handled = true;
            return;
        }
        // Full screen changes rebuild the tree; input in between would land
        // on node ids of the previous composition.
        if self.layout_immersive != Some(self.immersive()) {
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
            self.page
                .engine_handle()
                .set_modifiers(ui::webview::KeyModifiers {
                    ctrl: modifiers.control_key(),
                    shift: modifiers.shift_key(),
                    alt: modifiers.alt_key(),
                    meta: modifiers.super_key(),
                });
        }
        self.page.set_window_scale(ctx.scale_factor);
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
            if key.state == ElementState::Pressed && self.shortcut(&key.logical_key, modifiers) {
                *ctx.handled = true;
                return;
            }
        }
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if self.layout_immersive != Some(true)
            && let Some(panel) = self.exit_panel.as_mut()
        {
            if let Some(node) = children.first() {
                panel.handle_event(event, &mut ctx.reborrow(ctx.path, *node));
            }
            return;
        }
        if let Some(page) = children.first() {
            self.page
                .handle_event(event, &mut ctx.reborrow(ctx.path, *page));
        }
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self
            .exit_panel
            .as_mut()
            .is_some_and(|panel| panel.perform_semantic_action(target, action, state, path))
        {
            return true;
        }
        self.page
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
        self.page
            .draw_overlay(scene, transform, tr, theme, presence);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if self.immersive() {
            return std::iter::once(self.page.as_ref() as &dyn Widget)
                .chain(self.fullscreen_hint.as_deref())
                .collect();
        }
        let mut children: Vec<&dyn Widget> = Vec::new();
        if let Some(panel) = self.exit_panel.as_ref() {
            children.push(panel.as_ref());
        }
        children.push(self.page.as_ref());
        children
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.immersive() {
            let mut children: Vec<&mut dyn Widget> = vec![self.page.as_mut()];
            if let Some(hint) = self.fullscreen_hint.as_mut() {
                children.push(hint.as_mut());
            }
            return children;
        }
        let mut children: Vec<&mut dyn Widget> = Vec::new();
        if let Some(panel) = self.exit_panel.as_mut() {
            children.push(panel.as_mut());
        }
        children.push(self.page.as_mut());
        children
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let mut children = Vec::new();
        if self.fullscreen_hint.is_some() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Esc to exit full screen")
                    .with_hashed_key("browser.fullscreen-hint-status"),
            );
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name("Browser")
                .with_children(children),
        )
    }
    fn take_structure_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.dirty);
        self.page.take_structure_dirty() || dirty
    }
    fn needs_animation_frame(&self) -> bool {
        fullscreen_hint_visible(self.fullscreen_hint_deadline, ui::time::now())
            || self.page.needs_animation_frame()
    }
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.page.set_motion_reduced(reduced);
    }
    fn modal_capture(&self) -> Option<Rect> {
        self.exit_panel
            .as_ref()
            .map_or_else(|| self.page.modal_capture(), |panel| Some(panel.get_rect()))
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.page.cursor_shape(point)
    }
    fn name(&self) -> &'static str {
        "BrowserWindow"
    }
    fn test_id(&self) -> Option<&str> {
        Some("browser.window")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::component_lifecycle::ComponentLifecycle for BrowserWindow {
    fn on_window_close_requested(&mut self) -> WindowCloseAdmission {
        match self.exit_state {
            ExitState::Choice | ExitState::Cancelling => WindowCloseAdmission::Defer,
            ExitState::Normal if self.page.download_exit_requires_choice() => {
                // The immersive layout omits window-level dialogs. Leave page
                // full screen before asking what to do with active downloads.
                self.page.exit_content_fullscreen();
                self.observed_content_fullscreen = false;
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

impl ui::widget::controls::ActionControls for BrowserWindow {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        if let Some(invocation) = self
            .exit_panel
            .as_mut()
            .and_then(|panel| ui::widget::controls::take_control_invocation(panel.as_mut()))
        {
            return Some(invocation);
        }
        self.page.take_control_invocation()
    }
}

impl Continuable for BrowserWindow {
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        root: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        self.page.continuation_metadata(state, root)
    }
    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool {
        self.page
            .restore_continuation_metadata(metadata, state, root)
    }
}

/// Version of the page a window saves to show again when it reopens.
const SAVED_PAGE_VERSION: u32 = 1;

/// A reopened window shows the page it showed; the page is the window's own,
/// kept with the window rather than in a file beside the app.
impl ui::application_view::ApplicationViewState for BrowserWindow {
    fn capture_application_view(
        &self,
    ) -> Result<Option<ui::application_view::ApplicationLocalView>, ui::view::ThawError> {
        Ok(self
            .page
            .restorable_page()
            .map(|payload| ui::application_view::ApplicationLocalView {
                app_id: <crate::resource::BrowserShell as ui::framework::ResourceApp>::ID.into(),
                version: SAVED_PAGE_VERSION,
                payload,
            }))
    }

    fn restore_application_view(
        &mut self,
        local: &ui::application_view::ApplicationLocalView,
        _doc: &AutoCommit,
    ) -> Result<ui::application_view::ApplicationFocusRestore, ui::view::ThawError> {
        if local.version != SAVED_PAGE_VERSION || !self.page.restore_page(&local.payload) {
            return Err(ui::view::ThawError::State(
                "the saved page cannot be shown".into(),
            ));
        }
        // The window shows a page again; the caret has no address to wait in.
        self.focus_address = false;
        self.dirty = true;
        Ok(ui::application_view::ApplicationFocusRestore::Generic)
    }
}

impl FullscreenProvider for BrowserWindow {
    fn fullscreen_intent(&self) -> Option<bool> {
        Some(self.page.content_fullscreen())
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        ui::fullscreen::FullscreenRequest::mirror(ui::fullscreen::FullscreenTarget::Largest)
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        self.window_fullscreen_active = active;
        self.page.set_window_fullscreen_active(active);
        if active && self.page.content_fullscreen() {
            self.observed_content_fullscreen = true;
            self.start_fullscreen_hint();
        } else {
            if !active {
                self.page.exit_content_fullscreen();
                self.observed_content_fullscreen = false;
            }
            self.clear_fullscreen_hint();
        }
        self.dirty = true;
    }
}

impl FloatingSurfaceProvider for BrowserWindow {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        FloatingSurfaceProvider::take_floating_surface_request(&mut *self.page)
    }

    fn take_capture_studio_request(&mut self) -> Option<ui::widget::CaptureMode> {
        FloatingSurfaceProvider::take_capture_studio_request(&mut *self.page)
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        FloatingSurfaceProvider::take_floating_surface_source_updates(&mut *self.page)
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        FloatingSurfaceProvider::active_floating_surface_ids(&*self.page)
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        FloatingSurfaceProvider::on_floating_surface_host_message(&mut *self.page, message);
    }
}

impl Drop for BrowserWindow {
    fn drop(&mut self) {
        self.page.clear_history_on_workspace_exit();
    }
}

impl AppWidget for BrowserWindow {
    fn app_kind(&self) -> AppKind {
        AppKind::Browser
    }
    fn app_title(&self) -> &str {
        self.page.app_title()
    }
    fn app_icon(&self) -> IconDescriptor {
        self.page.app_icon()
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Browser)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ui::framework::HostDelivery as _;
    use ui::webview::{FrameSlot, WebViewCommand, WebViewEngine, WebViewEvent};

    /// A page engine that records commands and replays queued events and new
    /// tab intents.
    #[derive(Default)]
    struct PageEngine {
        events: std::sync::Mutex<Vec<WebViewEvent>>,
        commands: std::sync::Mutex<Vec<WebViewCommand>>,
        new_tabs: std::sync::Mutex<Vec<(String, bool)>>,
        frames: FrameSlot,
    }
    impl PageEngine {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn emit(&self, event: WebViewEvent) {
            self.events.lock().unwrap().push(event);
        }
        fn sent(&self, command: &WebViewCommand) -> usize {
            self.commands
                .lock()
                .unwrap()
                .iter()
                .filter(|sent| *sent == command)
                .count()
        }
    }
    impl WebViewEngine for PageEngine {
        fn send(&self, command: WebViewCommand) {
            self.commands.lock().unwrap().push(command);
        }
        fn navigate_owned(
            &self,
            request_id: u32,
            command: ui::webview::NavigationCommand,
        ) -> Result<(), String> {
            if request_id == 0 || request_id > i32::MAX as u32 {
                return Err("Invalid owned navigation request".into());
            }
            self.send(WebViewCommand::ScopedNavigation {
                request_id,
                command,
            });
            Ok(())
        }
        fn frames(&self) -> FrameSlot {
            self.frames.clone()
        }
        fn drain_events(&self) -> Vec<WebViewEvent> {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
        fn take_new_tab_requests(&self) -> Vec<(String, bool)> {
            std::mem::take(&mut *self.new_tabs.lock().unwrap())
        }
    }

    fn window() -> (BrowserWindow, Arc<PageEngine>) {
        let engine = PageEngine::new();
        (
            BrowserWindow::new(BrowserWidget::with_engine(engine.clone(), true), false),
            engine,
        )
    }

    fn key_event_with_state(key: Key, state: ElementState) -> WindowEvent {
        let event = ui::synthetic_input::key_event(key, None, state);
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    fn key_event(named: NamedKey) -> WindowEvent {
        key_event_with_state(Key::Named(named), ElementState::Pressed)
    }

    type Layout = (TaffyTree<()>, NodeId, HashMap<NodeId, *mut dyn Widget>);

    fn layout(window: &mut BrowserWindow) -> Layout {
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let root = window.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        tree.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(1000.0),
                height: taffy::AvailableSpace::Definite(700.0),
            },
        )
        .unwrap();
        window.apply_layout(&tree, root, Point::ORIGIN);
        (tree, root, nodes)
    }

    fn layout_rects(window: &mut BrowserWindow) -> HashMap<String, Rect> {
        let (_tree, _root, nodes) = layout(window);
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

    fn dispatch(
        window: &mut BrowserWindow,
        (tree, root): (&TaffyTree<()>, NodeId),
        event: &WindowEvent,
        modifiers: winit::keyboard::ModifiersState,
    ) -> bool {
        let mut handled = false;
        window.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::from(modifiers),
                state: Edits::lend(&mut AutoCommit::new()),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: root,
                taffy: tree,
            },
        );
        handled
    }

    fn invoke_exit_choice(window: &mut BrowserWindow, id: &str, doc: &mut AutoCommit) {
        let (_tree, _root, nodes) = layout(window);
        let &button = nodes
            .values()
            .find(|widget| unsafe { (&***widget).test_id() == Some(id) })
            .expect("exit choice button");
        assert!(unsafe {
            (&mut *button).perform_action(
                &ui::a11y::Action::Activate,
                &mut Edits::lend(doc),
                &automerge::ROOT,
            )
        });
        let invocation = ui::widget::controls::take_control_invocation(window)
            .expect("exit choice must reach the root action broker");
        assert_eq!(invocation.address.resource, format!("tilo.control:{id}"));
        assert!(
            window
                .start_encoded(
                    ui::tilo_actions::OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec(id).unwrap(),
                    doc,
                    &automerge::ROOT
                )
                .unwrap()
                .is_some()
        );
    }

    /// A window is one page: other pages are desktop tabs.
    #[test]
    fn other_pages_are_desktop_tabs_never_tabs_of_this_window() {
        // Phase `the_window_has_no_tab_strip`.
        {
            let (mut window, _engine) = window();
            let rects = layout_rects(&mut window);
            assert!(rects.contains_key("browser.url_input"), "{rects:?}");
            assert!(rects.contains_key("browser.surface"), "{rects:?}");
            assert!(
                !rects
                    .keys()
                    .any(|id| id.contains("tab-strip") || id.contains("new-tab")),
                "no tab strip and no new-tab button: {rects:?}"
            );
            let accessibility = window.accessibility().unwrap();
            assert_ne!(accessibility.role, ui::a11y::Role::TabBar);
        }

        // Phase `a_link_opened_in_a_new_tab_asks_the_desktop_for_a_tab_beside`.
        {
            let _ = ui::shell_tabs::take_requests();
            let (mut window, engine) = window();
            engine.new_tabs.lock().unwrap().extend([
                ("https://front.test/".to_owned(), false),
                ("https://behind.test/".to_owned(), true),
            ]);
            window.sync(&AutoCommit::new(), &automerge::ROOT);
            assert_eq!(
                ui::shell_tabs::take_requests(),
                vec![
                    ui::shell_tabs::TabRequest::OpenBeside {
                        target: Some("https://front.test/".into()),
                        background: false,
                    },
                    ui::shell_tabs::TabRequest::OpenBeside {
                        target: Some("https://behind.test/".into()),
                        background: true,
                    },
                ]
            );
            assert_eq!(
                engine.sent(&WebViewCommand::Navigate("https://front.test/".into())),
                0,
                "this window keeps showing its own page"
            );
        }

        // Phase `a_page_that_closes_itself_closes_its_desktop_tab`.
        {
            let _ = ui::shell_tabs::take_requests();
            let (mut window, engine) = window();
            engine.emit(WebViewEvent::TabClosed);
            window.sync(&AutoCommit::new(), &automerge::ROOT);
            assert_eq!(
                ui::shell_tabs::take_requests(),
                vec![ui::shell_tabs::TabRequest::CloseThis]
            );
        }

        // Phase `the_desktop_tab_chords_never_reach_the_page`.
        {
            use winit::keyboard::ModifiersState;
            let (mut window, engine) = window();
            let (tree, root, _nodes) = layout(&mut window);
            let before = engine.commands.lock().unwrap().len();
            for (text, modifiers) in [
                ("t", ModifiersState::CONTROL),
                ("T", ModifiersState::CONTROL | ModifiersState::SHIFT),
                ("w", ModifiersState::CONTROL),
            ] {
                let event =
                    key_event_with_state(Key::Character(text.into()), ElementState::Pressed);
                assert!(
                    !dispatch(&mut window, (&tree, root), &event, modifiers),
                    "{text} is left to the desktop"
                );
            }
            assert!(
                !engine.commands.lock().unwrap()[before..]
                    .iter()
                    .any(|command| matches!(command, WebViewCommand::Key { .. })),
                "the page saw none of them"
            );
        }
    }

    /// Content full screen hint lifetime and exit routes.
    #[test]
    fn content_fullscreen_owner_hint_and_exit_follow_the_root_window() {
        // Phase `content_fullscreen_hides_the_chrome_and_shows_the_hint`.
        {
            let (mut window, engine) = window();
            engine.emit(WebViewEvent::FullscreenChanged(true));
            window.sync(&AutoCommit::new(), &automerge::ROOT);
            assert_eq!(window.fullscreen_intent(), Some(true));

            window.on_fullscreen_changed(true);
            let rects = layout_rects(&mut window);
            assert!(rects.contains_key("browser.surface"), "{rects:?}");
            assert!(rects.contains_key("browser.fullscreen-hint"), "{rects:?}");
            assert!(!rects.contains_key("browser.url_input"), "{rects:?}");
            assert_eq!(rects["browser.surface"], Rect::new(0.0, 0.0, 1000.0, 700.0));
            assert!(rects["browser.fullscreen-hint"].y0 >= 24.0);
            assert!(window.needs_animation_frame());

            let (tree, root, _nodes) = layout(&mut window);
            assert!(dispatch(
                &mut window,
                (&tree, root),
                &key_event(NamedKey::Escape),
                winit::keyboard::ModifiersState::empty()
            ));
            assert_eq!(window.fullscreen_intent(), Some(false));
            assert_eq!(engine.sent(&WebViewCommand::ExitFullscreen), 1);
            window.on_fullscreen_changed(false);
            let rects = layout_rects(&mut window);
            assert!(rects.contains_key("browser.url_input"), "{rects:?}");
            assert!(!rects.contains_key("browser.fullscreen-hint"), "{rects:?}");
        }

        // Phase `ordinary_os_fullscreen_keeps_browser_chrome_and_has_no_hint`.
        {
            let (mut window, _engine) = window();
            window.on_fullscreen_changed(true);
            assert_eq!(window.fullscreen_intent(), Some(false));
            let rects = layout_rects(&mut window);
            assert!(rects.contains_key("browser.url_input"), "{rects:?}");
            assert!(!rects.contains_key("browser.fullscreen-hint"), "{rects:?}");
        }

        // Phase `input_is_held_until_the_immersive_tree_is_rebuilt`.
        {
            let (mut window, engine) = window();
            window.on_fullscreen_changed(true);
            let (tree, root, _nodes) = layout(&mut window);
            engine.emit(WebViewEvent::FullscreenChanged(true));
            window.sync(&AutoCommit::new(), &automerge::ROOT);
            assert!(window.fullscreen_hint.is_some());
            assert!(dispatch(
                &mut window,
                (&tree, root),
                &key_event_with_state(Key::Named(NamedKey::Enter), ElementState::Released),
                winit::keyboard::ModifiersState::empty()
            ));
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

    /// Window close and the download-exit choice.
    #[test]
    fn a_reopened_window_shows_its_page_and_a_private_one_keeps_none() {
        use ui::application_view::ApplicationViewState as View;
        let page = br#"{"url":"https://example.test/read?token=1"}"#;
        let (mut first, _engine) = window();
        assert!(first.page_mut().restore_page(page));
        let saved = View::capture_application_view(&first)
            .unwrap()
            .expect("the window saves its page");
        assert!(
            !String::from_utf8_lossy(&saved.payload).contains("token"),
            "a one-shot token in the query is not kept"
        );

        let (mut reopened, _engine) = window();
        reopened.focus_address = true;
        View::restore_application_view(&mut reopened, &saved, &AutoCommit::new()).unwrap();
        assert!(!reopened.focus_address, "the window shows its page again");
        assert_eq!(
            reopened.page().restorable_page(),
            first.page().restorable_page()
        );

        let mut private = BrowserWindow::new(
            BrowserWidget::with_engine_mode(
                PageEngine::new(),
                true,
                crate::browsing_mode::BrowsingMode::Private,
            ),
            false,
        );
        assert!(!private.page_mut().restore_page(page));
        assert!(View::capture_application_view(&private).unwrap().is_none());
        assert!(View::restore_application_view(&mut private, &saved, &AutoCommit::new()).is_err());
    }

    #[test]
    fn window_close_waits_for_an_explicit_download_choice() {
        let running = WebViewEvent::DownloadUpdated {
            id: 11,
            state: "progress".into(),
            path: "/tmp/file".into(),
            name: "file".into(),
            url: "https://downloads.test/file".into(),
            retryable: true,
            received: 10,
            total: 20,
            error: String::new(),
        };
        let runtime =
            |admitted: u32, detached: u32, accepting: bool| WebViewEvent::DownloadRuntime {
                admitted,
                detached,
                accepting,
                error: String::new(),
            };

        // Phase `fullscreen_download_close_exposes_both_exit_choices`.
        {
            let (mut window, engine) = window();
            let mut doc = AutoCommit::new();
            engine.emit(WebViewEvent::FullscreenChanged(true));
            engine.emit(runtime(1, 0, true));
            window.sync(&doc, &automerge::ROOT);
            window.on_fullscreen_changed(true);
            let _immersive = layout(&mut window);
            assert_eq!(window.fullscreen_intent(), Some(true));
            assert_eq!(
                ui::component_lifecycle::on_window_close_requested(&mut window),
                WindowCloseAdmission::Defer
            );
            assert_eq!(window.fullscreen_intent(), Some(false));
            assert!(window.fullscreen_hint.is_none());
            let rects = layout_rects(&mut window);
            for id in ["browser.download-exit.keep", "browser.download-exit.cancel"] {
                let rect = rects.get(id).expect("visible exit choice");
                assert!(rect.width() > 0.0 && rect.height() > 0.0);
            }
            assert_eq!(engine.sent(&WebViewCommand::CancelAllDownloads), 0);
            invoke_exit_choice(&mut window, "browser.download-exit.keep", &mut doc);
            window.sync(&doc, &automerge::ROOT);
            assert!(window.exit_panel.is_none());
            assert!(!ui::component_lifecycle::take_window_close_intent(
                &mut window
            ));
            assert_eq!(engine.sent(&WebViewCommand::CancelAllDownloads), 0);
        }

        // Phase `cancelling_waits_for_the_native_owner_before_quitting`.
        {
            let (mut window, engine) = window();
            let mut doc = AutoCommit::new();
            engine.emit(running);
            engine.emit(runtime(1, 0, true));
            window.sync(&doc, &automerge::ROOT);
            assert_eq!(
                ui::component_lifecycle::on_window_close_requested(&mut window),
                WindowCloseAdmission::Defer
            );
            invoke_exit_choice(&mut window, "browser.download-exit.cancel", &mut doc);
            window.sync(&doc, &automerge::ROOT);
            assert_eq!(engine.sent(&WebViewCommand::CancelAllDownloads), 1);
            assert!(!ui::component_lifecycle::take_window_close_intent(
                &mut window
            ));
            assert_eq!(
                ui::component_lifecycle::on_window_close_requested(&mut window),
                WindowCloseAdmission::Defer,
                "repeated close cannot bypass native cancellation"
            );
            engine.emit(WebViewEvent::DownloadUpdated {
                id: 11,
                state: "cancelled".into(),
                path: String::new(),
                name: String::new(),
                url: String::new(),
                retryable: false,
                received: 0,
                total: 0,
                error: String::new(),
            });
            engine.emit(runtime(0, 1, false));
            window.sync(&doc, &automerge::ROOT);
            assert!(
                !ui::component_lifecycle::take_window_close_intent(&mut window),
                "the physical owner is still closing"
            );
            engine.emit(runtime(0, 0, false));
            window.sync(&doc, &automerge::ROOT);
            assert!(ui::component_lifecycle::take_window_close_intent(
                &mut window
            ));
            assert!(
                !ui::component_lifecycle::take_window_close_intent(&mut window),
                "approval is one shot"
            );
        }

        // Phase `a_window_without_downloads_closes_at_once`.
        {
            let (mut window, _engine) = window();
            assert_eq!(
                ui::component_lifecycle::on_window_close_requested(&mut window),
                WindowCloseAdmission::Allow
            );
        }
    }

    /// The address arrives on the keyboard as a person types it: Ctrl+L, the
    /// address, Enter navigates the page to exactly that address, whatever
    /// the engine reports about the start page before, during or after.
    #[test]
    fn a_typed_address_after_ctrl_l_navigates_there() {
        const URL: &str = "http://127.0.0.1:42975/";
        // Before Ctrl+L, mid-scheme, mid-host, and just before Enter.
        const SPLITS: [usize; 4] = [0, 3, 12, URL.len() + 1];
        let scenarios: Vec<(&str, Vec<WebViewEvent>)> = vec![
            (
                "uri",
                vec![WebViewEvent::UriChanged("https://duckduckgo.com/".into())],
            ),
            (
                "uri+title+finished",
                vec![
                    WebViewEvent::UriChanged("https://duckduckgo.com/".into()),
                    WebViewEvent::TitleChanged("DuckDuckGo".into()),
                    WebViewEvent::LoadFinished,
                ],
            ),
            (
                "failed",
                vec![WebViewEvent::LoadFailed {
                    uri: "https://duckduckgo.com/".into(),
                    error: "net::ERR_CERT_AUTHORITY_INVALID".into(),
                }],
            ),
        ];
        let mut failures = Vec::new();
        for (name, events) in &scenarios {
            for split in SPLITS {
                // Browser runs in its own app host, which routes the address
                // bar's typed navigate action to the page's engine.
                let engine = PageEngine::new();
                crate::browser_widget::test_window_engine::install(engine.clone());
                let mut app = ui::framework::testing::TestApp::<crate::BrowserShell>::new()
                    .expect("Browser opens");
                let harness = app.harness();
                for _ in 0..3 {
                    harness.relayout();
                }
                let emit = |harness: &mut ui::headless::FrameHarness| {
                    for event in events {
                        engine.emit(event.clone());
                    }
                    harness.relayout();
                    harness.relayout();
                };
                if split == 0 {
                    emit(harness);
                }
                harness.send_host_event(&WindowEvent::ModifiersChanged(
                    winit::keyboard::ModifiersState::CONTROL.into(),
                ));
                harness.key("l");
                harness.send_host_event(&WindowEvent::ModifiersChanged(
                    winit::keyboard::ModifiersState::empty().into(),
                ));
                harness.relayout();
                for (at, ch) in URL.chars().enumerate() {
                    if split == at + 1 {
                        emit(harness);
                    }
                    harness.key(&ch.to_string());
                    harness.relayout();
                }
                if split == URL.len() + 1 {
                    emit(harness);
                }
                harness.key("Enter");
                app.settle();
                let commands = engine.commands.lock().unwrap().clone();
                let navigated: Vec<_> = commands
                    .iter()
                    .filter_map(|command| match command {
                        WebViewCommand::ScopedNavigation {
                            request_id,
                            command: ui::webview::NavigationCommand::Navigate { url },
                        } => {
                            assert!(*request_id > 0, "typed navigation has an owner");
                            Some(url.clone())
                        }
                        _ => None,
                    })
                    .collect();
                if !navigated.iter().any(|url| url == URL) {
                    failures.push(format!("{name} split {split}: {navigated:?}"));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}

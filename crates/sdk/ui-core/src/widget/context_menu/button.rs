//! The button that opens a menu ([`ContextMenu`]) anchored to its own rect:
//! a header's More button, a toolbar's sort menu. It is a button a menu is
//! attached to, so it reads as expanded while the menu is open, and the menu
//! it opens is the one every other menu is.

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{AppModel, ModelPath, Widget};

use super::{ContextMenu, MenuItem, PanelAlign};

/// A button that opens a menu anchored to its own rect
/// ([`crate::widget::Button::with_menu`]): a header's More button, a
/// toolbar's sort menu. The menu opens under the button, lines up with
/// whichever of its edges keeps it in the window, and flips above near the
/// bottom; Enter, Space or a click opens it, and the button reads as
/// expanded while it is open.
pub struct MenuButton {
    pub(super) button: crate::widget::Button,
    pub(super) menu: ContextMenu,
    /// Set by the button's click; the menu opens on the next turn.
    request: std::rc::Rc<std::cell::Cell<bool>>,
    /// Pointer opening leaves selection under the pointer; keyboard and
    /// semantic activation start at the first action.
    request_keyboard: bool,
    /// The window the top layer last painted over.
    window: Option<Rect>,
    align: PanelAlign,
}

impl crate::widget::Button {
    /// Make this button open `items` as a menu anchored to its own rect.
    #[must_use]
    pub fn with_menu(mut self, items: Vec<MenuItem>) -> MenuButton {
        self.opens_menu = true;
        let request = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = std::rc::Rc::clone(&request);
        MenuButton {
            button: self.on_view(move || flag.set(true)),
            menu: ContextMenu::with_items(items),
            request,
            request_keyboard: true,
            window: None,
            align: PanelAlign::Leading,
        }
    }
}

impl MenuButton {
    /// Line the menu up with the button's trailing edge (a button at the
    /// end of a header row) instead of its leading edge.
    #[must_use]
    pub fn aligned(mut self, align: PanelAlign) -> Self {
        self.align = align;
        self.menu.set_align(align);
        self
    }

    /// Replace the menu's rows (a state row's check, a disabled command).
    pub fn set_items(&mut self, items: Vec<MenuItem>) {
        self.menu.set_items(items);
    }

    /// The menu, for its selection channel
    /// ([`ContextMenu::take_selection`]).
    pub fn menu_mut(&mut self) -> &mut ContextMenu {
        &mut self.menu
    }

    pub fn button(&self) -> &crate::widget::Button {
        &self.button
    }

    pub fn button_mut(&mut self) -> &mut crate::widget::Button {
        &mut self.button
    }

    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Open the menu at the button (a stage hook, a shortcut).
    pub fn open(&mut self, keyboard: bool) {
        let anchor = Widget::get_rect(&self.button);
        let viewport = self
            .window
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        self.menu.set_align(self.align);
        self.menu.open_anchored(anchor, viewport);
        if keyboard {
            self.menu.focus_first_action();
        }
    }

    pub fn close(&mut self) {
        self.menu.close();
    }

    fn take_request(&mut self, keyboard: bool) {
        if self.request.replace(false) {
            if self.menu.is_open() {
                self.menu.close();
            } else {
                self.open(keyboard);
            }
            // A later admitted semantic action has no raw pointer event.
            self.request_keyboard = true;
        }
    }
}

impl crate::widget::Decorator for MenuButton {
    type Inner = crate::widget::Button;

    fn inner(&self) -> &crate::widget::Button {
        &self.button
    }

    fn inner_mut(&mut self) -> &mut crate::widget::Button {
        &mut self.button
    }

    /// Its menu opens where the button stands, and the window's bar has no place to hang it: a
    /// header holding one keeps its row.
    fn window_slot_barrier(&self) -> bool {
        true
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        Widget::sync(&mut self.button, state, path);
        // Admitted view actions execute after event dispatch. Consume their
        // one-shot request on the same real sync path as the next frame.
        self.take_request(self.request_keyboard);
    }

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        Widget::set_platform_conventions(&mut self.button, conventions);
        Widget::set_platform_conventions(&mut self.menu, conventions);
    }

    // The menu is a child only while it is open, so it hears the host's
    // input capabilities whenever they change, closed or not: its rows are
    // the size of a target the next time it opens.
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.button.set_interaction_environment(environment);
        self.menu.set_interaction_environment(environment);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.button, taffy, theme, node_map);
        // Focus and keyboard activation reach the menu button, not the
        // button inside it.
        crate::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.menu.is_open() {
            self.menu.handle_event(event, ctx);
            // The press that closed the menu on the button itself must not
            // open it again.
            if !self.menu.is_open() {
                self.request.set(false);
            }
            return;
        }
        match event {
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::Touch(_) => self.request_keyboard = false,
            WindowEvent::KeyboardInput { .. } => self.request_keyboard = true,
            _ => {}
        }
        Widget::handle_event(&mut self.button, event, ctx);
        self.take_request(self.request_keyboard);
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) -> bool {
        if matches!(action, crate::a11y::Action::Activate) && Widget::is_enabled(&self.button) {
            if self.menu.is_open() {
                self.menu.close();
            } else {
                self.open(true);
            }
            return true;
        }
        Widget::perform_action(&mut self.button, action, &[], state, root)
    }

    // The menu stands against the button, not in the button's box: the pass
    // places it too, so its edges land on the display's pixels.
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        Widget::apply_layout(&mut self.button, taffy, node, origin);
        if self.menu.is_open()
            && let Some(window) = self.window
        {
            self.menu.reanchor(Widget::get_rect(&self.button), window);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Widget::draw_overlay(
            &mut self.button,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        if let Some(window) = presence.viewport.get() {
            self.window = Some(window);
        }
        if self.menu.is_open()
            && let Some(window) = self.window
        {
            self.menu.reanchor(Widget::get_rect(&self.button), window);
        }
        self.menu
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    // A host reaches its menu button in a tree it does not own (a shell
    // header) to refresh the rows or open it from a stage hook.

    fn modal_capture(&self) -> Option<Rect> {
        self.menu.modal_capture()
    }

    fn traps_focus(&self) -> bool {
        self.menu.is_open()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open() || Widget::wants_pointer_capture(&self.button)
    }

    fn needs_redraw(&self) -> bool {
        self.menu.is_open() || Widget::needs_redraw(&self.button)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = Widget::accessibility(&self.button)?;
        node.state.expanded = Some(self.menu.is_open());
        if self.menu.is_open()
            && let Some(menu) = Widget::accessibility(&self.menu)
        {
            node.children.push(menu);
        }
        Some(node)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.menu
            .perform_semantic_action(target, action, state, path)
            || Widget::perform_semantic_action(&mut self.button, target, action, state, path)
    }

    fn name(&self) -> &str {
        "MenuButton"
    }

    /// The menu button answers for the button's own press and for every row its menu offers,
    /// open or closed: a row runs the action the view bound to it as the button runs its own,
    /// and the rows it publishes reach the host through the button that stands in the tree.
    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn crate::widget::controls::ActionControls>() {
            request.provide::<dyn crate::widget::controls::ActionControls>(self);
        } else if request.wants::<dyn crate::resources::PublishesResources>() {
            request.provide::<dyn crate::resources::PublishesResources>(self);
        } else {
            Widget::provide(&self.button, request);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn crate::widget::controls::ActionControls>() {
            request.provide::<dyn crate::widget::controls::ActionControls>(self);
        } else if request.wants::<dyn crate::resources::PublishesResources>() {
            request.provide::<dyn crate::resources::PublishesResources>(self);
        } else if request.wants::<dyn crate::widget::ResourceOperations>() {
            request.provide::<dyn crate::widget::ResourceOperations>(self);
        } else {
            Widget::provide_mut(&mut self.button, request);
        }
    }
}

/// The menu is drawn over the button and stands against it, not in the button's subtree, so
/// the rows it publishes are the button's to publish and its rows' operations the button's to
/// start.
impl crate::resources::PublishesResources for MenuButton {
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        let mut resources = crate::resources::resources(&self.button, doc);
        resources.extend(crate::resources::resources(&self.menu, doc));
        resources
    }
}

impl crate::widget::ResourceOperations for MenuButton {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        let mut handlers =
            crate::widget::resource_operation::resource_operation_handlers(&self.button);
        handlers.extend(crate::widget::resource_operation::resource_operation_handlers(&self.menu));
        handlers
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if let Some(started) =
            crate::widget::resource_operation::start_operation(&mut self.button, op, doc, path)?
        {
            return Ok(Some(started));
        }
        crate::widget::resource_operation::start_operation(&mut self.menu, op, doc, path)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        crate::widget::resource_operation::poll_resource_operation(
            &mut self.button,
            invocation,
            cancellation_requested,
            doc,
            path,
        )
        .or_else(|| {
            crate::widget::resource_operation::poll_resource_operation(
                &mut self.menu,
                invocation,
                cancellation_requested,
                doc,
                path,
            )
        })
    }
}

impl crate::widget::controls::ActionControls for MenuButton {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        crate::widget::controls::install_control_custody(&mut self.button, domain, attachment);
        crate::widget::controls::install_control_custody(&mut self.menu, domain, attachment);
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        crate::widget::controls::take_control_invocation(&mut self.button)
            .or_else(|| crate::widget::controls::take_control_invocation(&mut self.menu))
    }

    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = crate::widget::controls::control_action_bindings(&self.button);
        bindings.extend(crate::widget::controls::control_action_bindings(&self.menu));
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        crate::widget::controls::refresh_control_action_availability(&mut self.button, allowed);
        crate::widget::controls::refresh_control_action_availability(&mut self.menu, allowed);
    }
}

//! Retained host for a platform/native child view.
//!
//! Unlike [`WebView`](super::WebView), which paints an offscreen engine frame,
//! `NativeView` gives a backend the live parent [`winit::window::Window`]. A
//! Win32 child HWND, Cocoa `NSView`, GTK widget, media surface, or another
//! platform component can therefore remain native while participating in the
//! toolkit's layout, focus, platform conventions, visibility, accessibility,
//! and teardown lifecycle.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use winit::event::WindowEvent;

use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{Element, NativeViewLifecycle, TaffyTreeExt, Widget};

/// Live parent-window context supplied before a native view is shown.
pub struct NativeViewHost<'a> {
    window: Option<&'a winit::window::Window>,
    platform: PlatformConventions,
    scale_factor: f64,
}

impl<'a> NativeViewHost<'a> {
    pub fn live(window: &'a winit::window::Window, platform: PlatformConventions) -> Self {
        Self {
            window: Some(window),
            platform,
            scale_factor: window.scale_factor(),
        }
    }

    /// Context for deterministic backend tests. Production platform backends
    /// normally require [`Self::window`] and should return a mount error when
    /// it is absent.
    pub const fn headless(platform: PlatformConventions, scale_factor: f64) -> Self {
        Self {
            window: None,
            platform,
            scale_factor,
        }
    }

    pub const fn window(&self) -> Option<&'a winit::window::Window> {
        self.window
    }

    pub const fn platform(&self) -> PlatformConventions {
        self.platform
    }

    pub const fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    pub fn window_id(&self) -> Option<winit::window::WindowId> {
        self.window.map(winit::window::Window::id)
    }
}

/// Parent-relative geometry in both toolkit-logical and native physical
/// pixels. Backends should use `physical_*` for child-window APIs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeViewGeometry {
    pub logical: Rect,
    pub physical_x: i32,
    pub physical_y: i32,
    pub physical_width: u32,
    pub physical_height: u32,
    pub scale_factor: f64,
}

impl NativeViewGeometry {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "values are rounded and clamped to the exact target platform integer ranges before conversion"
    )]
    fn resolve(logical: Rect, scale_factor: f64) -> Self {
        let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let signed = |value: f64| {
            (value * scale_factor)
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        };
        let extent = |value: f64| {
            (value.max(0.0) * scale_factor)
                .round()
                .clamp(0.0, f64::from(u32::MAX)) as u32
        };
        Self {
            logical,
            physical_x: signed(logical.x0),
            physical_y: signed(logical.y0),
            physical_width: extent(logical.width()),
            physical_height: extent(logical.height()),
            scale_factor,
        }
    }
}

/// Event coordinates and modifiers already normalized by the toolkit host.
#[derive(Clone, Copy, Debug)]
pub struct NativeViewInput {
    pub local_position: Option<Point>,
    pub scale_factor: f64,
    pub modifiers: winit::keyboard::ModifiersState,
}

/// App/platform implementation of one native child view.
///
/// `mount` is called once per successful attachment. `host_changed` follows
/// scale-factor or host convention changes. `unmount` is guaranteed before
/// the parent window is destroyed and is also called from `NativeView::drop`.
pub trait NativeViewBackend {
    /// Attach the child to the supplied parent-window context.
    ///
    /// # Errors
    ///
    /// Returns a backend-specific explanation when the native child cannot
    /// be created or attached.
    fn mount(&mut self, host: &NativeViewHost<'_>) -> Result<(), String>;
    fn unmount(&mut self);

    fn host_changed(&mut self, _host: &NativeViewHost<'_>) {}
    fn set_geometry(&mut self, geometry: NativeViewGeometry);
    fn set_visible(&mut self, visible: bool);
    fn set_focus(&mut self, focused: bool);
    fn set_enabled(&mut self, enabled: bool);
    fn set_platform_conventions(&mut self, _conventions: PlatformConventions) {}

    /// Optional forwarding path for native APIs that do not own a child
    /// window/event queue. A real child window can keep the default `false`.
    fn handle_event(&mut self, _event: &WindowEvent, _input: NativeViewInput) -> bool {
        false
    }

    fn wants_pointer_capture(&self) -> bool {
        false
    }

    fn needs_animation_frame(&self) -> bool {
        false
    }
}

/// Whether AccessKit or the native child owns this view's semantic subtree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeViewAccessibility {
    /// Export a named wrapper node through the toolkit accessibility tree.
    Toolkit { name: String },
    /// The platform child publishes its own native accessibility subtree.
    PlatformOwned,
}

/// Retained widget adapter around a [`NativeViewBackend`].
#[allow(
    clippy::struct_excessive_bools,
    reason = "mount, focus, enabled, and native visibility are independent platform states"
)]
pub struct NativeView {
    backend: Box<dyn NativeViewBackend>,
    accessibility: NativeViewAccessibility,
    test_id: String,
    rect: Rect,
    requested_size: Size<Dimension>,
    platform: PlatformConventions,
    scale_factor: f64,
    host_window_id: Option<winit::window::WindowId>,
    mounted: bool,
    mount_error: Option<String>,
    focused: bool,
    enabled: bool,
    visible: bool,
    last_cursor: Point,
    fallback: Option<Element>,
    fallback_node: Option<NodeId>,
}

impl NativeView {
    /// Construct a native view whose wrapper is named in AccessKit. Use
    /// [`Self::platform_accessible`] when the child publishes native semantics.
    pub fn new(name: impl Into<String>, backend: impl NativeViewBackend + 'static) -> Self {
        Self {
            backend: Box::new(backend),
            accessibility: NativeViewAccessibility::Toolkit { name: name.into() },
            test_id: "native-view".into(),
            rect: Rect::ZERO,
            requested_size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(240.0),
            },
            platform: PlatformConventions::default(),
            scale_factor: 1.0,
            host_window_id: None,
            mounted: false,
            mount_error: None,
            focused: false,
            enabled: true,
            visible: false,
            last_cursor: Point::ZERO,
            fallback: None,
            fallback_node: None,
        }
    }

    /// Construct a view whose child is represented by the platform's own
    /// accessibility bridge, avoiding a duplicate AccessKit subtree.
    pub fn platform_accessible(backend: impl NativeViewBackend + 'static) -> Self {
        let mut view = Self::new("Native view", backend);
        view.accessibility = NativeViewAccessibility::PlatformOwned;
        view
    }

    #[must_use]
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = id.into();
        self
    }

    #[must_use]
    pub fn with_size(mut self, width: Dimension, height: Dimension) -> Self {
        self.requested_size = Size { width, height };
        self
    }

    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            if self.mounted {
                self.backend.set_enabled(enabled);
            }
        }
    }

    /// Toolkit-rendered equivalent used when no live parent window exists or
    /// native mounting fails. This keeps screenshots, remote/WASM builds, and
    /// headless app tests useful without pretending to capture a child window.
    #[must_use]
    pub fn with_fallback(mut self, fallback: Element) -> Self {
        self.fallback = Some(fallback);
        self
    }

    pub fn mount_error(&self) -> Option<&str> {
        self.mount_error.as_deref()
    }

    pub fn is_mounted(&self) -> bool {
        self.mounted
    }

    fn sync_geometry(&mut self) {
        if self.mounted {
            self.backend
                .set_geometry(NativeViewGeometry::resolve(self.rect, self.scale_factor));
        }
    }
}

impl Drop for NativeView {
    fn drop(&mut self) {
        if self.mounted {
            self.backend.set_visible(false);
            self.backend.unmount();
            self.mounted = false;
        }
    }
}

impl NativeViewLifecycle for NativeView {
    fn mount_native_view(&mut self, host: &NativeViewHost<'_>) {
        let host_changed = self.host_window_id != host.window_id()
            || self.platform != host.platform()
            || self.scale_factor.to_bits() != host.scale_factor().to_bits();
        self.platform = host.platform();
        self.scale_factor = host.scale_factor();
        self.host_window_id = host.window_id();
        self.backend.set_platform_conventions(self.platform);
        if self.mounted {
            if host_changed {
                self.backend.host_changed(host);
            }
        } else {
            match self.backend.mount(host) {
                Ok(()) => {
                    self.mounted = true;
                    self.mount_error = None;
                    self.backend.set_enabled(self.enabled);
                    self.backend.set_focus(self.focused);
                }
                Err(error) => self.mount_error = Some(error),
            }
        }
        self.sync_geometry();
    }

    fn unmount_native_view(&mut self) {
        if self.mounted {
            self.backend.set_visible(false);
            self.backend.unmount();
            self.mounted = false;
            self.visible = false;
        }
    }

    fn native_view_frame_start(&mut self) {
        if self.mounted && self.visible {
            self.backend.set_visible(false);
            self.visible = false;
        }
    }
}

impl Widget for NativeView {
    crate::provides!(dyn crate::widget::NativeViewLifecycle);

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.backend.set_platform_conventions(conventions);
        if let Some(fallback) = &mut self.fallback {
            fallback.set_platform_conventions(conventions);
        }
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        if let Some(fallback) = &mut self.fallback {
            fallback.set_interaction_environment(environment);
        }
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        if let Some(fallback) = &mut self.fallback {
            fallback.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.fallback
            .as_mut()
            .is_some_and(|fallback| fallback.update(state, path))
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.fallback.as_mut().map(|fallback| {
            let child = fallback.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(child).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            style.size = Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            };
            taffy.set_style(child, style).ok();
            child
        });
        self.fallback_node = child;
        let children = child.into_iter().collect::<Vec<_>>();
        let node = taffy.parent_node(
            Style {
                size: self.requested_size,
                ..Default::default()
            },
            &children,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        self.sync_geometry();
        if let (Some(fallback), Some(child)) = (&mut self.fallback, self.fallback_node) {
            fallback.apply_layout(taffy, child, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if self.mounted {
            if self.rect.width() > 0.0 && self.rect.height() > 0.0 && !self.visible {
                self.backend.set_visible(true);
                self.visible = true;
            }
            return;
        }

        if let Some(fallback) = &mut self.fallback {
            fallback.draw(scene, transform, text_renderer, theme, presence);
            return;
        }

        let radius = f64::from(theme.tokens.radii.control);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            self.mount_error.as_ref().map_or(
                crate::surface::edge_on(theme.tokens.colors.surfaces.base),
                |_| theme.tokens.colors.semantic.danger,
            ),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );
        let label = self
            .mount_error
            .as_deref()
            .unwrap_or("Native view is available only in a live platform window");
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the native fallback width is bounded by the on-screen widget rectangle"
        )]
        let max_width = (self.rect.width() - 24.0).max(0.0) as f32;
        text_renderer.draw_style(
            scene,
            label,
            theme.tokens.colors.text_dim,
            transform.pre_translate((self.rect.x0 + 12.0, self.rect.y0 + 12.0).into()),
            Some(max_width),
            &theme.text(TextRole::Caption),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.enabled {
            return;
        }
        if !self.mounted {
            if let (Some(fallback), Some(child)) = (&mut self.fallback, self.fallback_node) {
                fallback.handle_event(event, &mut ctx.reborrow(ctx.path, child));
            }
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        let pointer_event = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        );
        if pointer_event
            && !self.rect.contains(self.last_cursor)
            && !self.backend.wants_pointer_capture()
        {
            return;
        }
        if matches!(event, WindowEvent::KeyboardInput { .. }) && !self.focused {
            return;
        }
        let local_position = self.rect.contains(self.last_cursor).then(|| {
            Point::new(
                self.last_cursor.x - self.rect.x0,
                self.last_cursor.y - self.rect.y0,
            )
        });
        if self.backend.handle_event(
            event,
            NativeViewInput {
                local_position,
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers.state(),
            },
        ) {
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            ) {
                *ctx.focus_target = Some(ctx.node_id);
            }
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "NativeView"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        self.enabled
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            if self.mounted {
                self.backend.set_focus(focused);
            }
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn wants_pointer_capture(&self) -> bool {
        if self.mounted {
            self.backend.wants_pointer_capture()
        } else {
            self.fallback
                .as_deref()
                .is_some_and(Widget::wants_pointer_capture)
        }
    }

    fn needs_animation_frame(&self) -> bool {
        if self.mounted {
            self.backend.needs_animation_frame()
        } else {
            self.fallback
                .as_deref()
                .is_some_and(Widget::needs_animation_frame)
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.mounted {
            Vec::new()
        } else {
            self.fallback
                .as_deref()
                .map_or_else(Vec::new, |fallback| vec![fallback])
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if !self.mounted
            && let Some(fallback) = self.fallback.as_deref()
        {
            visitor(fallback);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.mounted {
            Vec::new()
        } else if let Some(fallback) = self.fallback.as_deref_mut() {
            vec![fallback as &mut (dyn Widget + '_)]
        } else {
            Vec::new()
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        match &self.accessibility {
            NativeViewAccessibility::Toolkit { name } => Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                    .with_name(name)
                    .with_description(
                        self.mount_error
                            .as_deref()
                            .unwrap_or("Native platform view"),
                    )
                    .with_state(
                        crate::a11y::AccessState::new()
                            .focusable(self.enabled)
                            .focused(self.focused)
                            .disabled(!self.enabled),
                    ),
            ),
            NativeViewAccessibility::PlatformOwned => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Debug, PartialEq)]
    enum Call {
        Mount,
        HostChanged,
        Geometry(NativeViewGeometry),
        Visible(bool),
        Focus(bool),
        Enabled(bool),
        Platform(PlatformConventions),
        Input(Option<Point>),
        Unmount,
    }

    struct ProbeBackend {
        calls: Rc<RefCell<Vec<Call>>>,
        fail_mount: bool,
    }

    impl NativeViewBackend for ProbeBackend {
        fn mount(&mut self, _host: &NativeViewHost<'_>) -> Result<(), String> {
            self.calls.borrow_mut().push(Call::Mount);
            if self.fail_mount {
                Err("native API unavailable".into())
            } else {
                Ok(())
            }
        }

        fn unmount(&mut self) {
            self.calls.borrow_mut().push(Call::Unmount);
        }

        fn host_changed(&mut self, _host: &NativeViewHost<'_>) {
            self.calls.borrow_mut().push(Call::HostChanged);
        }

        fn set_geometry(&mut self, geometry: NativeViewGeometry) {
            self.calls.borrow_mut().push(Call::Geometry(geometry));
        }

        fn set_visible(&mut self, visible: bool) {
            self.calls.borrow_mut().push(Call::Visible(visible));
        }

        fn set_focus(&mut self, focused: bool) {
            self.calls.borrow_mut().push(Call::Focus(focused));
        }

        fn set_enabled(&mut self, enabled: bool) {
            self.calls.borrow_mut().push(Call::Enabled(enabled));
        }

        fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
            self.calls.borrow_mut().push(Call::Platform(conventions));
        }

        fn handle_event(&mut self, _event: &WindowEvent, input: NativeViewInput) -> bool {
            self.calls
                .borrow_mut()
                .push(Call::Input(input.local_position));
            true
        }
    }

    fn backend(calls: &Rc<RefCell<Vec<Call>>>) -> ProbeBackend {
        ProbeBackend {
            calls: Rc::clone(calls),
            fail_mount: false,
        }
    }

    #[test]
    fn native_mount_lifecycle_is_idempotent_and_owns_geometry_visibility_and_focus() {
        // Case `mount_geometry_visibility_focus_and_unmount_are_one_lifecycle`.
        {
            use crate::platform::Platform;

            let calls = Rc::new(RefCell::new(Vec::new()));
            let mut view = NativeView::new("Calendar", backend(&calls));
            view.rect = Rect::new(10.0, 20.0, 110.0, 70.0);
            let host = NativeViewHost::headless(PlatformConventions::new(Platform::Windows), 1.5);
            crate::widget::mount_native_views(&mut view, &host);
            assert!(view.is_mounted());
            assert!(calls.borrow().contains(&Call::Geometry(NativeViewGeometry {
                logical: view.rect,
                physical_x: 15,
                physical_y: 30,
                physical_width: 150,
                physical_height: 75,
                scale_factor: 1.5,
            })));
            view.set_focus(true);
            view.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::default(),
            );
            crate::widget::native_view_frame_start(&mut view);
            crate::widget::unmount_native_views(&mut view);

            let calls = calls.borrow();
            assert!(calls.contains(&Call::Platform(host.platform())));
            assert!(calls.contains(&Call::Focus(true)));
            assert!(calls.contains(&Call::Visible(true)));
            assert!(calls.contains(&Call::Visible(false)));
            assert_eq!(calls.last(), Some(&Call::Unmount));
        }

        // Case `mounting_is_idempotent_and_real_host_changes_do_not_recreate_the_child`.
        {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let mut view = NativeView::new("Preview", backend(&calls));
            let host = NativeViewHost::headless(PlatformConventions::default(), 1.0);
            view.mount_native_view(&host);
            view.mount_native_view(&host);
            let scaled = NativeViewHost::headless(PlatformConventions::default(), 2.0);
            view.mount_native_view(&scaled);
            assert_eq!(
                calls
                    .borrow()
                    .iter()
                    .filter(|call| matches!(call, Call::Mount))
                    .count(),
                1
            );
            assert_eq!(
                calls
                    .borrow()
                    .iter()
                    .filter(|call| matches!(call, Call::HostChanged))
                    .count(),
                1,
                "only the scale change reaches host_changed"
            );
        }

        // Case `forwarded_input_is_local_and_a_handled_press_requests_focus`.
        {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let mut view = NativeView::new("Native editor", backend(&calls));
            view.rect = Rect::new(10.0, 20.0, 210.0, 120.0);
            view.mount_native_view(&NativeViewHost::headless(
                PlatformConventions::default(),
                1.0,
            ));
            let mut taffy = TaffyTree::new();
            let node = taffy.leaf_node(Style::default());
            let mut state = AutoCommit::new();
            let mut handled = false;
            let mut focus = None;
            let modifiers = winit::event::Modifiers::default();
            let cursor = WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(40.0, 70.0),
            };
            view.handle_event(
                &cursor,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut state,
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            let press = WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            };
            view.handle_event(
                &press,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut state,
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(handled);
            assert_eq!(focus, Some(node));
            assert!(
                calls
                    .borrow()
                    .contains(&Call::Input(Some(Point::new(30.0, 50.0))))
            );
        }
    }

    #[test]
    fn native_mount_failure_and_platform_accessibility_stay_truthful() {
        // Case `mount_failure_is_visible_and_accessible_without_fake_native_state`.
        {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let mut view = NativeView::new(
                "System account picker",
                ProbeBackend {
                    calls,
                    fail_mount: true,
                },
            );
            view.mount_native_view(&NativeViewHost::headless(
                PlatformConventions::default(),
                1.0,
            ));
            assert!(!view.is_mounted());
            assert_eq!(view.mount_error(), Some("native API unavailable"));
            let node = view
                .accessibility()
                .expect("failure wrapper remains semantic");
            assert_eq!(node.name.as_deref(), Some("System account picker"));
            assert_eq!(node.description.as_deref(), Some("native API unavailable"));
        }

        // Case `platform_owned_accessibility_does_not_export_a_duplicate_wrapper`.
        {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let view = NativeView::platform_accessible(backend(&calls));
            assert!(view.accessibility().is_none());
        }

        // Case `toolkit_fallback_remains_in_the_tree_when_native_mounting_fails`.
        {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let mut view = NativeView::new(
                "System picker",
                ProbeBackend {
                    calls,
                    fail_mount: true,
                },
            )
            .with_fallback(Box::new(crate::widget::Text::new("Portable picker")));
            view.mount_native_view(&NativeViewHost::headless(
                PlatformConventions::default(),
                1.0,
            ));
            assert_eq!(view.children().len(), 1);
            assert_eq!(view.children()[0].name(), "Text");
        }
    }
}

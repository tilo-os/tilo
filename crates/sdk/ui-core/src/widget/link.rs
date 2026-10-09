//! A hyperlink: primary ink with an underline, theme-driven, that opens a
//! URL when clicked, activated by keyboard (Enter / Space), or triggered by
//! an assistive-technology `Activate`.
//!
//! Activation hands the URL to the current host's [`crate::host::UriOpener`]
//! through [`crate::portal::open_uri`] (on the native desktop, the XDG
//! `OpenURI` portal with the `portal` feature). A host routes external opens
//! through its own opener; a test observes them with
//! [`crate::host::HostServices::recording`]. The most-recently-activated URL
//! is also recorded on [`Link::last_activated`] so a parent can confirm the
//! link fired.

use crate::widget::ModelPath;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Best-effort and non-blocking — a launch failure is logged, not surfaced.
/// See [`crate::portal::open_uri`] for how the URL reaches the desktop.
pub fn os_open(url: &str) {
    crate::portal::open_uri_detached(url);
}

/// Hand a URL to the current host's URI handler and report the outcome.
/// May block until the handler answers; call it from a worker thread.
///
/// # Errors
/// See [`crate::portal::open_uri`].
pub fn try_os_open(url: &str) -> std::io::Result<()> {
    crate::portal::open_uri(url)
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent widget-configuration or ephemeral-UI-state flag (hover/press/focus/enabled), not a state machine; grouping them would misrepresent orthogonal concerns and ripple through every accessor in this file"
)]
pub struct Link {
    url: String,
    /// Defaults to the URL; a caller can show a friendlier label via
    /// [`with_label`](Self::with_label).
    label: String,
    text_role: TextRole,
    /// The role resolved in the theme seen at `build_layout`, for `measure`.
    style: TextStyle,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
    enabled: bool,
    /// Lets a parent or test observe that the link fired without intercepting the opener.
    last_activated: Option<String>,
    /// An exported action the link runs instead of opening a URL.
    action: Option<crate::widget::ActionEmitter<()>>,
    dirty: bool,
    last_state: (bool, bool, bool),
}

impl Link {
    /// A link whose visible text is the URL itself.
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            label: url.to_string(),
            text_role: TextRole::Body,
            style: crate::tokens::typography::stock().style(TextRole::Body),
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
            enabled: true,
            last_activated: None,
            action: None,
            dirty: true,
            last_state: (false, false, false),
        }
    }

    /// A link that runs an exported action instead of opening a URL ("Search
    /// everywhere", "Open in Files"): link-shaped navigation inside Tilo.
    pub fn action(label: &str, binding: tilo_actions::ActionBinding<()>) -> Self {
        let mut link = Self::new("");
        link.label = label.to_string();
        link.action = Some(crate::widget::ActionEmitter::new(binding));
        link
    }

    /// Run `binding` when activated, instead of opening the URL.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.action = Some(crate::widget::ActionEmitter::new(binding));
        self
    }

    fn effectively_enabled(&self) -> bool {
        self.enabled
            && self
                .action
                .as_ref()
                .is_none_or(crate::widget::ActionEmitter::is_available)
    }

    /// The URL is still what opens and what the accessible value reports.
    #[must_use]
    pub fn with_label(mut self, label: &str) -> Self {
        self.label = label.to_string();
        self
    }

    /// Set the link in `role` (default [`TextRole::Body`]), usually the role
    /// of the text around it.
    #[must_use]
    pub fn with_text_role(mut self, role: TextRole) -> Self {
        self.text_role = role;
        self.style = crate::tokens::typography::stock().style(role);
        self
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn last_activated(&self) -> Option<&str> {
        self.last_activated.as_deref()
    }

    /// For a parent that routes input by hand instead of through
    /// [`handle_event`](Widget::handle_event). Returns `true` when the state
    /// changed, so the caller can request a redraw.
    pub fn set_hovered(&mut self, hovered: bool) -> bool {
        if hovered == self.is_hovered {
            false
        } else {
            self.is_hovered = hovered;
            true
        }
    }

    /// Bypasses taffy — mirrors
    /// [`TextInput::apply_layout_external`](crate::widget::TextInput) for
    /// parents that lay children out by hand.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    /// Single entry point shared by mouse, keyboard and AT so all three paths behave identically.
    pub fn activate(&mut self) {
        if !self.effectively_enabled() {
            return;
        }
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(()) {
                tracing::warn!(%error, "action-bound link refused its arguments");
            }
            return;
        }
        self.last_activated = Some(self.url.clone());
        os_open(&self.url);
    }
}

impl crate::widget::controls::ActionControls for Link {
    crate::widget::controls::optional_emitter_controls!(custody);
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        self.action
            .as_ref()
            .map(|action| vec![action.metadata(self.label.clone())])
            .unwrap_or_default()
    }
    fn has_action_control_bindings(&self) -> bool {
        self.action.is_some()
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            action.refresh_availability(allowed);
            self.dirty = true;
        }
    }
}

impl Widget for Link {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut style = theme.text(self.text_role);
        if self.label == self.url {
            // A link that shows its address is read as one.
            style = style.with_kind(crate::text_render::TextKind::Url);
        }
        if style != self.style {
            self.style = style;
            self.dirty = true;
        }
        let style = Style {
            size: Size {
                width: Dimension::auto(),
                height: Dimension::auto(),
            },
            flex_shrink: 0.0,
            flex_grow: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let (w, h) = text_renderer.measure_style(&self.label, None, &self.style);
        taffy::Size {
            width: w + 0.1,
            height: h,
        }
    }

    // Exact equality is intentional: `new_rect` and `self.rect` are both
    // widths/heights from finite taffy layout resolution, not independent
    // float computations prone to precision drift — any bit-level
    // difference means the layout changed and a redraw is due.
    #[allow(
        clippy::float_cmp,
        reason = "dirty-checking wants exact equality against the previous layout, not a fuzzy tolerance"
    )]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let new_rect = crate::widget::layout_rect(taffy, node, origin);
        if new_rect.width() != self.rect.width() || new_rect.height() != self.rect.height() {
            self.dirty = true;
        }
        self.rect = new_rect;
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `transform * Affine::translate(..)` call below composes 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        self.last_state = (self.is_hovered, self.is_pressed, self.is_focused);
        let color = theme.tokens.colors.text_primary;
        let style = theme.text(self.text_role);
        let (tw, th) = text_renderer.measure_style(&self.label, None, &style);
        let (tw, th) = (f64::from(tw), f64::from(th));
        let x0 = self.rect.x0;
        let y0 = self.rect.y0;

        text_renderer.draw_style(
            scene,
            &self.label,
            color,
            transform * Affine::translate((x0, y0)),
            None,
            &style,
        );

        // A touch thicker on hover so the link reads as "live".
        let uy = y0 + th - 1.5;
        let stroke_w = if self.is_hovered { 1.6 } else { 1.0 };
        scene.stroke(
            &Stroke::new(stroke_w),
            transform,
            color,
            None,
            &Line::new((x0, uy), (x0 + tw, uy)),
        );

        if self.is_focused {
            let pad = 2.0;
            let ring = RoundedRect::from_rect(
                Rect::new(x0 - pad, y0 - pad, x0 + tw + pad, y0 + th + pad),
                4.0,
            );
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                theme.border_focus,
                None,
                &ring,
            );
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || (self.is_hovered, self.is_pressed, self.is_focused) != self.last_state
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.enabled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hover = self.rect.contains(p);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        *ctx.handled = true;
                        if self.is_pressed {
                            *ctx.focus_target = Some(ctx.node_id);
                        } else {
                            // Release over the link → open it.
                            self.activate();
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Link"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.label.as_str())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.enabled && self.rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        self.effectively_enabled()
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }
    fn is_enabled(&self) -> bool {
        self.effectively_enabled()
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        if !self.enabled {
            return false;
        }
        match action {
            Action::Activate => {
                self.activate();
                true
            }
            _ => false,
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Link)
                .with_name(self.label.clone())
                .with_optional_value((!self.url.is_empty()).then(|| self.url.clone()))
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    disabled: !self.is_enabled(),
                    ..Default::default()
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use winit::event::{ElementState, MouseButton};

    #[test]
    fn link_opens_its_url_by_pointer_and_keyboard() {
        fn native_opener_rejects_nul_without_truncating_the_url() {
            let error = try_os_open("https://example.invalid/\0unexpected").unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }

        fn click_opens_url_through_the_opener_with_the_right_url() {
            let (services, recorder) = crate::host::HostServices::recording();
            let _host = crate::host::enter(&services);
            // Opening runs as background work; pump it so the open is observable.
            let work = crate::background::Pumped::new();
            let _work = work.background().enter();
            let mut link = Link::new("https://example.com/page");
            let (tree, node) = laid_out(&mut link);

            let device = winit::event::DeviceId::dummy();
            let mut sender = crate::testing::Dispatch::new().layout(&tree, node);
            let mut dispatch = |link: &mut Link, ev: WindowEvent| {
                sender.send(link, &ev);
            };

            dispatch(
                &mut link,
                WindowEvent::CursorMoved {
                    device_id: device,
                    position: (10.0, 10.0).into(),
                },
            );
            dispatch(
                &mut link,
                WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            assert!(
                recorder.opened_uris().is_empty(),
                "does not open until release"
            );
            dispatch(
                &mut link,
                WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );
            work.run_pending();

            assert_eq!(
                recorder.opened_uris(),
                ["https://example.com/page"],
                "opened exactly the link's URL on release"
            );
            assert_eq!(link.last_activated(), Some("https://example.com/page"));
        }

        fn keyboard_activate_opens_url() {
            let (services, recorder) = crate::host::HostServices::recording();
            let work = crate::background::Pumped::new();
            let _work = work.background().enter();
            let mut link = Link::new("https://w3.org");
            let mut doc = AutoCommit::new();
            let handled = crate::host::scoped(&services, || {
                link.perform_action(
                    &crate::a11y::Action::Activate,
                    &mut crate::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                )
            });
            assert!(handled, "Activate is handled");
            work.run_pending();
            assert_eq!(recorder.opened_uris(), ["https://w3.org"]);
        }

        native_opener_rejects_nul_without_truncating_the_url();
        click_opens_url_through_the_opener_with_the_right_url();
        keyboard_activate_opens_url();
    }

    /// Lay the link out at a known rect so it has a non-zero hit box.
    fn laid_out(link: &mut Link) -> (TaffyTree<()>, NodeId) {
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = link.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(400.0),
                height: taffy::AvailableSpace::Definite(40.0),
            },
        )
        .unwrap();
        link.apply_layout(&tree, node, Point::ZERO);
        // Deterministic hit rect (measure needs a text renderer we don't have here).
        link.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 20.0));
        (tree, node)
    }

    #[test]
    fn link_semantics_cursor_and_default_label() {
        fn renders_as_a_link_in_the_accessibility_tree() {
            use crate::a11y::Role;
            let link = Link::new("https://turing.org.uk").with_label("Turing");
            let node = Widget::accessibility(&link).expect("a link is not presentational");
            assert_eq!(node.role, Role::Link);
            assert_eq!(
                node.name.as_deref(),
                Some("Turing"),
                "the visible label is the accessible name"
            );
            assert_eq!(
                node.value.as_deref(),
                Some("https://turing.org.uk"),
                "the URL is the accessible value"
            );
            assert!(node.state.focusable, "a link is keyboard-focusable");
        }

        fn hovered_link_requests_the_pointer_cursor() {
            let mut link = Link::new("https://example.com");
            let _ = laid_out(&mut link);
            assert_eq!(
                Widget::cursor_shape(&link, Point::new(10.0, 10.0)),
                Some(winit::window::CursorIcon::Pointer),
                "pointer over the link asks for the pointing-hand cursor",
            );
            assert_eq!(
                Widget::cursor_shape(&link, Point::new(500.0, 500.0)),
                None,
                "off the link, no cursor claim",
            );
        }

        fn default_label_is_the_url() {
            let link = Link::new("https://example.com");
            assert_eq!(link.label(), "https://example.com");
            assert_eq!(link.url(), "https://example.com");
        }

        renders_as_a_link_in_the_accessibility_tree();
        hovered_link_requests_the_pointer_cursor();
        default_label_is_the_url();
    }
    #[test]
    fn an_action_link_runs_its_action_instead_of_opening() {
        let resource = tilo_actions::Resource::new("app:1", "app", "App").with_action(
            tilo_actions::ActionSpec::operation(
                "search",
                "Search everywhere",
                tilo_actions::OperationKind::View,
                "test.search",
                &(),
            )
            .unwrap(),
        );
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "search").unwrap();
        let mut link = Link::action("Search everywhere", binding);
        link.activate();
        assert!(link.last_activated().is_none(), "no URL opened");
        assert!(crate::widget::controls::take_control_invocation(&mut link).is_some());
        let node = Widget::accessibility(&link).expect("semantics");
        assert_eq!(node.value, None);
    }
}

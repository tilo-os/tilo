//! A hyperlink: your colour with an underline, theme-driven, that opens a URL when clicked,
//! activated by keyboard (Enter / Space), or triggered by an assistive-technology
//! `Activate`.

use tilo_ui_base::widget::ModelPath;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::TaffyTreeExt;
#[path = "link_component.rs"]
mod component;
use tilo_ui_base::widget::Widget;
pub use component::LinkProps;

/// The key of the link's "Copy link" action in its accessibility tree: the one child a link
/// adds beside itself, so a menu, a screen reader and an agent find the address to copy.
const COPY_CHILD: u64 = 0;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent widget-configuration or ephemeral-UI-state flag (hover/press/focus/enabled), not a state machine; grouping them would misrepresent orthogonal concerns and ripple through every accessor in this file"
)]
pub struct Link {
    url: String,
    label: String,
    text_role: TextRole,
    style: TextStyle,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
    enabled: bool,
    last_activated: Option<String>,
    action: Option<tilo_ui_base::widget::ActionEmitter<()>>,
    dirty: bool,
    last_state: (bool, bool, bool),
    click: Option<Box<dyn FnMut()>>,
    id: Option<String>,
    interaction: tilo_ui_base::interaction::InteractionEnvironment,
}

impl Link {
    /// A link whose visible text is the URL itself.
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            label: url.to_string(),
            text_role: TextRole::Body,
            style: tilo_ui_base::tokens::typography::stock().style(TextRole::Body),
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
            enabled: true,
            last_activated: None,
            action: None,
            dirty: true,
            last_state: (false, false, false),
            click: None,
            id: None,
            interaction: tilo_ui_base::interaction::InteractionEnvironment::default(),
        }
    }

    /// A link that runs an exported action instead of opening a URL ("Search everywhere",
    /// "Open in Files"): link-shaped navigation inside Tilo.
    pub fn action(label: &str, binding: tilo_actions::ActionBinding<()>) -> Self {
        let mut link = Self::new("");
        link.label = label.to_string();
        link.action = Some(tilo_ui_base::widget::ActionEmitter::new(binding));
        link
    }

    /// Run `binding` when activated, instead of opening the URL.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.action = Some(tilo_ui_base::widget::ActionEmitter::new(binding));
        self
    }

    fn effectively_enabled(&self) -> bool {
        self.enabled
            && self
                .action
                .as_ref()
                .is_none_or(tilo_ui_base::widget::ActionEmitter::is_available)
    }

    /// The URL is still what opens and what the accessible value reports.
    #[must_use]
    pub fn with_label(mut self, label: &str) -> Self {
        self.label = label.to_string();
        self
    }

    /// Set the link in `role` (default [`TextRole::Body`]), usually the role of the text
    /// around it.
    #[must_use]
    pub fn with_text_role(mut self, role: TextRole) -> Self {
        self.text_role = role;
        self.style = tilo_ui_base::tokens::typography::stock().style(role);
        self
    }

    /// Run `f` when the link is pressed, instead of opening its address.
    #[must_use]
    pub fn on_click(mut self, f: impl FnMut() + 'static) -> Self {
        self.click = Some(Box::new(f));
        self
    }

    /// Give it a stable identity, for tests, captures and agents.
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = Some(id.to_owned());
        self
    }

    /// Show another address in place.
    pub fn set_url(&mut self, url: &str) {
        url.clone_into(&mut self.url);
        if self.label == self.url {
            url.clone_into(&mut self.label);
        }
        self.dirty = true;
    }

    /// Say something else in place.
    pub fn set_label(&mut self, label: &str) {
        label.clone_into(&mut self.label);
        self.dirty = true;
    }

    /// What a press lands on: its face, grown to the host's minimum target about its
    /// centre, so a finger meets it however thin its line is.
    #[must_use]
    pub fn target(&self) -> Rect {
        self.interaction.minimum_target_rect(self.rect)
    }

    /// Whether what a press does is a view's event.
    #[must_use]
    pub fn has_action(&self) -> bool {
        self.click.is_some() || self.action.is_some()
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

    /// Put the address it goes to on the person's clipboard, as its "Copy link" action does:
    /// the address every other thing about the link reports, not the words it shows.
    pub fn copy_address(&self) {
        if !self.url.is_empty() {
            tilo_ui_base::text_selection::clipboard_copy(&self.url);
        }
    }

    /// The link's own "Copy link", for a link that goes somewhere: the row a context menu over it
    /// shows beside its owner's, and the child its accessibility tree publishes.
    fn copy_action(&self) -> Option<tilo_ui_base::widget::OwnAction> {
        (!self.url.is_empty()).then(|| {
            tilo_ui_base::widget::OwnAction::new(
                tilo_ui_base::i18n::widget_format("widget-copy-link", &[]),
                vec![COPY_CHILD],
            )
        })
    }

    /// Bypasses taffy — mirrors
    /// [`TextInput::apply_layout_external`](crate::widget::TextInput) for parents that lay
    /// children out by hand.
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
        if let Some(click) = &mut self.click {
            click();
            return;
        }
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(()) {
                tracing::warn!(%error, "action-bound link refused its arguments");
            }
            return;
        }
        self.last_activated = Some(self.url.clone());
        tilo_ui_base::platform::os_open(&self.url);
    }
}

impl tilo_ui_base::widget::controls::ActionControls for Link {
    tilo_ui_base::widget::controls::optional_emitter_controls!(ownership);
    fn control_action_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
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
    tilo_ui_base::described!();

    tilo_ui_base::provides!(dyn tilo_ui_base::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut style = theme.text(self.text_role);
        if self.label == self.url {
            style = style.with_kind(tilo_ui_base::text_render::TextKind::Url);
        }
        if style != self.style {
            self.style = style;
            self.dirty = true;
        }
        let style = Style {
            size: Size::auto(),
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

    #[allow(
        clippy::float_cmp,
        reason = "dirty-checking wants exact equality against the previous layout, not a fuzzy tolerance"
    )]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let new_rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if new_rect.width() != self.rect.width() || new_rect.height() != self.rect.height() {
            self.dirty = true;
        }
        self.rect = new_rect;
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `transform * Affine::translate(..)` call below composes 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.last_state = (self.is_hovered, self.is_pressed, self.is_focused);
        // A link reads in your colour, apart from the words around it; held down it dims, so a
        // press answers before the page it opens does.
        let colors = &theme.tokens.colors;
        let ink = colors.ink_on_any(
            &[colors.surfaces.content],
            colors.intent.accent_ink,
            tilo_ui_base::tokens::colors::ON_FILL_MIN_CONTRAST,
        );
        let color = if self.is_pressed {
            tilo_ui_base::tokens::colors::mix8(ink, colors.surfaces.content, 0.35)
        } else {
            ink
        };
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

        let uy = y0 + th - 1.5;
        let stroke_w = if self.is_hovered { 1.6 } else { 1.0 };
        scene.stroke(
            &Stroke::new(stroke_w),
            transform,
            color,
            None,
            &Line::new((x0, uy), (x0 + tw, uy)),
        );

        if self.is_focused && tilo_ui_base::focus_visible::shown() {
            // The ring every control wears, round the words.
            let pad = 2.0;
            crate::widget::paint_focus_ring(
                scene,
                transform,
                Rect::new(x0 - pad, y0 - pad, x0 + tw + pad, y0 + th + pad),
                f64::from(theme.tokens.radii.item),
                theme,
            );
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || (self.is_hovered, self.is_pressed, self.is_focused) != self.last_state
    }

    /// A link the frame left out (scrolled out of its view) shows nothing that could be stale:
    /// it rests as it stands, and is drawn whole once it comes back into view.
    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.last_state = (self.is_hovered, self.is_pressed, self.is_focused);
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if !self.enabled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                let hover = self.target().contains(p);
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
                            if self.effectively_enabled() {
                                *ctx.focus_target = Some(ctx.node_id);
                            }
                        } else if self.effectively_enabled() {
                            self.activate();
                        } else if let Some(action) = &mut self.action
                            && let Err(error) = action.press(())
                        {
                            // The person may not run it here: the press is sent, to be refused
                            // with why.
                            tracing::warn!(%error, "action-bound link refused its arguments");
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

    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
        self.is_hovered &= environment.supports_hover();
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
        if self.enabled && self.target().contains(point) {
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
        action: &tilo_ui_base::a11y::Action,
        key_path: &[u64],
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        use tilo_ui_base::a11y::Action;
        if !self.enabled {
            return false;
        }
        // The link's own "Copy link", the child its tree names: an agent or an assistive
        // technology copies what it goes to without opening it.
        if key_path == [COPY_CHILD] && matches!(action, Action::Activate) && !self.url.is_empty() {
            self.copy_address();
            return true;
        }
        match action {
            Action::Activate => {
                self.activate();
                true
            }
            _ => false,
        }
    }

    fn own_actions(&self) -> Vec<tilo_ui_base::widget::OwnAction> {
        self.copy_action().into_iter().collect()
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        let mut node = AccessNode::new(Role::Link)
            .with_name(self.label.clone())
            .with_bounds(self.target())
            .with_optional_value((!self.url.is_empty()).then(|| self.url.clone()))
            .with_state(
                AccessState::new()
                    .focusable(self.is_focusable())
                    .focused(self.is_focused())
                    .disabled(!self.is_enabled()),
            );
        // "Copy link" is an action of the link itself, so a context menu, a screen reader and
        // an agent copy its address the same way, and none of them opens it.
        if let Some(copy) = self.copy_action() {
            node = node.with_children(vec![
                AccessNode::new(Role::Button)
                    .with_key(COPY_CHILD)
                    .with_name(copy.name)
                    .with_state(AccessState::new().focusable(true)),
            ]);
        }
        let node = node.with_keyed_action_routes(self.id.as_deref().unwrap_or("link"));
        // A link its app names keeps that name across frames, so what it offers of its own (its
        // "Copy link") is published where an agent reaches it.
        Some(match &self.id {
            Some(id) => node.with_semantic_key(id),
            None => node,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    tilo_test_env::scenario! {
        fn a_link_opens_runs_and_describes_itself;

        phase link_opens_its_url_by_pointer_and_keyboard;
        {
            fn native_opener_rejects_nul_without_truncating_the_url() {
                let error = tilo_ui_base::platform::try_os_open("https://example.invalid/\0unexpected").unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            }

            native_opener_rejects_nul_without_truncating_the_url();
        }

        phase link_semantics_cursor_and_default_label;
        {
            fn renders_as_a_link_in_the_accessibility_tree() {
                use tilo_ui_base::a11y::Role;
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

            renders_as_a_link_in_the_accessibility_tree();
            hovered_link_requests_the_pointer_cursor();
        }

        phase an_action_link_runs_its_action_instead_of_opening;
        {
            let resource = tilo_actions::ResourceSpec::new("app:1", "app", "App").with_action(
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
            assert!(tilo_ui_base::widget::controls::take_control_invocation(&mut link).is_some());
            let node = Widget::accessibility(&link).expect("semantics");
            assert_eq!(node.value, None);
        }

        phase a_link_copies_its_address_as_its_own_action;
        {
            fn an_action_link_offers_no_copy_of_an_address_it_has_not_got() {
                let link = Link::new("");
                let node = Widget::accessibility(&link).expect("semantics");
                assert!(node.children.is_empty());
            }

            fn the_copy_puts_the_address_on_the_clipboard_and_opens_nothing() {
                use crate::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
                let clipboard = std::sync::Arc::new(InProcessClipboard::default());
                let services =
                    crate::process::ProcessServices::null().with_clipboard(clipboard.clone());
                crate::process::scoped(&services, || {
                    let mut doc = AutoCommit::new();
                    let mut link = Link::new("https://example.org/page").with_label("The page");
                    assert!(link.perform_action(
                        &tilo_ui_base::a11y::Action::Activate,
                        &[0],
                        &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                        &automerge::ROOT
                    ));
                    assert!(link.last_activated().is_none(), "copying opens no page");
                    assert_eq!(
                        clipboard.text(ClipboardTarget::Clipboard).as_deref(),
                        Some("https://example.org/page")
                    );
                });
            }

            an_action_link_offers_no_copy_of_an_address_it_has_not_got();
            the_copy_puts_the_address_on_the_clipboard_and_opens_nothing();
        }
    }

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
        link.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 20.0));
        (tree, node)
    }
}

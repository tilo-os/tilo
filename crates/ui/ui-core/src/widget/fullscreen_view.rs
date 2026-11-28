//! [`Fullscreen`] — a view that fills the audience's screen while it is presented: a slide, a
//! page shown to a room, a video played large.
//!
//! A view says whether it is presented ([`FullscreenProps::showing`]); the window it stands in
//! answers the host's question by asking down the tree, and the host fills the screen the
//! audience looks at (the largest external display before the built-in panel). While it is
//! presented, the view is the whole screen, so it carries its own way out: a control in its
//! leading top corner, announced as "Exit full screen", which reports [`FullscreenProps::exit`].
//! What the view shows is never left without a visible way back.
//!
//! A view can give the audience a view of its own ([`FullscreenProps::audience`]): where there is
//! a second screen, the host opens a window of the audience's on it while the view is presented,
//! and the window the view stands in stays the presenter's, showing the view (the speaker's notes,
//! the next slide, a timer), which the compositor moves to the presenter's screen. With one screen
//! the window itself fills it, as without one.

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use crate::{widget::Button, widget::ButtonRole, widget::ControlSize};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::MotionSource;
use tilo_ui_base::widget::identity::concrete::Decorator;
use tilo_ui_base::{
    component::Component, component::Described, component::Event, component::PartViews,
};
use tilo_ui_base::{fullscreen::AudiencePresentationRequest, text_render::TextRenderer};
use tilo_ui_base::{fullscreen::FullscreenRequest, fullscreen::FullscreenTarget};
use tilo_ui_base::{motion::Presence, motion::Transient};
use tilo_ui_base::{
    widget::AudiencePresentationProvider, widget::DrawChild, widget::Element, widget::EventCtx,
    widget::FullscreenProvider, widget::TaffyTreeExt, widget::Widget, widget::layout_rect,
};

/// Which screen the window fills while the view is presented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// Let the compositor choose.
    Auto,
    /// The audience's screen: the largest external display before the built-in panel.
    #[default]
    Largest,
    /// The primary screen.
    Primary,
    /// The primary screen someone plugged in, the built-in panel when there is none.
    PrimaryExternal,
}

impl Screen {
    /// The output the window is asked to fill for it.
    #[must_use]
    pub fn target(self) -> FullscreenTarget {
        match self {
            Self::Auto => FullscreenTarget::Auto,
            Self::Largest => FullscreenTarget::Largest,
            Self::Primary => FullscreenTarget::Primary,
            Self::PrimaryExternal => FullscreenTarget::PrimaryExternal,
        }
    }
}

tilo_ui_base::choice!(Screen, "Screen", {
    Auto => "auto", Largest => "largest", Primary => "primary",
    PrimaryExternal => "primary_external",
});

/// A view shown filling the audience's screen while it is presented.
#[derive(Component, Default)]
#[component(
    name = "Fullscreen",
    role = "Group",
    label = "name",
    not_in_gallery = "presenting it fills the audience's screen with the view it shows, over the window the gallery is read in, so a gallery page cannot show it without taking that window over"
)]
pub struct FullscreenProps {
    /// Its stable key.
    #[component(key)]
    pub key: String,
    /// What it is, for assistive technology and agents.
    pub name: Option<String>,
    /// The view is presented: the window fills the screen until it is not.
    #[component(required)]
    pub showing: bool,
    /// Which screen it fills: the audience's, unless said.
    pub screen: Screen,
    /// The view it presents.
    #[component(part(default))]
    pub body: Option<Element>,
    /// What the audience sees on a screen of its own while the view is presented, where there is
    /// a second screen: the window then stays the presenter's and shows the view. Without it, or
    /// on one screen, the window itself fills the audience's screen.
    #[component(part)]
    pub audience: Option<Element>,
    /// The person pressed the way out: the view stops presenting what it shows. The control that
    /// reports it, "Exit full screen", is drawn while the view is presented; a view leaves it
    /// unbound when the system alone ends the presentation.
    #[component(event)]
    pub exit: Event,
}

/// The name the way out is announced and drawn with.
fn exit_label() -> String {
    tilo_ui_base::i18n::widget_text("widget-exit-fullscreen")
}

/// The view `Fullscreen` presents: the window fills the audience's screen while `showing`, and
/// the view carries the way out.
pub struct Fullscreen {
    body: Element,
    /// What the audience's own window shows, when the view gives the audience a view of its own.
    audience: Option<Element>,
    /// The audience's own window is up: the view's window stays the presenter's.
    audience_shown: bool,
    /// The part [`PartViews`] changes now: the audience's view rather than the body.
    editing_audience: bool,
    showing: bool,
    screen: Screen,
    /// The way out, drawn over the view's leading top corner while it is presented.
    control: Button,
    /// What the control reports: kept behind a cell so a view that rebinds `exit` keeps the
    /// same control, and the hand that was on it.
    exit: Event,
    /// The node the control stands on, while the view is presented.
    control_node: Option<NodeId>,
    /// The view's box, from the last layout.
    rect: Rect,
    /// The theme of the last build: where the control stands and how it is read.
    theme: Theme,
    /// How far the way out has come in, and whether it is on its way.
    presence: Presence,
}

impl Fullscreen {
    fn no_body() -> Element {
        Box::new(tilo_ui_base::widget::Spacer::new())
    }

    /// Put `view` in the part being changed.
    fn put(&mut self, view: Element) {
        if self.editing_audience {
            self.audience = Some(view);
        } else {
            self.body = view;
        }
    }

    /// The way out: a small control in the leading top corner, named "Exit full screen".
    fn control(key: &str) -> Button {
        Button::new(&format!("{key}.exit"), &exit_label())
            .with_icon(tilo_ui_base::icons::Icon::Minimize)
            .role(ButtonRole::Secondary)
            .control_size(ControlSize::Small)
    }

    /// The margin between the view's edge and the way out.
    fn inset(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.related)
    }

    /// Put the way out in the view's leading top corner, at the width its words take.
    fn place_control(&mut self) {
        if !self.showing || self.rect.width() <= 0.0 {
            return;
        }
        let inset = Self::inset(&self.theme);
        let theme = self.theme.clone();
        let size = tilo_ui_base::text_render::with_layout_renderer(&theme, |text| {
            Widget::measure(
                &mut self.control,
                Size {
                    width: None,
                    height: None,
                },
                Size {
                    width: AvailableSpace::MaxContent,
                    height: AvailableSpace::MaxContent,
                },
                text,
            )
        });
        let width = f64::from(size.width).min(self.rect.width());
        let height = f64::from(size.height).min(self.rect.height());
        let x0 = self.rect.x0 + inset;
        let y0 = self.rect.y0 + inset;
        self.control
            .apply_layout_external(Rect::new(x0, y0, x0 + width, y0 + height));
    }

    /// How far the way out is drawn, at `now`: it comes in as the view does.
    fn openness(&mut self, theme: &Theme) -> f64 {
        let now = tilo_ui_base::motion::frame_now();
        if self.presence.is_open() != self.showing {
            self.presence.show(
                self.showing,
                Transient::CONFIRMATION,
                theme.motion(),
                MotionSource::Keyboard,
                now,
            );
        }
        if !self.showing {
            self.control.set_focus(false);
        }
        self.presence.progress(now)
    }
}

impl Decorator for Fullscreen {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.body.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.body.as_mut()
    }

    /// The fullscreen intent is this view's own; everything else it knows is the view's it
    /// presents.
    fn provide<'a>(&'a self, request: &mut tilo_ui_base::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn FullscreenProvider>() {
            request.provide::<dyn FullscreenProvider>(self);
            return;
        }
        if request.wants::<dyn AudiencePresentationProvider>() && self.audience.is_some() {
            request.provide::<dyn AudiencePresentationProvider>(self);
            return;
        }
        self.body.provide(request);
    }

    fn provide_mut<'a>(
        &'a mut self,
        request: &mut tilo_ui_base::widget::capability::RequestMut<'a, '_>,
    ) {
        if request.wants::<dyn FullscreenProvider>() {
            request.provide::<dyn FullscreenProvider>(self);
            return;
        }
        if request.wants::<dyn AudiencePresentationProvider>() && self.audience.is_some() {
            request.provide::<dyn AudiencePresentationProvider>(self);
            return;
        }
        self.body.provide_mut(request);
    }

    /// While it presents, the view fills the screen it is shown on; the way out stands over it,
    /// out of the flow. Not presenting, it is the view it holds and nothing more: a view says
    /// what shows at rest, and fullscreen changes none of it.
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        let body = self.body.build_layout(taffy, theme, node_map);
        if !self.showing {
            self.control_node = None;
            self.presence.snap(false);
            return body;
        }
        let control = self.control.build_layout(taffy, theme, node_map);
        if let Ok(style) = taffy.style(control) {
            let mut style = style.clone();
            style.position = taffy::Position::Absolute;
            style.size = Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            };
            let _ = taffy.set_style(control, style);
        }
        self.control_node = Some(control);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[body, control], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
        }
        if !self.showing {
            self.body.apply_layout(taffy, node, origin);
            return;
        }
        let children = taffy.children(node).unwrap_or_default();
        if let Some(&body) = children.first() {
            self.body
                .apply_layout(taffy, body, Point::new(rect.x0, rect.y0));
        }
        self.place_control();
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.body.draw_child(scene, transform, text, theme, &env);
        let openness = self.openness(theme);
        if openness <= 0.0 {
            return;
        }
        let rect = self.control.get_rect();
        if openness >= 1.0 {
            self.control.draw_child(scene, transform, text, theme, &env);
            return;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "an opacity between 0 and 1"
        )]
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            openness as f32,
            transform,
            &rect,
        );
        self.control.draw_child(scene, transform, text, theme, &env);
        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if self.showing
            && let Some(node) = self.control_node
        {
            self.control
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                return;
            }
        }
        self.body.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&(dyn Widget + '_)> {
        let mut children: Vec<&(dyn Widget + '_)> = vec![self.body.as_ref()];
        if self.showing {
            children.push(&self.control);
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let showing = self.showing;
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![self.body.as_mut()];
        if showing {
            children.push(&mut self.control);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.body.as_ref());
        if self.showing {
            visitor(&self.control);
        }
    }

    fn for_each_child_in_clip(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.body.as_ref());
        if self.showing {
            visitor(&self.control);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.presence.is_moving(tilo_ui_base::motion::frame_now())
            || self.body.needs_animation_frame()
            || (self.showing && self.control.needs_animation_frame())
    }

    fn needs_redraw(&self) -> bool {
        self.showing && self.presence.is_moving(tilo_ui_base::motion::frame_now())
            || self.body.needs_redraw()
            || (self.showing && self.control.needs_redraw())
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.body.collect_damage(regions);
        if self.showing && self.presence.progress(tilo_ui_base::motion::frame_now()) > 0.0 {
            regions.push(self.control.get_rect());
        }
    }

    fn mark_children_drawn(&mut self) {
        self.body.mark_drawn();
        if self.showing {
            self.control.mark_drawn();
        }
    }
}

impl FullscreenProvider for Fullscreen {
    /// Presented, the window fills the audience's screen, unless the audience has a window of its
    /// own: the presenter's window then stays a window, which the compositor moves to the
    /// presenter's screen.
    fn fullscreen_intent(&self) -> Option<bool> {
        Some(self.showing && !self.audience_shown)
    }

    fn fullscreen_params(&self) -> FullscreenRequest {
        FullscreenRequest::mirror(self.screen.target())
    }
}

impl AudiencePresentationProvider for Fullscreen {
    fn audience_presentation_request(&self) -> Option<AudiencePresentationRequest> {
        (self.showing && self.audience.is_some())
            .then(|| AudiencePresentationRequest::new(self.screen.target()))
    }

    fn on_audience_presentation_changed(&mut self, active: bool) {
        self.audience_shown = active;
    }

    /// The audience's view, laid out on the audience's window and drawn there.
    fn draw_audience_presentation(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) -> bool {
        if !self.showing {
            return false;
        }
        let Some(audience) = self.audience.as_mut() else {
            return false;
        };
        if !tilo_ui_base::widget::layout::lay_out_alone(
            audience.as_mut(),
            rect.origin(),
            (rect.width(), rect.height()),
            theme,
            text_renderer,
        ) {
            return false;
        }
        audience.draw_child(
            scene,
            transform,
            text_renderer,
            theme,
            &tilo_ui_base::draw::DrawEnv::default(),
        );
        true
    }

    /// The person closed the audience's window: the presentation ends, as the way out ends it.
    fn on_audience_presentation_closed(&mut self) {
        self.audience_shown = false;
        if self.showing
            && let Some(mut exit) = self.exit.clone().handler()
        {
            exit();
        }
    }
}

/// What an event reports, by identity: two events of the same press run the same way.
fn event_identity(event: &Event) -> String {
    match event {
        Event::None => String::new(),
        Event::Action(binding) => format!("action:{:?}", binding.address()),
        Event::View(intent) => format!("view:{}:{}", intent.key(), intent.event()),
    }
}

impl Described for Fullscreen {
    type Props = FullscreenProps;

    fn from_props(key: &str, props: FullscreenProps) -> Self {
        let mut control = Self::control(key);
        if let Some(pressed) = props.exit.clone().handler() {
            control = control.on_click(pressed);
        }
        Self {
            body: props.body.unwrap_or_else(Self::no_body),
            audience: props.audience,
            audience_shown: false,
            editing_audience: false,
            showing: props.showing,
            screen: props.screen,
            control,
            exit: props.exit,
            control_node: None,
            rect: Rect::ZERO,
            theme: Theme::default(),
            presence: if props.showing {
                Presence::opened()
            } else {
                Presence::closed()
            },
        }
    }

    fn set_props(&mut self, props: FullscreenProps) -> bool {
        if event_identity(&props.exit) != event_identity(&self.exit)
            || props.audience.is_some() != self.audience.is_some()
        {
            return false;
        }
        self.showing = props.showing;
        self.screen = props.screen;
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        self.editing_audience = match part {
            "body" => false,
            "audience" if self.audience.is_some() => true,
            _ => return None,
        };
        Some(self as &mut dyn PartViews)
    }
}

/// The view of the part being changed: the body, or the audience's.
impl PartViews for Fullscreen {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index != 0 {
            return None;
        }
        if self.editing_audience {
            self.audience.as_mut()
        } else {
            Some(&mut self.body)
        }
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.put(view);
    }

    fn remove(&mut self, _index: usize) {
        self.put(Self::no_body());
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.put(view);
    }
}

#[cfg(test)]
#[path = "fullscreen_tests.rs"]
mod tests;

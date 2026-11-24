//! [`Fullscreen`] — a view that fills the audience's screen while it is presented: a slide, a
//! page shown to a room, a video played large.
//!
//! A view says whether it is presented ([`FullscreenProps::showing`]); the window it stands in
//! answers the host's question by asking down the tree, and the host fills the screen the
//! audience looks at (the largest external display before the built-in panel). While it is
//! presented, the view is the whole screen, so it carries its own way out: a control in its
//! leading top corner, announced as "Exit full screen", which reports [`FullscreenProps::exit`].
//! What the view shows is never left without a visible way back.

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use crate::component::{Component, Described, Event, PartViews};
use crate::fullscreen::{FullscreenRequest, FullscreenTarget};
use crate::motion::{Presence, Transient};
use crate::theme::Theme;
use crate::tokens::MotionSource;
use crate::widget::decorator::concrete::Decorator;
use crate::widget::{
    Button, ButtonRole, ControlSize, DrawChild, Element, EventCtx, FullscreenProvider,
    TaffyTreeExt, Widget, layout_rect,
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

crate::component::choice!(Screen, "Screen", {
    Auto => "auto", Largest => "largest", Primary => "primary",
    PrimaryExternal => "primary_external",
});

/// A view shown filling the audience's screen while it is presented.
#[derive(Component, Default)]
#[component(name = "Fullscreen", role = "Group", label = "name")]
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
    /// The person pressed the way out: the view stops presenting what it shows. The control that
    /// reports it, "Exit full screen", is drawn while the view is presented; a view leaves it
    /// unbound when the system alone ends the presentation.
    #[component(event)]
    pub exit: Event,
}

/// The name the way out is announced and drawn with.
fn exit_label() -> String {
    crate::i18n::widget_text("widget-exit-fullscreen")
}

/// The view `Fullscreen` presents: the window fills the audience's screen while `showing`, and
/// the view carries the way out.
pub struct Fullscreen {
    body: Element,
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
        Box::new(crate::widget::Spacer::new())
    }

    fn put(&mut self, body: Element) {
        self.body = body;
    }

    /// The way out: a small control in the leading top corner, named "Exit full screen".
    fn control(key: &str) -> Button {
        Button::new(&format!("{key}.exit"), &exit_label())
            .with_icon(crate::icons::Icon::Minimize)
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
        let size = crate::text_render::with_layout_renderer(&theme, |text| {
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
        let now = crate::motion::frame_now();
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
    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn FullscreenProvider>() {
            request.provide::<dyn FullscreenProvider>(self);
            return;
        }
        self.body.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn FullscreenProvider>() {
            request.provide::<dyn FullscreenProvider>(self);
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
        let node = taffy.widget_node(style, &[body, control], node_map, self);
        node
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

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
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
        self.presence.is_moving(crate::motion::frame_now())
            || self.body.needs_animation_frame()
            || (self.showing && self.control.needs_animation_frame())
    }

    fn needs_redraw(&self) -> bool {
        self.showing && self.presence.is_moving(crate::motion::frame_now())
            || self.body.needs_redraw()
            || (self.showing && self.control.needs_redraw())
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.body.collect_damage(regions);
        if self.showing && self.presence.progress(crate::motion::frame_now()) > 0.0 {
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
    fn fullscreen_intent(&self) -> Option<bool> {
        Some(self.showing)
    }

    fn fullscreen_params(&self) -> FullscreenRequest {
        FullscreenRequest::mirror(self.screen.target())
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
        if event_identity(&props.exit) != event_identity(&self.exit) {
            return false;
        }
        self.showing = props.showing;
        self.screen = props.screen;
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "body").then_some(self as &mut dyn PartViews)
    }
}

impl PartViews for Fullscreen {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        (index == 0).then_some(&mut self.body)
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

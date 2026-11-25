//! The views a shell runs where its windows asked for them ([`crate::window_share::Embed`]).
//!
//! - **A miniature** is its app laid out at a window's size and drawn scaled into its place. It
//!   takes no input and stands outside the shell's tree.
//! - **A view a person works in** ([`crate::window_share::Embed::working`]) stands in the shell's
//!   own tree, laid out at its place's size where its window shows it. The pointer, the keyboard,
//!   text input, focus, undo and what a screen reader reads therefore reach it as they reach a
//!   window, with nothing made for it alone. A press in it gives it the keyboard and a press
//!   outside takes the keyboard back; the wheel is its own only while it has the keyboard, so a
//!   conversation scrolls past a document it shows until the person works in the document.
//!
//! A view worked in may show views of its own (a conversation shown in another app's window, with
//! a document shared in it): they stand where it does.

use automerge::ObjId;
use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, TouchPhase, WindowEvent};

use super::{LiveView, VIEW, Views, placed as place_of, view_transform};
use crate::hosted_window::HostedWindow;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::widget::{
    AppModel, DrawChild, Edits, EventCtx, ModelPath, StructureChange, TaffyTreeExt, TileContent,
    TileHost, Widget,
};
use crate::window_share::{EmbedState, ResourceView};

/// How many views deep a view worked in may show views of its own.
const NESTED: u8 = 2;

/// A window that asks for views, as its shell shows it.
pub struct Asking {
    /// The window: it says which views it wants and hears how each is coming along.
    pub window: HostedWindow,
    /// Where the window's layout stands in the shell's coordinates.
    pub origin: Vec2,
    /// What of the window's content shows.
    pub content: Rect,
    /// How many views deep the window stands: 0 for a window of the shell's own.
    depth: u8,
}

impl Asking {
    /// A window of the shell's own.
    #[must_use]
    pub fn new(window: HostedWindow, origin: Vec2, content: Rect) -> Self {
        Self {
            window,
            origin,
            content,
            depth: 0,
        }
    }
}

/// One view as its shell runs it: its app and, while a person works in it, its place in the
/// shell's tree.
#[crate::component::not_a_component(shell, "a view the shell runs at its window's place")]
pub struct Shown {
    host: TileHost,
    /// What the embedding app calls what the view shows.
    label: String,
    /// The size its place gives it while a person works in it.
    size: (f64, f64),
    rect: Rect,
}

impl LiveView for Shown {
    fn state(&self) -> EmbedState {
        self.host.state()
    }

    fn retries(&self) -> bool {
        self.host.retries()
    }
}

impl<V: LiveView + ?Sized> LiveView for Box<V> {
    fn state(&self) -> EmbedState {
        (**self).state()
    }

    fn retries(&self) -> bool {
        (**self).retries()
    }
}

impl Shown {
    /// The view's app.
    #[must_use]
    pub fn host(&self) -> &TileHost {
        &self.host
    }
}

impl Widget for Shown {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.host.build_layout(taffy, theme, node_map);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a place in a window is far inside f32's range"
        )]
        let size = taffy::Size {
            width: taffy::Dimension::length(self.size.0 as f32),
            height: taffy::Dimension::length(self.size.1 as f32),
        };
        // Out of the shell's own flow, at its corner: where it stands is its window's to say, and
        // is applied when the layout is ([`Stage::apply_layout`]).
        let style = Style {
            display: taffy::Display::Flex,
            position: taffy::Position::Absolute,
            inset: taffy::Rect {
                left: taffy::LengthPercentageAuto::length(0.0),
                top: taffy::LengthPercentageAuto::length(0.0),
                right: taffy::LengthPercentageAuto::auto(),
                bottom: taffy::LengthPercentageAuto::auto(),
            },
            size,
            ..Style::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(&child) = taffy.children(node).unwrap_or_default().first() {
            self.host
                .apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.host.draw_child(scene, transform, text, theme, &env);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let Some(&child) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() {
            self.host
                .handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.host]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.host]
    }

    /// The view adds no structure of its own: a change its app carries out in place stays in
    /// place.
    fn take_structure_change(&mut self) -> StructureChange {
        StructureChange::of_children(self.children_mut())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "EmbeddedView"
    }

    /// What the view shows, named before what it holds: the embedding app's name for it, or its
    /// app's.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let name = if self.label.is_empty() {
            match self.host.content() {
                TileContent::App(app) => app.app_title().to_owned(),
                _ => String::new(),
            }
        } else {
            self.label.clone()
        };
        let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Group)
            .with_hashed_key(("embedded-view", &name))
            .with_bounds(self.rect);
        if !name.is_empty() {
            node = node.with_name(name);
        }
        Some(node)
    }
}

/// Where one view is drawn, in the shell's coordinates.
struct Place {
    view: ResourceView,
    key: String,
    /// The window that asked, by its place in [`Stage::asking`].
    asker: usize,
    rect: Rect,
    clip: Rect,
    /// Whether the person works in the view here: the first place that asks so of a view.
    working: bool,
    label: String,
}

/// A view a person works in.
struct Working {
    view: ResourceView,
    size: (f64, f64),
    /// Its node in the shell's tree, once the shell built it.
    node: Option<NodeId>,
    /// Where its layout was last applied.
    laid: Point,
}

/// The views a shell runs for the windows that ask for them.
#[derive(Default)]
pub struct Stage {
    views: Views<Box<Shown>>,
    /// The windows that asked, and the views worked in that ask for views of their own.
    asking: Vec<Asking>,
    places: Vec<Place>,
    /// The views worked in, in the order they were asked for.
    working: Vec<Working>,
    /// The view worked in that has the keyboard.
    keyboard: Option<ResourceView>,
    /// The view worked in a held press began in: the press stays its own until it ends.
    pressed: Option<ResourceView>,
    pointer: Point,
    structure_dirty: bool,
    workspace: Option<String>,
}

/// Whether `widget` or anything under it has the keyboard.
fn subtree_focused(widget: &dyn Widget) -> bool {
    let mut focused = widget.is_focused();
    if !focused {
        widget.for_each_child(&mut |child| focused = focused || subtree_focused(child));
    }
    focused
}

/// The transform that draws what is laid out in `from` into `into`, through `transform`: as large
/// as fits, keeping its proportions, from `into`'s corner.
fn fit(transform: Affine, from: Rect, into: Rect) -> Affine {
    let scale = (into.width() / from.width().max(1.0)).min(into.height() / from.height().max(1.0));
    transform
        .pre_translate(Vec2::new(into.x0, into.y0))
        .pre_scale(scale)
        .pre_translate(Vec2::new(-from.x0, -from.y0))
}

impl Stage {
    /// The workspace the views' apps are told they are in.
    pub fn set_workspace(&mut self, workspace: Option<String>) {
        self.workspace = workspace;
    }

    /// Run the views the `asking` windows ask for at `now`: start the ones newly asked for with
    /// `start`, drop the ones no longer asked for, tell each window how its views are coming
    /// along, and update every view. Whether any view changed.
    pub fn follow(
        &mut self,
        mut asking: Vec<Asking>,
        now: Instant,
        mut start: impl FnMut(&ResourceView) -> Result<TileHost, String>,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        let mut places: Vec<Place> = Vec::new();
        let mut at = 0;
        while at < asking.len() {
            let Some(asked) = asking.get(at) else {
                break;
            };
            let (origin, content, depth) = (asked.origin, asked.content, asked.depth);
            for embed in asked.window.embeds() {
                let (rect, clip) = place_of(&embed, origin, content);
                let working = embed.working
                    && !places
                        .iter()
                        .any(|place| place.working && place.view == embed.view);
                // A view worked in asks for views of its own where it stands.
                if working
                    && depth < NESTED
                    && let Some(window) = self
                        .views
                        .get(&embed.view)
                        .and_then(|shown| shown.host.hosted_window())
                    && let Some(laid) = self
                        .working
                        .iter()
                        .find(|known| known.view == embed.view)
                        .map(|known| known.laid)
                {
                    asking.push(Asking {
                        window: window.clone(),
                        origin: rect.origin() - laid,
                        content: clip,
                        depth: depth.saturating_add(1),
                    });
                }
                places.push(Place {
                    view: embed.view,
                    key: embed.key,
                    asker: at,
                    rect,
                    clip,
                    working,
                    label: embed.label,
                });
            }
            at = at.saturating_add(1);
        }

        let workspace = self.workspace.clone();
        let mut started: Vec<ResourceView> = Vec::new();
        let followed = self
            .views
            .follow(places.iter().map(|place| &place.view), now, |view| {
                let mut host = start(view)?;
                // What its app declares for its window (the views it asks for itself) is kept
                // with it.
                if host.hosted_window().is_none() {
                    let window = HostedWindow::new();
                    window.set_workspace(workspace.clone());
                    host.set_hosted_window(window);
                }
                started.push(view.clone());
                Ok(Box::new(Shown {
                    host,
                    label: String::new(),
                    size: (0.0, 0.0),
                    rect: Rect::ZERO,
                }))
            });
        let mut changed = followed.changed;
        drop(followed);

        for place in &places {
            if let (Some(state), Some(asked)) =
                (self.views.state(&place.view), asking.get(place.asker))
            {
                asked.window.set_embed_state(&place.key, state);
            }
        }

        let mut working: Vec<Working> = Vec::new();
        for place in places.iter().filter(|place| place.working) {
            let Some(shown) = self.views.get_mut(&place.view) else {
                continue;
            };
            let size = (
                place.rect.width().round().max(1.0),
                place.rect.height().round().max(1.0),
            );
            shown.size = size;
            shown.label.clone_from(&place.label);
            let known = self
                .working
                .iter()
                .find(|known| known.view == place.view && known.size == size);
            working.push(Working {
                view: place.view.clone(),
                size,
                node: known.and_then(|known| known.node),
                laid: known.map_or(place.rect.origin(), |known| known.laid),
            });
        }
        let same = working.len() == self.working.len()
            && working
                .iter()
                .zip(&self.working)
                .all(|(now, before)| now.view == before.view && now.size == before.size)
            && !working.iter().any(|now| started.contains(&now.view));
        if !same {
            // A view began or stopped being worked in, changed its size, or its app was started
            // again: the shell's tree is built with it.
            self.structure_dirty = true;
            changed = true;
        } else if working.iter().any(|now| {
            places.iter().any(|place| {
                place.working && place.view == now.view && place.rect.origin() != now.laid
            })
        }) {
            // Its window moved it (a conversation scrolled): it is laid out again where it stands
            // now.
            crate::widget::note_layout_moved();
        }
        self.working = working;
        for held in [&mut self.keyboard, &mut self.pressed] {
            if held
                .as_ref()
                .is_some_and(|view| !self.working.iter().any(|now| &now.view == view))
            {
                *held = None;
            }
        }

        for (view, shown) in self.views.running_mut() {
            // A view worked in is synced with the shell's tree ([`Self::sync`]).
            if !self.working.iter().any(|now| &now.view == view) {
                shown.host.sync(state, path);
            }
            changed |= shown.host.update(state, path);
        }
        self.places = places;
        self.asking = asking;
        changed
    }

    /// Pull model state into the views worked in, with the shell's own tree.
    pub fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        for working in &self.working {
            if let Some(shown) = self.views.get_mut(&working.view) {
                shown.host.sync(state, path);
            }
        }
    }

    /// What the miniatures wrote this frame goes to their records; a view worked in settles with
    /// the shell's tree.
    pub fn settle(&mut self, edits: &mut Edits<'_>, path: &ObjId) {
        for (view, shown) in self.views.running_mut() {
            if !self.working.iter().any(|now| &now.view == view) {
                shown.host.settle(edits, path);
            }
        }
    }

    /// Build the views worked in into the shell's tree: their nodes, for the shell to hang under
    /// its own.
    pub fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> Vec<NodeId> {
        self.structure_dirty = false;
        let mut nodes = Vec::new();
        for working in &mut self.working {
            working.node = self
                .views
                .get_mut(&working.view)
                .map(|shown| shown.build_layout(taffy, theme, node_map));
            nodes.extend(working.node);
        }
        nodes
    }

    /// Lay each view worked in out where its window shows it.
    pub fn apply_layout(&mut self, taffy: &TaffyTree<()>) {
        for working in &mut self.working {
            let Some(node) = working.node else {
                continue;
            };
            let Some(place) = self
                .places
                .iter()
                .find(|place| place.working && place.view == working.view)
            else {
                continue;
            };
            let Some(shown) = self.views.get_mut(&working.view) else {
                continue;
            };
            let origin = place.rect.origin();
            let at = taffy.layout(node).map_or(Point::ZERO, |layout| {
                Point::new(f64::from(layout.location.x), f64::from(layout.location.y))
            });
            shown.apply_layout(taffy, node, Point::new(origin.x - at.x, origin.y - at.y));
            working.laid = origin;
        }
    }

    /// The views worked in, as the shell's tree holds them.
    #[must_use]
    pub fn working(&self) -> Vec<&dyn Widget> {
        self.working
            .iter()
            .filter_map(|working| self.views.get(&working.view))
            .map(|shown| &**shown as &dyn Widget)
            .collect()
    }

    /// The views worked in, to drive.
    pub fn working_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let working = &self.working;
        self.views
            .running_mut()
            .filter(|(view, _)| working.iter().any(|now| &now.view == *view))
            .map(|(_, shown)| &mut **shown as &mut (dyn Widget + '_))
            .collect()
    }

    /// A change to the views worked in that the shell's tree is built again for, or one a view's
    /// app carries out in place.
    pub fn take_structure_change(&mut self) -> StructureChange {
        let mut change = StructureChange::of(std::mem::take(&mut self.structure_dirty));
        for working in &self.working {
            if let Some(shown) = self.views.get_mut(&working.view) {
                change = change.join(shown.take_structure_change());
            }
        }
        change
    }

    /// The view worked in that shows at `point`.
    fn working_at(&self, point: Point) -> Option<ResourceView> {
        self.places
            .iter()
            .rev()
            .find(|place| {
                place.working
                    && place.clip.contains(point)
                    && place.rect.contains(point)
                    && self
                        .working
                        .iter()
                        .any(|working| working.view == place.view && working.node.is_some())
            })
            .map(|place| place.view.clone())
    }

    /// Whether any of `view` shows where a person works in it.
    fn shows(&self, view: &ResourceView) -> bool {
        self.places
            .iter()
            .any(|place| place.working && &place.view == view && !place.clip.is_zero_area())
    }

    /// The view worked in that has the keyboard.
    #[must_use]
    pub fn keyboard(&self) -> Option<&ResourceView> {
        self.keyboard.as_ref()
    }

    /// The app of the view that has the keyboard.
    #[must_use]
    pub fn keyboard_host(&self) -> Option<&TileHost> {
        self.keyboard
            .as_ref()
            .and_then(|view| self.views.get(view))
            .map(|shown| &shown.host)
    }

    /// Whether the wheel at the pointer is the view's that has the keyboard.
    #[must_use]
    pub fn takes_wheel(&self) -> bool {
        self.keyboard.is_some() && self.working_at(self.pointer) == self.keyboard
    }

    fn set_keyboard(&mut self, view: Option<ResourceView>) {
        if self.keyboard == view {
            return;
        }
        if let Some(old) = self.keyboard.take()
            && let Some(shown) = self.views.get_mut(&old)
        {
            shown.host.set_focused(false);
        }
        if let Some(shown) = view.as_ref().and_then(|view| self.views.get_mut(view)) {
            shown.host.set_focused(true);
        }
        self.keyboard = view;
    }

    /// Hand `event` to `view`'s app. Its answer is the event's own, or, with `seen`, kept from the
    /// event and written there.
    fn offer(
        &mut self,
        view: &ResourceView,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
        seen: Option<&mut bool>,
    ) {
        let Some(node) = self
            .working
            .iter()
            .find(|working| &working.view == view)
            .and_then(|working| working.node)
        else {
            return;
        };
        let Some(shown) = self.views.get_mut(view) else {
            return;
        };
        match seen {
            None => shown.handle_event(event, &mut ctx.reborrow(ctx.path, node)),
            Some(handled) => shown.handle_event(
                event,
                &mut EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state: ctx.state.reborrow(),
                    path: ctx.path,
                    handled,
                    focus_target: &mut *ctx.focus_target,
                    node_id: node,
                    taffy: ctx.taffy,
                },
            ),
        }
    }

    /// Hand `event` to the view worked in that it belongs to. Whether it was the view's alone:
    /// the shell then keeps it from its windows.
    pub fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if self.working.is_empty() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = ctx.logical(position);
                let Some(view) = self
                    .pressed
                    .clone()
                    .or_else(|| self.working_at(self.pointer))
                else {
                    return false;
                };
                // The window under the view still hears where the pointer is, so what it showed
                // under the pointer before no longer does; a drag that began in the view is the
                // view's alone.
                let mut seen = false;
                self.offer(&view, event, ctx, Some(&mut seen));
                self.pressed.is_some()
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {
                let hit = self.working_at(self.pointer);
                self.set_keyboard(hit.clone());
                let Some(view) = hit else {
                    return false;
                };
                self.pressed = Some(view.clone());
                self.offer(&view, event, ctx, None);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                let Some(view) = self.pressed.take() else {
                    return false;
                };
                self.offer(&view, event, ctx, None);
                true
            }
            WindowEvent::MouseWheel { .. } => {
                // The wheel is the view's only while the person works in it: until then it moves
                // what the view stands in.
                let Some(view) = self.keyboard.clone().filter(|_| self.takes_wheel()) else {
                    return false;
                };
                self.offer(&view, event, ctx, None);
                true
            }
            WindowEvent::Touch(touch) => {
                let point = ctx.logical(touch.location);
                match touch.phase {
                    TouchPhase::Started => {
                        let hit = self.working_at(point);
                        self.set_keyboard(hit.clone());
                        let Some(view) = hit else {
                            return false;
                        };
                        self.pressed = Some(view.clone());
                        self.offer(&view, event, ctx, None);
                        true
                    }
                    TouchPhase::Moved => {
                        let Some(view) = self.pressed.clone() else {
                            return false;
                        };
                        self.offer(&view, event, ctx, None);
                        true
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        let Some(view) = self.pressed.take() else {
                            return false;
                        };
                        self.offer(&view, event, ctx, None);
                        true
                    }
                }
            }
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                // The keyboard went into a view some other way than a press (the Tab key, a
                // screen reader): the view has it.
                let focused = self
                    .working
                    .iter()
                    .find(|working| {
                        self.views
                            .get(&working.view)
                            .is_some_and(|shown| subtree_focused(&shown.host))
                    })
                    .map(|working| working.view.clone());
                if focused.is_some() {
                    self.set_keyboard(focused);
                }
                let Some(view) = self.keyboard.clone().filter(|view| self.shows(view)) else {
                    return false;
                };
                self.offer(&view, event, ctx, None);
                // A key the view left alone is its window's.
                *ctx.handled
            }
            _ => false,
        }
    }

    /// The soonest a view that could not show is tried again.
    #[must_use]
    pub fn retry_deadline(&self) -> Option<Instant> {
        self.views.retry_deadline()
    }

    /// Whether a running view is animating.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.views
            .running()
            .any(|(_, shown)| shown.host.needs_animation_frame())
    }

    /// Whether no view runs or waits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    /// Where the windows put their views while this frame drew them: a conversation that
    /// scrolled in this frame's draw has its views drawn where it now shows them, not a frame
    /// behind.
    fn refresh_places(&mut self) {
        for (index, asked) in self.asking.iter().enumerate() {
            if !self.places.iter().any(|place| place.asker == index) {
                continue;
            }
            let embeds = asked.window.embeds();
            for place in self.places.iter_mut().filter(|place| place.asker == index) {
                if let Some(embed) = embeds
                    .iter()
                    .find(|embed| embed.key == place.key && embed.view == place.view)
                {
                    let (rect, clip) = place_of(embed, asked.origin, asked.content);
                    place.rect = rect;
                    place.clip = clip;
                }
            }
        }
    }

    /// Draw each view over the window that asked for it, clipped to what of its place may show: a
    /// miniature laid out at a window's size and scaled into its place, a view worked in as it is
    /// laid out.
    pub fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.refresh_places();
        for place in &self.places {
            if place.clip.is_zero_area() || place.rect.is_zero_area() {
                continue;
            }
            let working = self
                .working
                .iter()
                .find(|working| working.view == place.view && working.node.is_some());
            let Some(shown) = self.views.get_mut(&place.view) else {
                continue;
            };
            let (at, viewport) = match working {
                Some(working) if place.working => (
                    transform.pre_translate(place.rect.origin() - working.laid),
                    shown.rect,
                ),
                // A view worked in elsewhere, shown here as well: its picture, scaled.
                Some(_) => (fit(transform, shown.rect, place.rect), shown.rect),
                None => {
                    if !lay_out(&mut shown.host, theme, text) {
                        continue;
                    }
                    (
                        view_transform(transform, place.rect),
                        Rect::new(0.0, 0.0, VIEW.0, VIEW.1),
                    )
                }
            };
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &place.clip,
            );
            let outer = env.push_viewport(viewport);
            if working.is_some() {
                shown.draw_child(scene, at, text, theme, &env);
            } else {
                shown.host.draw_child(scene, at, text, theme, &env);
            }
            env.pop_viewport(outer);
            scene.pop_layer();
        }
    }
}

/// Lay `host` out at [`VIEW`]'s size from the origin. Whether it could.
fn lay_out(host: &mut TileHost, theme: &Theme, text: &mut TextRenderer) -> bool {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut nodes: std::collections::HashMap<NodeId, *mut dyn Widget> =
        std::collections::HashMap::new();
    let child = host.build_layout(&mut taffy, theme, &mut nodes);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the view's logical size is far inside f32's range"
    )]
    let size = taffy::Size {
        width: taffy::Dimension::length(VIEW.0 as f32),
        height: taffy::Dimension::length(VIEW.1 as f32),
    };
    let Ok(root) = taffy.new_with_children(
        Style {
            size,
            ..Style::default()
        },
        &[child],
    ) else {
        return false;
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the view's logical size is far inside f32's range"
    )]
    let available = taffy::Size {
        width: taffy::AvailableSpace::Definite(VIEW.0 as f32),
        height: taffy::AvailableSpace::Definite(VIEW.1 as f32),
    };
    let mut bindings = crate::widget::bound_nodes::BoundNodeMap::new();
    // SAFETY: the arena and node map were just built from `host` alone, which stays in place and
    // unaliased for both calls.
    let solved = unsafe {
        bindings.rebuild(&*host, &taffy, child, &nodes);
        crate::widget::compute_widget_layout(&mut taffy, root, available, &nodes, &bindings, text)
    };
    if solved.is_err() {
        return false;
    }
    host.apply_layout(&taffy, child, Point::ORIGIN);
    true
}

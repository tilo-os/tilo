//! The desk's search palette over the windows.
//! Search belongs to the desk: it knows what the windows hold, what a person chose and where the
//! palette stands when it was opened from one window. But the desk is drawn under the windows, so a
//! palette painted there is covered by any window in front. This draws it from the chrome, which
//! stands over the windows: it takes the palette the desk keeps ([`DesktopHandle::
//! search_palette`]) and paints it over the windows, dimmed with the theme's modal scrim, routes
//! the person's input to it while it is up and closes it. The desk takes up what happened on its
//! next pass ([`DesktopHandle:: note_search_activity`]), and lays nothing out while the palette is
//! closed.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_surface::{DesktopHandle, PALETTE_EDGE, is_escape, is_search_chord};
use ui::platform::PlatformConventions;
use ui::presence::PresenceCtx;
use ui::search::Scope;
use ui::theme::Theme;
use ui::widget::DrawChild;
use ui::widget::{AppModel, ModelPath, TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

/// Search's palette as the chrome draws it over the windows.
pub struct SearchOverlay {
    /// The desk whose search this draws and answers for.
    handle: DesktopHandle,
    /// The keys the search chord is spelled with on this platform.
    platform: PlatformConventions,
    /// The whole area the chrome stands on, as last laid out.
    rect: Rect,
    /// The pointer, in logical pixels.
    cursor: Option<Point>,
    /// The desk's search revision this overlay last took up.
    seen: u64,
    /// What the layout in hand was built for: whether the palette was up, the area it stood over
    /// and what the desk had published then.
    built: (bool, Option<Rect>, u64),
    /// What the last drawn frame showed: whether the palette was up and what the desk had published
    /// then.
    shown: (bool, u64),
    /// The palette's own layout node, while it is built: the leaf the in-place resize gives its new
    /// size ([`Self::adjust_layout_style`]).
    palette_node: Option<NodeId>,
    /// The theme the card was last built and drawn with: what an in-place resize places the card
    /// by, as the solve reaches here without one.
    theme: Theme,
}

impl SearchOverlay {
    /// The palette over the windows, for the search `handle` keeps.
    #[must_use]
    pub fn new(handle: DesktopHandle) -> Self {
        let seen = handle.search_revision();
        Self {
            handle,
            platform: PlatformConventions::default(),
            rect: Rect::ZERO,
            cursor: None,
            seen,
            built: (false, None, seen),
            shown: (false, seen),
            palette_node: None,
            theme: Theme::default(),
        }
    }

    /// Whether the palette is up: it takes the keyboard, the pointer and Escape, and the stack says
    /// so.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.handle.search_open()
    }

    /// What the palette says of itself to assistive technology, while it is up: its field and what
    /// it lists.
    #[must_use]
    pub fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let search = self.handle.search_palette();
        let search = search.borrow();
        search.palette()?.accessibility()
    }

    /// Close the palette: the desk takes the search down on its next pass.
    pub fn close(&self) {
        self.handle.toggle_search();
        self.handle.note_search_activity();
    }

    /// The card's left edge, inside the desk's edges even where it is barely
    /// wider than the card.
    fn card_left(&self, x: f64, width: f64) -> f64 {
        x.clamp(
            self.rect.x0 + PALETTE_EDGE,
            (self.rect.x1 - width - PALETTE_EDGE).max(self.rect.x0 + PALETTE_EDGE),
        )
    }
}

/// A place on the desk, as layout measures it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a desk is a few thousand pixels"
)]
fn place(value: f64) -> taffy::LengthPercentageAuto {
    taffy::LengthPercentageAuto::length(value as f32)
}

impl Widget for SearchOverlay {
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let revision = self.handle.search_revision();
        let changed = revision != self.seen;
        self.seen = revision;
        // The desk's palette is the card's own tree: what it needs to do each frame, it does
        // through here.
        let search = self.handle.search_palette();
        let mut search = search.borrow_mut();
        let palette_changed = search
            .palette_mut()
            .is_some_and(|palette| palette.update(state, path));
        changed || palette_changed
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        let search = self.handle.search_palette();
        let mut search = search.borrow_mut();
        if let Some(palette) = search.palette_mut() {
            palette.settle(edits, path);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.take_structure_change().rebuilds()
    }

    /// The card is mounted when the palette opens and gone when it closes, and
    /// it stands where the search says: those are the composition's changes,
    /// and the tree is built again for them. The rows the card shows — a
    /// keystroke's results, an answer arriving — are not: the card is one leaf,
    /// the same node, and its rows only change its own size. Telling them apart
    /// matters because a rebuild discards the whole page's layout and re-encodes
    /// every window's scene for a change in the card alone, the cost growing
    /// with every window open. A row that changed re-solves the card's size in
    /// place instead ([`Widget::layout_needs_measure`], the palette's own
    /// [`Widget::adjust_layout_style`]), and the frame repaints only the card
    /// ([`Self::draw`]'s own damage part).
    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        // Open or closed, or standing over another area: the composition. The
        // revision alone (the rows, the query, the scope) is the card's paint
        // and its size, never the tree's shape. A card that is up with no node
        // yet — its code arrived after it was opened, and the palette had
        // nothing to build then — is built now, once.
        let missing = self.is_open() && self.palette_node.is_none();
        let structural =
            (self.is_open(), self.handle.search_area()) != (self.built.0, self.built.1) || missing;
        ui::widget::StructureChange::of(structural)
    }

    /// The card resizes in place when its rows or the room it stands in changed,
    /// without rebuilding the tree ([`Self::take_structure_change`]).
    fn layout_needs_measure(&self) -> bool {
        self.built != self.pace()
    }

    /// The card's own leaf is the overlay's child: what it shows changed, so
    /// it is given its new size here and the tree solves around it, without a
    /// rebuild. The overlay node itself keeps the whole chrome, absolute, and
    /// its own style never changes.
    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _node: NodeId,
        _text: &mut ui::TextRenderer,
    ) -> bool {
        let Some(palette) = self.palette_node else {
            return false;
        };
        if !self.is_open() || self.rect.width() <= 0.0 {
            return false;
        }
        let area = self.handle.search_area().unwrap_or(self.rect);
        let theme = self.theme.clone();
        let (origin, width) = ui::action_palette::placement(area, &theme);
        let search = self.handle.search_palette();
        let mut search = search.borrow_mut();
        search.set_room(width, ui::action_palette::list_cap(area, &theme));
        let size = search.desired_size();
        let Ok(current) = taffy.style(palette) else {
            return false;
        };
        let mut style = current.clone();
        style.size = taffy::Size::from_lengths(size.0, size.1);
        let x0 = self.card_left(origin.x, f64::from(size.0));
        style.inset.left = place(x0 - self.rect.x0);
        style.inset.top = place(origin.y - self.rect.y0);
        if style == *current {
            // Nothing moved: still this frame laid the card out for this pace.
            self.built = self.pace();
            return false;
        }
        let changed = taffy.set_style(palette, style).is_ok();
        self.built = self.pace();
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut kids = Vec::new();
        if self.is_open() && self.rect.width() > 0.0 {
            // What the search covers is what it stands over: the window it was opened from, or the
            // whole desk when it reaches past it.
            let area = self.handle.search_area().unwrap_or(self.rect);
            let search = self.handle.search_palette();
            let mut search = search.borrow_mut();
            // The card stands where [`ui::action_palette::placement`] puts it,
            // three fifths of what it stands over and never wider than it.
            let (origin, width) = ui::action_palette::placement(area, theme);
            search.set_room(width, ui::action_palette::list_cap(area, theme));
            let node = search
                .palette_mut()
                .map(|palette| palette.build_layout(taffy, theme, map));
            if let Some(node) = node {
                let width = f64::from(search.desired_size().0);
                let mut style = taffy.style(node).cloned().unwrap_or_default();
                style.position = taffy::Position::Absolute;
                let x0 = self.card_left(origin.x, width);
                style.inset = taffy::Rect {
                    left: place(x0 - self.rect.x0),
                    right: taffy::LengthPercentageAuto::auto(),
                    top: place(origin.y - self.rect.y0),
                    bottom: taffy::LengthPercentageAuto::auto(),
                };
                let _ = taffy.set_style(node, style);
                kids.push(node);
                self.palette_node = Some(node);
            } else {
                self.palette_node = None;
            }
        } else {
            self.palette_node = None;
        }
        self.theme = theme.clone();
        // Over the whole chrome, out of its flow: the card sits where it was placed and dims what
        // stands under it.
        let style = Style {
            position: taffy::Position::Absolute,
            inset: taffy::Rect {
                left: taffy::LengthPercentageAuto::length(0.0),
                right: taffy::LengthPercentageAuto::length(0.0),
                top: taffy::LengthPercentageAuto::length(0.0),
                bottom: taffy::LengthPercentageAuto::length(0.0),
            },
            ..Style::default()
        };
        // What the node just built stands for: the frame built it, so it has nothing more to report
        // until one of these moves.
        self.built = self.pace();
        taffy.widget_node(style, &kids, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [palette] = taffy.children(node).unwrap_or_default()[..] {
            let search = self.handle.search_palette();
            let mut search = search.borrow_mut();
            if let Some(palette_widget) = search.palette_mut() {
                palette_widget.apply_layout(taffy, palette, inner);
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.is_open() {
            self.shown = (false, self.handle.search_revision());
            return;
        }
        // The desk's own dim, never a literal fill: the same scrim the shell lays under its
        // palette, over everything the card stands above, the window in front included. Then the
        // card, where it was laid out.
        ui::overlay::scrim::modal(scene, transform, self.rect, 1.0, theme);
        // The card is one part of the page's scene, bounded by what it paints: a person typing into
        // it (every key, its caret) or a result arriving repaints the card, not the scrimmed
        // display under it. Without the part the card's commands sit outside every part, which the
        // retained-damage producer cannot prove sparse, so every frame of a search repaints the
        // whole display — seconds a frame under a software rasteriser. The scrim stays outside the
        // part on purpose: it paints the whole area, and a part's bounds must cover everything it
        // paints; it changes only as the search opens or the theme does.
        let card = {
            let search = self.handle.search_palette();
            let search = search.borrow();
            search
                .palette()
                .map(ui::widget::Widget::get_rect)
                .filter(|card| !card.is_zero_area())
        };
        let Some(card) = card else {
            // No card laid out yet: nothing of the palette draws this frame.
            self.shown = (true, self.handle.search_revision());
            return;
        };
        let key = ui::scene_parts::PartKey::new("desktop.search-overlay", ());
        ui::scene_parts::part_drawn(scene, key, |scene| {
            let search = self.handle.search_palette();
            let mut search = search.borrow_mut();
            if let Some(palette) = search.palette_mut() {
                palette.draw_child(scene, transform, text, theme, presence);
            }
            vec![
                transform
                    .transform_rect_bbox(ui::widget::floating_surface::painted_bounds(card, theme)),
            ]
        });
        self.shown = (true, self.handle.search_revision());
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(ctx.logical(position));
        } else if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.cursor = None;
        }
        if !self.is_open() {
            // Search's chord opens it, as it does from the desk: the desk opens the palette on its
            // next pass. Opening is what the chord does while the palette is up, too, where the
            // desk's own key already closed it.
            if is_search_chord(event, *ctx.modifiers, self.platform) {
                self.handle.open_search(Scope::Workspace, None);
                self.handle.note_search_activity();
                *ctx.handled = true;
            }
            return;
        }
        // The card's node, or the overlay's own before the first layout of it: the palette answers
        // the event either way, as it did standing in the desk.
        let palette = ctx
            .taffy
            .children(ctx.node_id)
            .unwrap_or_default()
            .first()
            .copied()
            .unwrap_or(ctx.node_id);
        // The pointer is where it last moved, read as the palette answers events: a press on the
        // card is the palette's, and one off it is the person leaving search.
        let pressed = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        let on_card = self.cursor.is_some_and(|cursor| {
            let search = self.handle.search_palette();
            let search = search.borrow();
            search
                .palette()
                .is_some_and(|palette| palette.get_rect().contains(cursor))
        });
        if (pressed && !on_card) || is_search_chord(event, *ctx.modifiers, self.platform) {
            // A press off the card, or the chord again, leaves search: the desk closes it on its
            // next pass.
            self.close();
        } else if is_escape(event) {
            // Clears the query, or closes an empty search: see
            // [`tilo_desktop_search::DesktopSearch::escape`].
            let escaped = {
                let search = self.handle.search_palette();
                search.borrow_mut().escape()
            };
            if !escaped {
                self.forward(event, ctx, palette);
            }
        } else {
            self.forward(event, ctx, palette);
        }
        // Whatever the palette was asked, the desk takes it up on its next pass, and the card it
        // publishes is drawn from then on.
        self.handle.note_search_activity();
        *ctx.handled = true;
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if !self.is_open() {
            return false;
        }
        let answered = {
            let search = self.handle.search_palette();
            let mut search = search.borrow_mut();
            search
                .palette_mut()
                .is_some_and(|palette| palette.perform_semantic_action(target, action, state, path))
        };
        if answered {
            // The desk takes up what the card was asked on its next pass, as it does what the
            // person typed or clicked there.
            self.handle.note_search_activity();
        }
        answered
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        // The card is the desk's palette: the node this overlay lays out is the palette's own,
        // while the palette itself is not routed here (the overlay routes the person's input to
        // it). Nothing of it is in the layout while it is closed.
        if !self.is_open() {
            return true;
        }
        let search = self.handle.search_palette();
        let search = search.borrow();
        if let Some(palette) = search.palette() {
            visitor(palette);
        }
        true
    }

    fn children(&self) -> Vec<&dyn Widget> {
        // The card is the desk's own palette, laid out and drawn through it;
        // nothing is retained here.
        Vec::new()
    }

    fn for_each_hosted_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        // The card is in the tree as what stands over the windows, for inspection, named controls
        // and assistive technology: the overlay routes the person's input to it itself, so it is no
        // child of [`Self::children`].
        if !self.is_open() {
            return;
        }
        let search = self.handle.search_palette();
        let search = search.borrow();
        if let Some(palette) = search.palette() {
            visitor(palette);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        Vec::new()
    }

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        let search = self.handle.search_palette();
        search.borrow_mut().set_platform_conventions(conventions);
    }

    fn traps_focus(&self) -> bool {
        self.is_open()
    }

    fn needs_redraw(&self) -> bool {
        // Search opened, closed, moved or found other rows since the last frame, or the card is
        // still arriving: what stands on the screen no longer describes the desk, or is still
        // moving.
        self.shown != (self.is_open(), self.handle.search_revision())
            || self.needs_animation_frame()
    }

    fn is_layout_dirty(&self) -> bool {
        // Search opened, closed, moved or found other rows: the card it stands in is laid out
        // again. While nothing of that moves, the layout in hand is the one drawn.
        self.built != self.pace()
    }

    fn needs_animation_frame(&self) -> bool {
        let search = self.handle.search_palette();
        let search = search.borrow();
        search
            .palette()
            .is_some_and(ui::Widget::needs_animation_frame)
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let search = self.handle.search_palette();
        let search = search.borrow();
        // A search still filling in is looked for again shortly: its rows arrive off the frame (a
        // worker's answer wakes the desk; an application's answer comes on the host's clock), and
        // a desk with nothing else to draw presents none. The desk's own schedule is not the
        // chrome's — the palette is drawn from here — so the deadline goes with the card's. A
        // search that never asked for it shows the rows it had when the person stopped typing,
        // and Enter on an app's row comes long after the person pressed it.
        // Rows are still expected to join what the search shows (a window's
        // content index being built, an app's server still looking): look
        // again shortly, so a late row is drawn when it arrives. This is not
        // "still searching": the rows already shown are the search's answer,
        // and a fresh window's content must not keep them drawn as if it
        // were.
        let expecting = (self.handle.search_open() && self.handle.search_expecting())
            .then(|| ui::time::now().checked_add(tilo_desktop_search::ANSWER_POLL))
            .flatten();
        search
            .palette()
            .and_then(ui::Widget::next_wake_deadline)
            .into_iter()
            .chain(expecting)
            .min()
    }

    fn name(&self) -> &'static str {
        "SearchOverlay"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl SearchOverlay {
    /// What this overlay stands for now: whether the palette is up, the area it stands over and
    /// what the desk last published. Two equal paces mean the layout in hand still describes it.
    fn pace(&self) -> (bool, Option<Rect>, u64) {
        (
            self.is_open(),
            self.handle.search_area(),
            self.handle.search_revision(),
        )
    }

    /// Hand `event` to the palette as the person typed or pressed it there.
    fn forward(&self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>, node: NodeId) {
        let path = ctx.path.clone();
        let search = self.handle.search_palette();
        let mut search = search.borrow_mut();
        if let Some(palette) = search.palette_mut() {
            palette.handle_event(event, &mut ctx.reborrow(&path, node));
        }
    }
}

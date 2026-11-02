//! The desk's search palette over the windows.
//!
//! Search belongs to the desk: it knows what the windows hold, what a person
//! chose and where the palette stands when it was opened from one window. But
//! the desk is drawn under the windows, so a palette painted there is covered
//! by any window in front. This draws it from the chrome, which stands over
//! the windows: it takes the palette the desk keeps ([`DesktopHandle::
//! search_palette`]) and paints it over the windows, dimmed with the theme's
//! modal scrim, routes the person's input to it while it is up and closes it.
//! The desk takes up what happened on its next pass ([`DesktopHandle::
//! note_search_activity`]), and lays nothing out while the palette is closed.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_surface::{DesktopHandle, PALETTE_EDGE, is_escape, is_search_chord};
use ui::platform::PlatformConventions;
use ui::presence::PresenceCtx;
use ui::search::Scope;
use ui::theme::Theme;
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
    /// Whether the palette was up when this overlay last looked.
    was_open: bool,
    /// What the layout in hand was built for: whether the palette was up, the
    /// area it stood over and what the desk had published then.
    built: (bool, Option<Rect>, u64),
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
            was_open: false,
            built: (false, None, seen),
        }
    }

    /// Whether the palette is up: it takes the keyboard, the pointer and
    /// Escape, and the stack says so.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.handle.search_open()
    }

    /// What the palette says of itself to assistive technology, while it is
    /// up: its field and what it lists.
    #[must_use]
    pub fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let search = self.handle.search_palette();
        let search = search.borrow();
        search.palette()?.accessibility()
    }

    /// Close the palette: the desk takes the search down on its next pass.
    fn close(&self) {
        self.handle.toggle_search();
        self.handle.note_search_activity();
    }
}

impl Widget for SearchOverlay {
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let revision = self.handle.search_revision();
        let changed = revision != self.seen || self.was_open != self.is_open();
        self.seen = revision;
        self.was_open = self.is_open();
        // The desk's palette is the card's own tree: what it needs to do each
        // frame, it does through here.
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
        // The card is mounted when the palette opens and gone when it closes,
        // and it grows and shrinks with the rows the desk published and the
        // pace it stands on: the composition is built again for any of them.
        self.built != self.pace()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Search's code is asked for as the desktop first lays out, so it is
        // in by the time someone searches; search asked for before then opens
        // now, and the desk publishes it on its next pass.
        let adopted = self.handle.search_palette().borrow_mut().adopt();
        if adopted {
            self.handle.note_search_activity();
        }
        let mut kids = Vec::new();
        if self.is_open() && self.rect.width() > 0.0 {
            // What the search covers is what it stands over: the window it was
            // opened from, or the whole desk when it reaches past it.
            let area = self.handle.search_area().unwrap_or(self.rect);
            let search = self.handle.search_palette();
            let mut search = search.borrow_mut();
            // The palette dims what it stands over as it arrives, and the card
            // stands where [`ui::action_palette::placement`] puts it.
            let placed = search
                .palette_mut()
                .map(|palette| palette.place_in(area, theme));
            if let Some((origin, width)) = placed {
                search.set_room(width, ui::action_palette::list_cap(area, theme));
                let node = search
                    .palette_mut()
                    .map(|palette| palette.build_layout(taffy, theme, map));
                if let Some(node) = node {
                    let (width, _) = search.desired_size();
                    let mut style = taffy.style(node).cloned().unwrap_or_default();
                    style.position = taffy::Position::Absolute;
                    // Inside the desk's edges, even where it is barely wider
                    // than the card.
                    let x0 = origin.x.clamp(
                        self.rect.x0 + PALETTE_EDGE,
                        (self.rect.x1 - width - PALETTE_EDGE).max(self.rect.x0 + PALETTE_EDGE),
                    );
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "a place on the desk is a few thousand logical pixels"
                    )]
                    let at = |value: f64| taffy::LengthPercentageAuto::length(value as f32);
                    style.inset = taffy::Rect {
                        left: at(x0 - self.rect.x0),
                        right: taffy::LengthPercentageAuto::auto(),
                        top: at(origin.y - self.rect.y0),
                        bottom: taffy::LengthPercentageAuto::auto(),
                    };
                    let _ = taffy.set_style(node, style);
                    kids.push(node);
                }
            }
        }
        // Over the whole chrome, out of its flow: the card sits where it was
        // placed and dims what its own scrim covers.
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
        // What the node just built stands for: the frame built it, so it has
        // nothing more to report until one of these moves.
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
            return;
        }
        // The palette dims what it stands over as it arrives and draws the
        // card over it: the same card, in the same place, as it stood in the
        // desk.
        let search = self.handle.search_palette();
        let mut search = search.borrow_mut();
        if let Some(palette) = search.palette_mut() {
            palette.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(ctx.logical(position));
        } else if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.cursor = None;
        }
        if !self.is_open() {
            // Search's chord opens it, as it does from the desk: the desk
            // opens the palette on its next pass.
            if is_search_chord(event, *ctx.modifiers, self.platform) {
                self.handle.open_search(Scope::Workspace, None);
                self.handle.note_search_activity();
                *ctx.handled = true;
            }
            return;
        }
        let nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let Some(&palette) = nodes.first() else {
            // The card is not laid out yet: the palette is the desk's, and
            // nothing else may see this.
            *ctx.handled = true;
            self.handle.note_search_activity();
            return;
        };
        // The pointer is where it last moved, read as the palette answers
        // events: a press on the card is the palette's, and one off it is the
        // person leaving search.
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
        // Whatever the palette was asked, the desk takes it up on its next
        // pass, and the card it publishes is drawn from then on.
        self.handle.note_search_activity();
        *ctx.handled = true;
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        // The card is the desk's palette: the node this overlay lays out is
        // the palette's own, while the palette itself is not routed here (the
        // overlay routes the person's input to it).
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
        self.was_open != self.is_open() || self.handle.search_revision() != self.seen
    }

    fn is_layout_dirty(&self) -> bool {
        // Search opened, closed, moved or found other rows: the card it
        // stands in is laid out again. While nothing of that moves, the
        // layout in hand is the one drawn.
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
        search.palette().and_then(ui::Widget::next_wake_deadline)
    }

    fn name(&self) -> &'static str {
        "SearchOverlay"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl SearchOverlay {
    /// What this overlay stands for now: whether the palette is up, the area
    /// it stands over and what the desk last published. Two equal paces mean
    /// the layout in hand still describes it.
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

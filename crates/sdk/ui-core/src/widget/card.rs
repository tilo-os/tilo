//! `Card` and `CardGrid`: objects in content.
//!
//! A card exists only for an *object*: something the person can open, move,
//! share or drag (an app in a store, a document, a review comment, a saved
//! connection). Settings groups, form sections and statistics are not cards;
//! they sit on the content ground separated by space ([`Section`],
//! [`FormSection`], [`StatTile`]).
//!
//! A card has a header (its name in `Subheading`, with one optional trailing
//! action), a body, and an optional footer of [`Actions`]. Its height is its
//! content's; in a [`CardGrid`] a row is as tall as its tallest card and the
//! footers line up. A card that opens something is itself pressable
//! ([`Card::on_activate`]) and lifts under the pointer. A leading stripe in a
//! [`Tone`] marks whose or what kind of object it is (a reviewer's comment, a
//! failed job).
//!
//! [`CardFace`] paints the same surface, stripe, title and focus ring for a
//! caller that lays out and paints its own body.
//!
//! [`Section`]: crate::widget::Section
//! [`FormSection`]: crate::widget::FormSection
//! [`StatTile`]: crate::widget::StatTile

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{auto, fr, length, min_content, minmax, percent, repeat};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use super::composition_parts::{Parts, retained_parts};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::activation::{Activation, Interaction, activation_controls};
use crate::widget::{Button, Element, TaffyTreeExt, Text, Tone, Widget};

/// Lossless `usize -> f64` widening for grid geometry.
fn usize_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

// ---------------------------------------------------------------------------
// Pure reflow geometry, shared by CardGrid, MediaGrid and immediate callers.
// ---------------------------------------------------------------------------

/// How many columns of at least `min_card_w` fit in `avail_w` with `gap`
/// between them. Always at least 1.
pub fn card_columns(avail_w: f64, min_card_w: f64, gap: f64) -> usize {
    let stride = min_card_w + gap;
    if stride <= 0.0 || avail_w <= 0.0 {
        return 1;
    }
    let n = ((avail_w + gap) / stride).floor();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "n is clamped into `[1.0, u32::MAX]` immediately before the cast, so the \
                  conversion is provably exact"
    )]
    let cols = n.clamp(1.0, f64::from(u32::MAX)) as usize;
    cols
}

/// The width of each of `cols` columns that, with `gap` between them, fill
/// `avail_w` exactly. Never negative.
pub fn card_column_width(avail_w: f64, cols: usize, gap: f64) -> f64 {
    let cols = cols.max(1);
    let total_gap = usize_to_f64(cols.saturating_sub(1)) * gap;
    ((avail_w - total_gap) / usize_to_f64(cols)).max(0.0)
}

// ---------------------------------------------------------------------------
// Shared card geometry.
// ---------------------------------------------------------------------------

/// The resolved chrome of a card in `theme`.
#[derive(Clone, Copy, Debug)]
struct CardMetrics {
    padding: f64,
    gap: f64,
    radius: f64,
    stripe: f64,
}

impl CardMetrics {
    fn new(theme: &Theme) -> Self {
        let rhythm = &theme.tokens.spacing.rhythm;
        Self {
            padding: f64::from(rhythm.section),
            gap: f64::from(rhythm.sibling),
            radius: f64::from(theme.tokens.radii.tile),
            stripe: f64::from(theme.tokens.metrics.chrome_rule_px) * 3.0,
        }
    }
}

/// Paint a card's surface (and stripe, hover lift and focus ring) over
/// `rect`.
fn paint_surface(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    stripe: Option<Tone>,
    interaction: Interaction,
    pressable: bool,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let theme = ctx.theme;
    let m = CardMetrics::new(theme);
    let raised = theme.tokens.colors.surfaces.raised;
    let fill = if pressable && !interaction.disabled && interaction.pressed {
        theme.tokens.colors.lift_on(raised, 1.0)
    } else if pressable && !interaction.disabled && interaction.hovered {
        theme.tokens.colors.lift_on(raised, 0.5)
    } else {
        raised
    };
    crate::surface::card(ctx.scene, ctx.transform, rect, m.radius, fill);
    if let Some(tone) = stripe {
        let shape = RoundedRect::from_rect(rect, m.radius);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &shape,
        );
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            tone.colour(theme),
            None,
            &Rect::new(rect.x0, rect.y0, rect.x0 + m.stripe, rect.y1),
        );
        ctx.scene.pop_layer();
    }
    if interaction.focused && !interaction.disabled {
        crate::widget::paint_focus_ring(ctx.scene, ctx.transform, rect, m.radius, theme);
    }
}

// ---------------------------------------------------------------------------
// CardFace — immediate-mode card chrome.
// ---------------------------------------------------------------------------

/// Immediate-mode card: the surface, leading stripe, title and focus ring
/// of a [`Card`], for a caller that paints its own body into
/// [`CardFace::body_rect`].
#[derive(Clone, Copy, Debug, Default)]
pub struct CardFace<'a> {
    title: Option<&'a str>,
    stripe: Option<Tone>,
    pressable: bool,
    interaction: Interaction,
}

impl<'a> CardFace<'a> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The object's name, in `Subheading`.
    #[must_use]
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    /// See [`Card::leading_stripe`].
    #[must_use]
    pub fn leading_stripe(mut self, tone: Tone) -> Self {
        self.stripe = Some(tone);
        self
    }

    /// The card opens something: it lifts under the pointer.
    #[must_use]
    pub fn pressable(mut self) -> Self {
        self.pressable = true;
        self
    }

    #[must_use]
    pub fn interaction(mut self, interaction: Interaction) -> Self {
        self.interaction = interaction;
        self
    }

    /// The height of the card's chrome around a body of `body_height`:
    /// what a caller adds to its body to size the card.
    pub fn height_for_body(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        body_height: f64,
    ) -> f64 {
        let m = CardMetrics::new(theme);
        let title = self.title_height(text, theme, width);
        let header = if title > 0.0 { title + m.gap } else { 0.0 };
        (2.0 * m.padding + header + body_height).ceil()
    }

    fn title_height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let Some(title) = self.title else {
            return 0.0;
        };
        let m = CardMetrics::new(theme);
        let wrap = crate::num::f32_of_f64((width - 2.0 * m.padding).max(1.0));
        f64::from(
            text.measure_style(title, Some(wrap), &theme.text(TextRole::Subheading))
                .1,
        )
    }

    /// Where the caller paints its body inside a card at `rect`.
    pub fn body_rect(&self, text: &mut TextRenderer, theme: &Theme, rect: Rect) -> Rect {
        let m = CardMetrics::new(theme);
        let title = self.title_height(text, theme, rect.width());
        let header = if title > 0.0 { title + m.gap } else { 0.0 };
        Rect::new(
            rect.x0 + m.padding,
            rect.y0 + m.padding + header,
            rect.x1 - m.padding,
            rect.y1 - m.padding,
        )
    }

    /// Paint the card's chrome and title into `rect`.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        paint_surface(ctx, rect, self.stripe, self.interaction, self.pressable);
        if let Some(title) = self.title {
            let m = CardMetrics::new(ctx.theme);
            let style = ctx.theme.text(TextRole::Subheading);
            let wrap = crate::num::f32_of_f64((rect.width() - 2.0 * m.padding).max(1.0));
            let ink = ctx.theme.text_primary;
            ctx.text(
                title,
                &style,
                ink,
                Point::new(rect.x0 + m.padding, rect.y0 + m.padding),
                Some(wrap),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Card — retained.
// ---------------------------------------------------------------------------

/// The header row: a title and one optional trailing action.
struct CardHeader {
    parts: Parts,
}

impl Widget for CardHeader {
    retained_parts!("CardHeader");

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.parts.build(
            Style {
                display: taffy::Display::Grid,
                grid_template_columns: vec![minmax(length(0.0_f32), fr(1.0_f32)), auto()],
                align_items: Some(taffy::AlignItems::Center),
                gap: Size {
                    width: length(theme.tokens.spacing.rhythm.related),
                    height: length(0.0_f32),
                },
                size: Size {
                    width: percent(1.0_f32),
                    height: auto(),
                },
                ..Default::default()
            },
            taffy,
            theme,
            map,
        );
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}

/// A retained card. See the module docs.
pub struct Card {
    parts: Parts,
    has_header: bool,
    has_footer: bool,
    stripe: Option<Tone>,
    activation: Activation,
    dirty: bool,
}

impl Card {
    /// A card around `body`.
    pub fn new(body: Element) -> Self {
        Self {
            parts: Parts::new(vec![body]),
            has_header: false,
            has_footer: false,
            stripe: None,
            activation: Activation::new("card"),
            dirty: true,
        }
    }

    /// The object's name, in the header.
    #[must_use]
    pub fn with_title(self, title: impl Into<String>) -> Self {
        let title = title.into();
        self.with_header_parts(
            Box::new(
                Text::new(&title)
                    .with_role(TextRole::Subheading)
                    .with_heading_level(3),
            ),
            None,
        )
    }

    /// The object's name and one trailing action (a "More" menu, an
    /// "Install" button, a count).
    #[must_use]
    pub fn with_title_action(self, title: impl Into<String>, action: Element) -> Self {
        let title = title.into();
        self.with_header_parts(
            Box::new(
                Text::new(&title)
                    .with_role(TextRole::Subheading)
                    .with_heading_level(3),
            ),
            Some(action),
        )
    }

    fn with_header_parts(mut self, title: Element, action: Option<Element>) -> Self {
        let mut children = vec![title];
        if let Some(action) = action {
            children.push(action);
        }
        let header: Element = Box::new(CardHeader {
            parts: Parts::new(children),
        });
        if self.has_header
            && let Some(slot) = self.parts.children.first_mut()
        {
            *slot = header;
        } else {
            self.parts.children.insert(0, header);
            self.has_header = true;
        }
        self
    }

    /// The card's actions, at its foot, trailing.
    #[must_use]
    pub fn with_footer(mut self, actions: Actions) -> Self {
        if self.has_footer {
            self.parts.children.pop();
        }
        self.parts.children.push(Box::new(actions));
        self.has_footer = true;
        self
    }

    /// A stripe down the leading edge in `tone`: whose object this is (a
    /// reviewer's comment in their presence colour) or its status.
    #[must_use]
    pub fn leading_stripe(mut self, tone: Tone) -> Self {
        self.stripe = Some(tone);
        self
    }

    /// Pressing the card (not one of its own actions) opens what it stands
    /// for. `id` names the card for keyboard, tests and assistive technology.
    #[must_use]
    pub fn on_activate(mut self, id: impl Into<String>, f: impl FnMut() + 'static) -> Self {
        self.activation = Activation::new(id);
        self.activation.set_callback(Box::new(f));
        self
    }

    /// [`Self::on_activate`] through an exported resource action.
    #[must_use]
    pub fn on_action(
        mut self,
        id: impl Into<String>,
        binding: tilo_actions::ActionBinding<()>,
    ) -> Self {
        self.activation = Activation::new(id);
        self.activation.set_action(binding);
        self
    }
}

impl Widget for Card {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::undo::UndoPoints
    );

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let before = self.parts.rect;
        self.parts.apply_layout(taffy, node, origin);
        if before != self.parts.rect {
            self.dirty = true;
        }
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        crate::widget::start_in_children(self, op, doc, path)
    }

    fn update(
        &mut self,
        state: &mut crate::widget::AppModel,
        path: &crate::widget::ModelPath,
    ) -> bool {
        let mut changed = false;
        for child in &mut self.parts.children {
            changed |= child.update(state, path);
        }
        changed
    }

    fn sync(&mut self, state: &mut crate::widget::AppModel, path: &crate::widget::ModelPath) {
        for child in &mut self.parts.children {
            child.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let m = CardMetrics::new(theme);
        let mut tracks = Vec::new();
        if self.has_header {
            tracks.push(min_content());
        }
        tracks.push(minmax(min_content(), fr(1.0_f32)));
        if self.has_footer {
            tracks.push(min_content());
        }
        let pad = crate::num::f32_of_f64(m.padding);
        let stripe_pad = if self.stripe.is_some() {
            crate::num::f32_of_f64(m.stripe)
        } else {
            0.0
        };
        let node = self.parts.build(
            Style {
                display: taffy::Display::Grid,
                grid_template_rows: tracks,
                grid_template_columns: vec![minmax(length(0.0_f32), fr(1.0_f32))],
                size: Size {
                    width: percent(1.0_f32),
                    height: auto(),
                },
                min_size: Size {
                    width: length(0.0_f32),
                    // Intrinsic Grid rows need this item's automatic block minimum;
                    // `flex_shrink` does not protect an item in Grid sizing.
                    height: auto(),
                },
                flex_shrink: 0.0,
                padding: taffy::Rect {
                    left: length(pad + stripe_pad),
                    right: length(pad),
                    top: length(pad),
                    bottom: length(pad),
                },
                gap: Size {
                    width: length(crate::num::f32_of_f64(m.gap)),
                    height: length(crate::num::f32_of_f64(m.gap)),
                },
                ..Default::default()
            },
            taffy,
            theme,
            map,
        );
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if presence.should_cull(self.parts.rect) {
            return;
        }
        paint_surface(
            &mut PaintCtx::new(scene, transform, text, theme),
            self.parts.rect,
            self.stripe,
            self.activation.state,
            self.activation.is_bound(),
        );
        self.parts.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        for (child, &node) in self.parts.children.iter_mut().zip(&self.parts.nodes) {
            let path = ctx.path;
            child.handle_event(event, &mut ctx.reborrow(path, node));
            if *ctx.handled {
                // A child's own control took the press; the card stays put.
                if self.activation.state.pressed {
                    self.activation.state.pressed = false;
                    self.dirty = true;
                }
                return;
            }
        }
        if self.activation.is_bound()
            && self
                .activation
                .handle_pointer(event, ctx, self.parts.rect, &[])
        {
            self.dirty = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.parts.children.iter().map(|c| c.as_ref()).collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in &self.parts.children {
            visitor(child.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.parts
            .children
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        for child in &mut self.parts.children {
            child.mark_drawn();
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.parts.children.iter().any(|c| c.needs_redraw())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for child in &mut self.parts.children {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn wants_pointer_capture(&self) -> bool {
        self.parts
            .children
            .iter()
            .any(|c| c.wants_pointer_capture())
    }

    fn get_rect(&self) -> Rect {
        self.parts.rect
    }

    fn name(&self) -> &'static str {
        "Card"
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.activation.is_bound() && self.parts.rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        self.activation.is_bound()
    }

    fn set_focus(&mut self, focused: bool) {
        if self.activation.state.focused != focused {
            self.activation.state.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.activation.state.focused
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::AppModel,
        _root: &crate::widget::ModelPath,
    ) -> bool {
        matches!(action, crate::a11y::Action::Activate) && self.activation.activate()
    }

    fn test_id(&self) -> Option<&str> {
        self.activation.is_bound().then(|| self.activation.id())
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if !self.activation.is_bound() {
            return None;
        }
        Some(
            AccessNode::new(Role::Button)
                .with_semantic_key(self.activation.id())
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.activation.state.focused),
                ),
        )
    }
}

activation_controls!(Card, activation);

impl crate::undo::UndoPoints for Card {
    fn needs_commit(&self) -> bool {
        self.parts
            .children
            .iter()
            .any(|c| crate::undo::needs_commit(c.as_ref()))
    }
    fn on_commit(&mut self) {
        for child in &mut self.parts.children {
            crate::undo::on_commit(child.as_mut());
        }
    }
}

// ---------------------------------------------------------------------------
// Actions — a card's or a form's small action group.
// ---------------------------------------------------------------------------

/// Standard actions in declaration/focus order: secondaries, then primary.
/// Unspecified buttons use measured content sizing; explicit sizes remain manual.
/// Each wrapped line aligns to the trailing edge.
pub struct Actions {
    parts: Parts,
}

impl Actions {
    pub fn new(primary: Button) -> Self {
        Self {
            parts: Parts::new(vec![Box::new(primary)]),
        }
    }

    #[must_use]
    pub fn secondary(mut self, button: Button) -> Self {
        let index = self.parts.children.len().saturating_sub(1);
        self.parts.children.insert(index, Box::new(button));
        self
    }
}

impl Widget for Actions {
    retained_parts!("Actions");

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let gap = theme.tokens.spacing.rhythm.related;
        let node = self.parts.build(
            Style {
                display: taffy::Display::Flex,
                flex_wrap: taffy::FlexWrap::Wrap,
                justify_content: Some(taffy::JustifyContent::End),
                align_items: Some(taffy::AlignItems::End),
                align_content: Some(taffy::AlignContent::Start),
                size: Size {
                    width: auto(),
                    height: auto(),
                },
                min_size: Size {
                    width: length(0.0_f32),
                    height: length(0.0_f32),
                },
                flex_shrink: 0.0,
                gap: Size {
                    width: length(gap),
                    height: length(gap),
                },
                ..Default::default()
            },
            taffy,
            theme,
            map,
        );
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}

// ---------------------------------------------------------------------------
// CardGrid — a responsive grid of equal, aligned cards.
// ---------------------------------------------------------------------------

/// How wide a grid's cards are at least, before the grid drops a column.
/// Measured in lines of body text, so cards widen with the text scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardWidth {
    /// Small objects: a quick start, a device, a saved search.
    Compact,
    /// Most objects: an app, a document, a connection.
    #[default]
    Regular,
    /// Objects with a paragraph of content: a review, an article.
    Wide,
}

impl CardWidth {
    /// The minimum column width under `theme`.
    #[must_use]
    pub fn min_width(self, theme: &Theme) -> f64 {
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        let lines = match self {
            Self::Compact => 10.0,
            Self::Regular => 13.0,
            Self::Wide => 18.0,
        };
        (line * lines).round()
    }
}

/// Reflows its cards into equal, aligned columns by width. Each row is as
/// tall as its tallest card; cards share the row's height so their footers
/// line up, and a row of short cards stays short.
pub struct CardGrid {
    cards: Vec<Element>,
    width: CardWidth,
    min_card_w: f64,
    gap: f64,
    rect: Rect,
    child_nodes: Vec<NodeId>,
}

impl CardGrid {
    pub fn new(cards: Vec<Element>) -> Self {
        Self {
            cards,
            width: CardWidth::Regular,
            min_card_w: 0.0,
            gap: 0.0,
            rect: Rect::ZERO,
            child_nodes: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_card_width(mut self, width: CardWidth) -> Self {
        self.width = width;
        self
    }

    pub fn columns_for_width(&self, width: f64) -> usize {
        card_columns(width, self.min_card_w, self.gap).min(self.cards.len().max(1))
    }

    fn grid_style(&self) -> Style {
        let gap = crate::num::f32_of_f64(self.gap);
        Style {
            display: taffy::Display::Grid,
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                // Intrinsic Grid rows need this item's automatic block minimum;
                // `flex_shrink` does not protect an item in Grid sizing.
                height: auto(),
            },
            flex_shrink: 0.0,
            align_self: Some(taffy::AlignSelf::Start),
            align_content: Some(taffy::AlignContent::Start),
            grid_template_columns: vec![repeat(
                "auto-fit",
                vec![minmax(
                    length(crate::num::f32_of_f64(self.min_card_w)),
                    fr(1.0_f32),
                )],
            )],
            grid_auto_rows: vec![min_content()],
            gap: Size {
                width: length(gap),
                height: length(gap),
            },
            ..Default::default()
        }
    }
}

impl Widget for CardGrid {
    crate::provides!(dyn crate::undo::UndoPoints);

    fn update(
        &mut self,
        state: &mut crate::widget::AppModel,
        path: &crate::widget::ModelPath,
    ) -> bool {
        let mut changed = false;
        for card in &mut self.cards {
            changed |= card.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        self.min_card_w = self.width.min_width(theme);
        self.child_nodes = self
            .cards
            .iter_mut()
            .map(|card| card.build_layout(taffy, theme, node_map))
            .collect();
        let node = taffy.parent_node(self.grid_style(), &self.child_nodes);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_style(&mut self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        let Ok(layout) = taffy.layout(node) else {
            return false;
        };
        // Resolve the tracks at the allocated width before measuring row
        // minima, so each row is measured at its final column width.
        let width = f64::from(layout.size.width);
        let columns = self.columns_for_width(width);
        let column_width = crate::num::f32_of_f64(card_column_width(width, columns, self.gap));
        let tracks = vec![length(column_width); columns];
        let Ok(current) = taffy.style(node) else {
            return false;
        };
        if current.grid_template_columns == tracks {
            return false;
        }
        let mut style = current.clone();
        style.grid_template_columns = tracks;
        taffy.set_style(node, style).is_ok()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
        let origin = Point::new(self.rect.x0, self.rect.y0);
        for (card, &child) in self.cards.iter_mut().zip(&self.child_nodes) {
            card.apply_layout(taffy, child, origin);
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
        for card in &mut self.cards {
            if presence.should_cull(card.get_rect()) {
                continue;
            }
            card.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        for (card, &node) in self.cards.iter_mut().zip(self.child_nodes.iter()) {
            card.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.cards.iter().map(std::convert::AsRef::as_ref).collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for card in &self.cards {
            visitor(card.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.cards
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn mark_drawn(&mut self) {
        for card in &mut self.cards {
            card.mark_drawn();
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "CardGrid"
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for card in &mut self.cards {
            dirty |= card.take_structure_dirty();
        }
        dirty
    }
}

impl crate::undo::UndoPoints for CardGrid {
    fn needs_commit(&self) -> bool {
        self.cards
            .iter()
            .any(|c| crate::undo::needs_commit(c.as_ref()))
    }
    fn on_commit(&mut self) {
        for card in &mut self.cards {
            crate::undo::on_commit(card.as_mut());
        }
    }
}

#[cfg(test)]
mod tests;

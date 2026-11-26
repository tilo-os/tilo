//! `Card` and `CardGrid`: objects in content.

use std::collections::HashMap;
use tilo_ui_base::num::f64_of_usize;

use taffy::style_helpers::{auto, fr, length, max_content, min_content, minmax, percent, repeat};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::widget::composition_parts::Parts;

use crate::widget::activation::{Activation, Interaction, activation_controls};
use crate::{widget::Button, widget::Text};
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::{widget::Element, widget::TaffyTreeExt, widget::Tone, widget::Widget};

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
    let total_gap = f64_of_usize(cols.saturating_sub(1)) * gap;
    ((avail_w - total_gap) / f64_of_usize(cols)).max(0.0)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CardMetrics {
    padding: f64,
    gap: f64,
    pub(crate) radius: f64,
    stripe: f64,
}

impl CardMetrics {
    pub(crate) fn new(theme: &Theme) -> Self {
        let rhythm = &theme.tokens.spacing.rhythm;
        Self {
            padding: f64::from(rhythm.section),
            gap: f64::from(rhythm.sibling),
            radius: theme.radius_standard,
            stripe: f64::from(theme.tokens.metrics.chrome_rule_px) * 3.0,
        }
    }
}

/// A card's fill: the raised surface, lifted under a pointer over a card a press opens and
/// further while it is pressed.
pub(crate) fn surface_fill(theme: &Theme, interaction: Interaction, pressable: bool) -> Color {
    let raised = theme.tokens.colors.surfaces.raised;
    if pressable && !interaction.disabled && interaction.pressed {
        theme.tokens.colors.lift_on(raised, 1.0)
    } else if pressable && !interaction.disabled && interaction.hovered {
        theme.tokens.colors.lift_on(raised, 0.5)
    } else {
        raised
    }
}

pub(crate) fn paint_surface(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    stripe: Option<Tone>,
    interaction: Interaction,
    pressable: bool,
    selected: bool,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let theme = ctx.theme;
    let m = CardMetrics::new(theme);
    let fill = surface_fill(theme, interaction, pressable);
    tilo_ui_base::surface::selectable_card(
        ctx.scene,
        ctx.transform,
        (rect, m.radius),
        fill,
        selected,
        theme,
    );
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

/// Immediate-mode card: the surface, leading stripe, title and focus ring
/// of a [`Card`], for a caller that paints its own body into
/// [`CardFace::body_rect`].
#[derive(Clone, Copy, Debug, Default)]
pub struct CardFace<'a> {
    title: Option<&'a str>,
    stripe: Option<Tone>,
    pressable: bool,
    selected: bool,
    interaction: Interaction,
}

impl<'a> CardFace<'a> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    #[must_use]
    pub fn leading_stripe(mut self, tone: Tone) -> Self {
        self.stripe = Some(tone);
        self
    }

    #[must_use]
    pub fn pressable(mut self) -> Self {
        self.pressable = true;
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
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
        let wrap = tilo_ui_base::num::f32_of_f64((width - 2.0 * m.padding).max(1.0));
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

    /// The theme for what a host paints on the card: its ground is the
    /// card's own face, so a Secondary button lifts off the card instead of
    /// sinking into it, and ink and lifts are judged against the card.
    #[must_use]
    pub fn content_theme(theme: &Theme) -> Theme {
        theme.on_ground(theme.tokens.colors.surfaces.raised)
    }

    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        paint_surface(
            ctx,
            rect,
            self.stripe,
            self.interaction,
            self.pressable,
            self.selected,
        );
        if let Some(title) = self.title {
            let m = CardMetrics::new(ctx.theme);
            let style = ctx.theme.text(TextRole::Subheading);
            let wrap = tilo_ui_base::num::f32_of_f64((rect.width() - 2.0 * m.padding).max(1.0));
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

struct CardHeader {
    parts: Parts,
}

impl Widget for CardHeader {
    tilo_ui_base::retained_parts!("CardHeader");

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
                justify_items: Some(taffy::AlignItems::Stretch),
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
        // The trailing action (a button, a `Row` or `Actions` of icon
        // buttons) keeps its own width in the `auto` column instead of
        // claiming the header's.
        if let Some(&action) = self.parts.nodes.get(1)
            && let Ok(style) = taffy.style(action)
        {
            let mut style = style.clone();
            style.size.width = auto();
            style.justify_self = Some(taffy::AlignItems::End);
            style.flex_shrink = 0.0;
            let _ = taffy.set_style(action, style);
        }
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.parts.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
}

pub struct Card {
    parts: Parts,
    media_first: bool,
    has_header: bool,
    has_footer: bool,
    stripe: Option<Tone>,
    activation: Activation,
    /// What the pointer coming over the card does (`hover`): a view of what is under it, reported
    /// once each time it arrives.
    hover: tilo_ui_base::component::Event,
    hovering: bool,
    cursor: Point,
    dirty: bool,
    title: Option<String>,
    header_count: Option<usize>,
    accessible_name: Option<String>,
    selected: bool,
}

impl Card {
    pub fn new(body: Element) -> Self {
        Self {
            parts: Parts::new(vec![body]),
            media_first: false,
            has_header: false,
            has_footer: false,
            stripe: None,
            activation: Activation::new("card"),
            hover: tilo_ui_base::component::Event::None,
            hovering: false,
            cursor: Point::ZERO,
            dirty: true,
            title: None,
            header_count: None,
            accessible_name: None,
            selected: false,
        }
    }

    /// A card led by its art (a cover, a rendered preview): `media` spans
    /// the card's top at its full inner width, and the header
    /// ([`Self::with_title`], [`Self::with_title_action`]) sits under it, so
    /// the object's name and its actions read as the caption of its picture.
    pub fn with_media(media: Element) -> Self {
        Self {
            media_first: true,
            ..Self::new(media)
        }
    }

    /// The selected object among its peers: its edge heavier and in the
    /// accent, in place of the rest edge ([`tilo_ui_base::surface::selected_edge`]),
    /// announced as selected.
    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn get_selected(&self) -> bool {
        self.selected
    }

    pub fn set_selected(&mut self, selected: bool) {
        if self.selected != selected {
            self.selected = selected;
            self.dirty = true;
        }
    }

    /// What assistive technology calls the card; its title by default.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        let title = title.into();
        self.title = Some(title.clone());
        self.with_header_parts(
            Box::new(
                Text::new(&title)
                    .with_role(TextRole::Subheading)
                    .with_heading_level(3),
            ),
            None,
        )
    }

    #[must_use]
    pub fn with_title_action(mut self, title: impl Into<String>, action: Element) -> Self {
        let title = title.into();
        self.title = Some(title.clone());
        self.with_header_parts(
            Box::new(
                Text::new(&title)
                    .with_role(TextRole::Subheading)
                    .with_heading_level(3),
            ),
            Some(action),
        )
    }

    #[must_use]
    pub fn with_section_header(mut self, header: super::SectionHeader) -> Self {
        self.title = Some(header.title().to_owned());
        self.header_count = header.stated_count();
        self.with_header_parts(header.into_element(), None)
    }

    fn with_header_parts(mut self, title: Element, action: Option<Element>) -> Self {
        self.set_header_parts(title, action);
        self
    }

    pub(crate) fn set_header(&mut self, header: super::SectionHeader) {
        self.title = Some(header.title().to_owned());
        self.header_count = header.stated_count();
        self.set_header_parts(header.into_element(), None);
    }

    pub(crate) fn remove_header(&mut self) {
        if !self.has_header {
            return;
        }
        self.parts.children.remove(usize::from(self.media_first));
        self.has_header = false;
        self.title = None;
        self.header_count = None;
        self.dirty = true;
    }

    fn set_header_parts(&mut self, title: Element, action: Option<Element>) {
        let mut children = vec![title];
        if let Some(action) = action {
            children.push(action);
        }
        let header: Element = Box::new(CardHeader {
            parts: Parts::new(children),
        });
        let at = usize::from(self.media_first);
        if self.has_header
            && let Some(slot) = self.parts.children.get_mut(at)
        {
            *slot = header;
        } else {
            self.parts.children.insert(at, header);
            self.has_header = true;
        }
        self.dirty = true;
    }

    #[must_use]
    pub fn with_footer(mut self, actions: Actions) -> Self {
        if self.has_footer {
            self.parts.children.pop();
        }
        self.parts.children.push(Box::new(actions));
        self.has_footer = true;
        self
    }

    #[must_use]
    pub fn leading_stripe(mut self, tone: Tone) -> Self {
        self.stripe = Some(tone);
        self
    }

    #[must_use]
    pub fn on_activate(mut self, id: impl Into<String>, f: impl FnMut() + 'static) -> Self {
        self.activation = Activation::new(id);
        self.activation.set_callback(Box::new(f));
        self
    }

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
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    tilo_ui_base::provides!(dyn tilo_ui_base::widget::controls::ActionControls,);

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let before = self.parts.rect;
        self.parts.apply_layout(taffy, node, origin);
        if before != self.parts.rect {
            self.dirty = true;
        }
    }

    fn update(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        let mut changed = false;
        for child in &mut self.parts.children {
            changed |= child.update(state, path);
        }
        changed
    }

    fn sync(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) {
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
        // The body (a media card's caption) is at least as tall as what it holds, and grows
        // in a row taller than the card. Its floor is its max-content height, which is the
        // height an open card lays it out at (its `fr` takes the body's max-content where
        // the card's height is open): measured under a min-content constraint (a hugging
        // grid's rows, a column shrinking what it holds to its minimum), the card is then
        // as tall as it is laid out. A min-content floor measured a body holding layers
        // without them, as a layers' cell takes up nothing under that constraint, so the card
        // stood past the grid that measured it.
        let body = || minmax(max_content(), fr(1.0_f32));
        let mut tracks = Vec::new();
        if self.media_first {
            // The art keeps its own height; a row taller than this card
            // grows its caption, so the art never stretches.
            tracks.push(min_content());
            if self.has_header {
                tracks.push(body());
            }
        } else {
            if self.has_header {
                tracks.push(min_content());
            }
            tracks.push(body());
        }
        if self.has_footer {
            tracks.push(min_content());
        }
        let pad = tilo_ui_base::num::f32_of_f64(m.padding);
        let stripe_pad = if self.stripe.is_some() {
            tilo_ui_base::num::f32_of_f64(m.stripe)
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
                    width: length(tilo_ui_base::num::f32_of_f64(m.gap)),
                    height: length(tilo_ui_base::num::f32_of_f64(m.gap)),
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

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        if env.should_cull(self.parts.rect) {
            return;
        }
        paint_surface(
            &mut PaintCtx::new(scene, transform, text, theme),
            self.parts.rect,
            self.stripe,
            self.activation.state,
            self.activation.is_bound(),
            self.selected,
        );
        let inner = CardFace::content_theme(theme);
        self.parts.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            &inner,
            env.clone(),
        ));
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        // The pointer coming over the card is the card's own, whether or not something in it takes
        // the same move: a view that shows what is under the pointer hears it once, on arrival.
        if !self.hover.is_none() {
            match event {
                winit::event::WindowEvent::CursorMoved { position, .. } => {
                    self.cursor = ctx.logical(position);
                    let over = self.parts.rect.contains(self.cursor);
                    if over && !self.hovering {
                        self.hovering = true;
                        self.hover.fire(None);
                    } else if !over {
                        self.hovering = false;
                    }
                }
                winit::event::WindowEvent::CursorLeft { .. } => self.hovering = false,
                _ => {}
            }
        }
        for (child, &node) in self.parts.children.iter_mut().zip(&self.parts.nodes) {
            let path = ctx.path;
            child.handle_event(event, &mut ctx.reborrow(path, node));
            if *ctx.handled {
                if self.activation.state.pressed {
                    self.activation.cancel_press();
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
        self.dirty
            || self
                .parts
                .children
                .iter()
                .any(|c| c.needs_redraw() || c.needs_animation_frame())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for child in &mut self.parts.children {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
        let mut change = tilo_ui_base::widget::StructureChange::None;
        for child in &mut self.parts.children {
            change = change.join(child.take_structure_change());
        }
        change
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

    tilo_ui_base::pressable!(parts.rect);

    fn test_id(&self) -> Option<&str> {
        self.activation.is_bound().then(|| self.activation.id())
    }

    fn kept_in_view(&self) -> Option<&str> {
        self.activation.is_bound().then(|| self.activation.id())
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        let name = self.accessible_name.clone().or_else(|| self.title.clone());
        if !self.activation.is_bound() {
            // An object that does not open still has a name and may be the
            // selected one.
            return (name.is_some() || self.selected).then(|| {
                AccessNode::new(Role::Group)
                    .with_optional_name(name)
                    .with_state(AccessState::new().selected(self.selected))
            });
        }
        Some(
            AccessNode::new(Role::Button)
                .with_semantic_key(self.activation.id())
                .with_optional_name(name)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.activation.state.focused)
                        .selected(self.selected),
                ),
        )
    }
}

activation_controls!(Card, activation);

/// Standard actions in declaration/focus order: secondaries, then primary.
/// Unspecified buttons use measured content sizing; explicit sizes remain manual.
/// Each wrapped line aligns to the trailing edge.
#[tilo_ui_base::component::not_a_component(
    part,
    "the action row of a card or a dialog, built from their actions"
)]
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
    tilo_ui_base::retained_parts!("Actions");

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
                size: Size::auto(),
                min_size: Size::from_lengths(0.0_f32, 0.0_f32),
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

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.parts.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
}

/// How wide a grid's cards are at least, before the grid drops a column.
/// Measured in lines of body text, so cards widen with the text scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardWidth {
    Compact,
    #[default]
    Regular,
    Wide,
}

impl CardWidth {
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
#[tilo_ui_base::component::not_a_component(
    part,
    "`ui.CardGrid` (`DescribedCardGrid`) is its described form, over cards of its own"
)]
pub struct CardGrid {
    cards: Vec<Element>,
    width: CardWidth,
    min_card_w: f64,
    gap: f64,
    rect: Rect,
    child_nodes: Vec<NodeId>,
    name: Option<String>,
    /// Whether the cards were changed by the keyed operations ([`Self::insert_card`],
    /// [`Self::remove_card`], [`Self::move_card`], [`Self::replace_card`]) since the last frame:
    /// a change the frame's damage describes, rather than a rebuild of the grid.
    structure: tilo_ui_base::widget::StructureChange,
    /// The card the keyboard is on: arrows move it, Home and End and the page keys reach its
    /// ends, Enter opens it, and edit and Delete act on it.
    cursor: Option<usize>,
    /// Whether the grid itself holds the keyboard.
    focused: bool,
    /// The box its last layout stood in, when a scroll view clipped it: how far a page key moves.
    viewport: Option<Rect>,
    /// What the grid reports when the keyboard moves the selection or acts on it, by the index
    /// of the card it stands on.
    on_select: Option<GridCallback>,
    on_edit: Option<GridCallback>,
    on_delete: Option<GridCallback>,
}

/// What a grid reports of the card at an index its keyboard acts on.
type GridCallback = Box<dyn FnMut(usize)>;

/// What a key asks of the card the keyboard is on, beside opening it.
#[derive(Clone, Copy)]
enum Report {
    Edit,
    Delete,
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
            name: None,
            structure: tilo_ui_base::widget::StructureChange::None,
            cursor: None,
            focused: false,
            viewport: None,
            on_select: None,
            on_edit: None,
            on_delete: None,
        }
    }

    /// Report the card the keyboard moved the selection to.
    pub(crate) fn set_on_select(&mut self, callback: impl FnMut(usize) + 'static) {
        self.on_select = Some(Box::new(callback));
    }

    /// Report the card the edit key was pressed on.
    pub(crate) fn set_on_edit(&mut self, callback: impl FnMut(usize) + 'static) {
        self.on_edit = Some(Box::new(callback));
    }

    /// Report the card Delete was pressed on.
    pub(crate) fn set_on_delete(&mut self, callback: impl FnMut(usize) + 'static) {
        self.on_delete = Some(Box::new(callback));
    }

    /// The card the keyboard is on, if any.
    #[must_use]
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Put the keyboard on the card at `index` and select it, reporting the move when it changed
    /// and `report` says so.
    pub(crate) fn select_card(&mut self, index: usize, report: bool) -> bool {
        if index >= self.cards.len() {
            return false;
        }
        let changed = self.cursor != Some(index);
        self.cursor = Some(index);
        self.show_selection();
        if changed
            && report
            && let Some(callback) = &mut self.on_select
        {
            callback(index);
        }
        changed
    }

    /// Take the card the props call selected, when one is: the app's own selection, which the
    /// keyboard then moves from.
    pub(crate) fn adopt_selection(&mut self) {
        if let Some(index) = self.cards.iter().position(|card| {
            card.as_any()
                .and_then(|any| any.downcast_ref::<Card>())
                .is_some_and(Card::get_selected)
        }) {
            self.cursor = Some(index);
        }
        self.show_selection();
    }

    /// Lift the card the keyboard is on, and only it.
    fn show_selection(&mut self) {
        for (index, card) in self.cards.iter_mut().enumerate() {
            if let Some(card) = card.as_any_mut().and_then(|any| any.downcast_mut::<Card>()) {
                card.set_selected(self.cursor == Some(index));
            }
        }
    }

    /// How many columns the grid is laid out in at its last width.
    fn columns(&self) -> usize {
        self.columns_for_width(self.rect.width()).max(1)
    }

    /// How many rows of cards a page key moves over: the rows its viewport shows.
    fn page_rows(&self) -> usize {
        let row = self
            .cards
            .first()
            .map_or(0.0, |card| card.get_rect().height() + self.gap);
        match self.viewport {
            Some(viewport) if row > 0.0 => (viewport.height() / row).floor().max(1.0) as usize,
            _ => 1,
        }
    }

    /// Move the keyboard over the cards as `key` says, selecting the card it lands on.
    fn step(&mut self, key: &Key) -> bool {
        let last = match self.cards.len() {
            0 => return false,
            len => len - 1,
        };
        let columns = self.columns();
        let current = self.cursor.unwrap_or(0).min(last);
        let next = match key {
            Key::Named(NamedKey::ArrowLeft) => current.saturating_sub(1),
            Key::Named(NamedKey::ArrowRight) => (current + 1).min(last),
            Key::Named(NamedKey::ArrowUp) => current.saturating_sub(columns),
            Key::Named(NamedKey::ArrowDown) => (current + columns).min(last),
            Key::Named(NamedKey::Home) => 0,
            Key::Named(NamedKey::End) => last,
            Key::Named(NamedKey::PageUp) => {
                current.saturating_sub(columns.saturating_mul(self.page_rows()))
            }
            Key::Named(NamedKey::PageDown) => {
                (current + columns.saturating_mul(self.page_rows())).min(last)
            }
            _ => return false,
        };
        self.select_card(next, true);
        true
    }

    /// Run the card the keyboard is on, as pressing it does.
    fn activate_cursor(&mut self) -> bool {
        let Some(index) = self.cursor else {
            return false;
        };
        self.cards
            .get_mut(index)
            .and_then(|card| card.as_any_mut())
            .and_then(|any| any.downcast_mut::<Card>())
            .is_some_and(|card| card.activation.activate())
    }

    fn report(&mut self, event: Report) -> bool {
        let Some(index) = self.cursor else {
            return false;
        };
        let callback = match event {
            Report::Edit => &mut self.on_edit,
            Report::Delete => &mut self.on_delete,
        };
        let Some(callback) = callback else {
            return false;
        };
        callback(index);
        true
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn set_accessible_name(&mut self, name: impl Into<String>) {
        self.name = Some(name.into());
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.cards.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    #[must_use]
    pub fn card(&self, index: usize) -> Option<&Element> {
        self.cards.get(index)
    }

    pub fn card_mut(&mut self, index: usize) -> Option<&mut Element> {
        self.cards.get_mut(index)
    }

    /// Show `card` at `index` instead of the one there, which it returns. The change is
    /// described to the window, so the other cards stay mounted where they are.
    pub fn replace_card(&mut self, index: usize, card: Element) -> Option<Element> {
        let slot = self.cards.get_mut(index)?;
        self.structure = self
            .structure
            .join(tilo_ui_base::widget::StructureChange::Described);
        Some(std::mem::replace(slot, card))
    }

    pub fn insert_card(&mut self, index: usize, card: Element) {
        let index = index.min(self.cards.len());
        self.cards.insert(index, card);
        self.structure = self
            .structure
            .join(tilo_ui_base::widget::StructureChange::Described);
    }

    pub fn remove_card(&mut self, index: usize) -> Option<Element> {
        if index >= self.cards.len() {
            return None;
        }
        self.structure = self
            .structure
            .join(tilo_ui_base::widget::StructureChange::Described);
        Some(self.cards.remove(index))
    }

    pub fn move_card(&mut self, from: usize, to: usize) {
        if from == to || from >= self.cards.len() {
            return;
        }
        let card = self.cards.remove(from);
        self.cards.insert(to.min(self.cards.len()), card);
        self.structure = self
            .structure
            .join(tilo_ui_base::widget::StructureChange::Described);
    }

    #[must_use]
    pub fn with_card_width(mut self, width: CardWidth) -> Self {
        self.width = width;
        self
    }

    /// How many columns fit `width`. A short section keeps the columns its
    /// width chose, so two cards are no wider than two of a full row.
    pub fn columns_for_width(&self, width: f64) -> usize {
        card_columns(width, self.min_card_w, self.gap)
    }

    fn grid_style(&self) -> Style {
        let gap = tilo_ui_base::num::f32_of_f64(self.gap);
        Style {
            display: taffy::Display::Grid,
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            align_self: Some(taffy::AlignSelf::Start),
            align_content: Some(taffy::AlignContent::Start),
            grid_template_columns: vec![repeat(
                "auto-fit",
                vec![minmax(
                    length(tilo_ui_base::num::f32_of_f64(self.min_card_w)),
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
    fn update(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
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
        let _ = taffy.set_restyled_for_width(node, true);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// The tracks of the width it is laid out at, before its rows are measured, so each row is
    /// measured at its final column width.
    fn style_for_width(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        width: taffy::AvailableSpace,
        _text: &mut tilo_ui_base::text_render::TextRenderer,
    ) {
        let tracks = match width {
            taffy::AvailableSpace::Definite(width) => {
                let width = f64::from(width);
                let columns = self.columns_for_width(width);
                let column =
                    tilo_ui_base::num::f32_of_f64(card_column_width(width, columns, self.gap));
                vec![length(column); columns]
            }
            // A question of how wide it could be keeps the tracks of the width it was last laid
            // out at: the rows they make are the height its parent places the next part under.
            _ => return,
        };
        let node = styles.node();
        let mut style = styles.style(node).clone();
        style.grid_template_columns = tracks;
        styles.set_style(node, style);
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        // The box a scroll view around it clips the grid to, when one does: a page key moves the
        // keyboard over what a person can see.
        self.viewport = tilo_ui_base::widget::layout_clip();
        let origin = Point::new(self.rect.x0, self.rect.y0);
        for (card, &child) in self.cards.iter_mut().zip(&self.child_nodes) {
            card.apply_layout(taffy, child, origin);
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        for card in &mut self.cards {
            if env.should_cull(card.get_rect()) {
                continue;
            }
            card.draw_child(scene, transform, text_renderer, theme, &env);
        }
        // The card the keyboard is on wears the focus ring, in the theme's own ink.
        if self.focused
            && let Some(rect) = self
                .cursor
                .and_then(|cursor| self.cards.get(cursor))
                .map(|card| card.get_rect())
        {
            crate::widget::paint_focus_ring(scene, transform, rect, theme.radius_standard, theme);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        for (card, &node) in self.cards.iter_mut().zip(self.child_nodes.iter()) {
            card.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
        // A press on a card's own face puts the keyboard in the grid, not on the card's box: the
        // card has no keyboard of its own, and the arrows are the grid's. A press on a control
        // inside the card keeps its own focus, which is deeper than the card's box.
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && ctx
            .focus_target
            .is_some_and(|target| self.child_nodes.contains(&target))
        {
            *ctx.focus_target = Some(ctx.node_id);
        }
        if let WindowEvent::KeyboardInput { event, .. } = event
            && self.focused
            && event.state == ElementState::Pressed
            && !*ctx.handled
        {
            *ctx.handled = match event.logical_key {
                Key::Named(NamedKey::Enter) => self.activate_cursor(),
                Key::Named(NamedKey::F2) => self.report(Report::Edit),
                Key::Named(NamedKey::Delete) => self.report(Report::Delete),
                ref key => self.step(key),
            };
            if *ctx.handled {
                *ctx.focus_target = Some(ctx.node_id);
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

    /// Its box is the rows its cards make: it asks layout when a card does, and a grid holding
    /// none has no row to size.
    fn is_layout_dirty(&self) -> bool {
        self.cards.iter().any(|card| card.is_layout_dirty())
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

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessState, a11y::Role};
        self.name.as_ref().map(|name| {
            let mut children: Vec<_> = self
                .cards
                .iter()
                .filter_map(|card| card.accessibility())
                .collect();
            // The card the keyboard is on is the one a reader names, without the focus leaving
            // the grid: the grid is one control, its cards the items it moves between.
            if let Some(node) = self.cursor.and_then(|cursor| children.get_mut(cursor)) {
                node.state.selected = true;
                node.state.active_descendant = self.focused;
            }
            tilo_ui_base::a11y::AccessNode::new(Role::List)
                .with_name(name.clone())
                .with_state(AccessState::new().focusable(true).focused(self.focused))
                .with_children(children)
        })
    }

    fn is_focusable(&self) -> bool {
        !self.cards.is_empty()
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused && self.cursor.is_none() && !self.cards.is_empty() {
            self.cursor = Some(0);
            self.show_selection();
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn kept_in_view(&self) -> Option<&str> {
        self.cursor
            .and_then(|cursor| self.cards.get(cursor))
            .and_then(|card| card.as_any())
            .and_then(|any| any.downcast_ref::<Card>())
            .filter(|card| card.activation.is_bound())
            .map(|card| card.activation.id())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure).rebuilds();
        for card in &mut self.cards {
            dirty |= card.take_structure_dirty();
        }
        dirty
    }

    fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
        let mut change = std::mem::take(&mut self.structure);
        for card in &mut self.cards {
            change = change.join(card.take_structure_change());
        }
        change
    }
}

#[cfg(test)]
mod tests;

mod component;
mod grid_component;
pub use component::CardProps;
pub use grid_component::{CardGridCardProps, CardGridProps, DescribedCardGrid};

//! `ConflictPicker` — modal widget that shows the alternatives
//! produced by a [`crate::widget::merge_strategy::ConflictResolver`]
//! side-by-side and lets the user pick one.
//!
//! ```text
//!  +--- Kanban move conflict ----------------------------------+
//!  |  ALICE's choice              BOB's choice                 |
//!  |  Move to Done                Move to In Review            |
//!  |  [Pick Alice's]              [Pick Bob's]                 |
//!  |              [Pick neither (keep current)]                |
//!  +----------------------------------------------------------+
//! ```
//!
//! A [`crate::widget::merge_strategy::ConflictKind::MergeOrPick`] with
//! `merge_preview: Some(_)` adds a third `[Use merged]` card. Cards lay out
//! side-by-side above `NARROW_THRESHOLD`/`MIN_CARD_W`, else stack
//! vertically.
//!
//! Outcomes flow through `on_pick`: [`PickerOutcome::Picked`],
//! [`PickerOutcome::PickedMerged`], [`PickerOutcome::KeepCurrent`],
//! [`PickerOutcome::Cancel`] (Esc / scrim click).

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::ON_FILL_MIN_CONTRAST;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::data_table::{text_band, type_line_height};
use crate::widget::merge_strategy::{Alternative, ConflictKind};

const PADDING: f64 = 16.0;
/// Vertical spacing between title row, cards, and the `[Pick neither]` row.
const ROW_GAP: f64 = 12.0;
const BUTTON_H: f64 = 32.0;
const TITLE_H: f64 = 28.0;
/// Width below which the picker wraps the alternatives into a
/// vertical stack instead of side-by-side cards. ~800 px viewport
/// minus 2*PADDING = 768 inner; below that we wrap.
const NARROW_THRESHOLD: f64 = 760.0;
/// Minimum width any single alternative card needs before it
/// becomes hard to read. Drives the wrap decision. Tuned so that:
/// - 2 cards at 800 px wide stay side-by-side (~378 px each)
/// - 3 cards at 800 px wide wrap to a vertical stack (~248 px
///   each would fall below this threshold)
/// - 2 or 3 cards at 1280 px wide stay side-by-side
const MIN_CARD_W: f64 = 260.0;
const CARD_H: f64 = 160.0;
/// Scrim alpha — matches the command-palette dim treatment.
const SCRIM_ALPHA: u8 = 153; // ~ 60%

/// A card count is a handful of alternatives plus maybe a merged preview —
/// always tiny, so the `usize -> f64` conversion used for layout division
/// cannot lose precision.
#[allow(
    clippy::cast_precision_loss,
    reason = "card count is a small UI count, well within f64 precision"
)]
fn card_count_f64(n: usize) -> f64 {
    n as f64
}

/// Composes `transform` with a translation by `(dx, dy)`. `kurbo::Affine`'s
/// `Mul` impl trips `arithmetic_side_effects` like any operator overload,
/// but composing two f64-based affine transforms cannot meaningfully
/// overflow — same reasoning clippy already exempts bare `f64` arithmetic
/// for.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine::mul composes f64 transform matrices; cannot meaningfully overflow"
)]
fn translated(transform: Affine, dx: f64, dy: f64) -> Affine {
    transform * Affine::translate((dx, dy))
}

/// What the user chose in the picker. Forwarded through the
/// `on_pick` callback so the host knows what to apply.
#[derive(Clone, Debug, PartialEq)]
pub enum PickerOutcome {
    /// User clicked one of the `[Pick X's]` buttons; the index is into the
    /// alternatives list the picker was constructed with.
    Picked { alternative_index: usize },
    /// User clicked `[Use merged]`. Only emitted when the kind was
    /// `MergeOrPick` with `merge_preview: Some(_)`.
    PickedMerged,
    /// User clicked `[Pick neither (keep current)]`. Host should
    /// leave the current scalar / move / rename as-is.
    KeepCurrent,
    /// User pressed Esc or clicked the scrim. Host typically
    /// re-mounts the banner so the user can try again later.
    Cancel,
}

pub type PickCallback = Box<dyn FnMut(PickerOutcome)>;

/// Side-by-side conflict picker. See module-level docs for the
/// contract.
pub struct ConflictPicker {
    family: String,
    kind: ConflictKind,
    rect: Rect,
    last_cursor: Point,
    /// Card index the cursor is hovering, or `None` outside any card.
    hover_alt: Option<usize>,
    /// Whether the merged preview card is hovered — only meaningful when
    /// the kind is `MergeOrPick` with a preview.
    hover_merged: bool,
    /// Whether the `[Pick neither]` button is hovered.
    hover_keep: bool,
    on_pick: Option<PickCallback>,
    dirty: bool,
    bands: PickerBands,
    /// The button label role resolved at `build_layout` and the keep
    /// button's side padding, for the renderer-aware layout hook.
    label_style: TextStyle,
    keep_pad: f64,
    /// Measured `(width, height)` of [`KEEP_LABEL`], resolved in layout so
    /// the button hit-testing reads is the button paint draws; `(0, 0)` until
    /// then, when the button keeps its pinned width.
    keep_label: (f32, f32),
}

/// The picker's structural bands. The constants are the 1× defaults; the
/// theme resolves taller bands at larger text scales so the card text stack
/// no longer runs under its pick button and the `[Pick neither]` label stays
/// inside its button. The keep-button width is measured from its label at
/// paint time (the geometry helpers are theme-less for hit-testing).
#[derive(Clone, Copy, Debug, PartialEq)]
struct PickerBands {
    title_h: f64,
    card_h: f64,
    /// Band reserved for a pick button under the card text.
    button_h: f64,
    /// The pill inside that band.
    pill_h: f64,
    keep_w: f64,
}

/// Inset between a card's edge and its text stack.
const CARD_PAD_X: f64 = 12.0;
const CARD_PAD_Y: f64 = 10.0;
/// The pick pill at the default text scale; `BUTTON_H` is its band.
const PILL_H: f64 = 24.0;
const KEEP_W: f64 = 240.0;
const KEEP_LABEL: &str = "Pick neither (keep current)";
/// Side padding of the keep button before a theme is seen: the `s3` step,
/// unscaled. `build_layout` reads the live `spacing.s3`.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const; f32 -> f64 is exact"
)]
const DEFAULT_KEEP_SIDE_PAD: f64 = crate::tokens::Spacing::BASE.s3 as f64;

impl PickerBands {
    const DEFAULT: Self = Self {
        title_h: TITLE_H,
        card_h: CARD_H,
        button_h: BUTTON_H,
        pill_h: PILL_H,
        keep_w: KEEP_W,
    };

    fn resolve(theme: &Theme, keep_w: f64) -> Self {
        let tokens = &theme.tokens;
        let md = type_line_height(&tokens.typography, TextRole::Body);
        let sm = type_line_height(&tokens.typography, TextRole::Caption);
        let lg = type_line_height(&tokens.typography, TextRole::Heading);
        let tight = f64::from(tokens.spacing.s1);
        let group = f64::from(tokens.spacing.s2);
        let pill_h = text_band(PILL_H, sm, tight);
        let button_h = text_band(BUTTON_H, pill_h, group);
        // author / timestamp, then label / summary: two tight pairs one
        // group gap apart, exactly as `draw_card` stacks them.
        let stack = md + tight + sm + group + md + tight + sm;
        Self {
            title_h: text_band(TITLE_H, lg, tight),
            card_h: text_band(CARD_H, stack + button_h, CARD_PAD_Y * 2.0 + group),
            button_h,
            pill_h,
            keep_w,
        }
    }
}

impl ConflictPicker {
    /// Build a picker for `family` (the conflict family display
    /// name) + `kind` (the classification the resolver produced).
    pub fn new(family: impl Into<String>, kind: ConflictKind) -> Self {
        Self {
            family: family.into(),
            kind,
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            hover_alt: None,
            hover_merged: false,
            hover_keep: false,
            on_pick: None,
            dirty: true,
            bands: PickerBands::DEFAULT,
            label_style: crate::tokens::typography::stock().style(TextRole::Label),
            keep_pad: 2.0 * DEFAULT_KEEP_SIDE_PAD,
            keep_label: (0.0, 0.0),
        }
    }

    /// Builder: install the pick callback. Fires for every outcome
    /// listed in [`PickerOutcome`].
    #[must_use]
    pub fn on_pick<F: FnMut(PickerOutcome) + 'static>(mut self, f: F) -> Self {
        self.on_pick = Some(Box::new(f));
        self
    }

    pub fn family(&self) -> &str {
        &self.family
    }

    pub fn kind(&self) -> &ConflictKind {
        &self.kind
    }

    pub fn hover_alt(&self) -> Option<usize> {
        self.hover_alt
    }

    /// Empty for `ConflictKind::Auto`, which the picker should never be
    /// mounted for — defending the invariant is cheap.
    fn alternatives(&self) -> &[Alternative] {
        match &self.kind {
            ConflictKind::PickOne { alternatives }
            | ConflictKind::MergeOrPick { alternatives, .. } => alternatives,
            ConflictKind::Auto => &[],
        }
    }

    pub fn has_merged_card(&self) -> bool {
        matches!(
            &self.kind,
            ConflictKind::MergeOrPick {
                merge_preview: Some(_),
                ..
            }
        )
    }

    fn merge_preview(&self) -> Option<&str> {
        match &self.kind {
            ConflictKind::MergeOrPick {
                merge_preview: Some(s),
                ..
            } => Some(s.as_str()),
            _ => None,
        }
    }

    /// Rect of each alternative card and the optional merged card. Reserves
    /// a title row at top and a `[Pick neither]` row at bottom; the middle
    /// band lays cards side-by-side unless the per-card width would fall
    /// below `MIN_CARD_W`, in which case it stacks vertically.
    pub fn card_rects(&self) -> Vec<Rect> {
        let mut rects = Vec::new();
        let alt_count = self.alternatives().len();
        let total_cards = alt_count.saturating_add(usize::from(self.has_merged_card()));
        if total_cards == 0 {
            return rects;
        }

        let inner = self.inner_rect();
        let middle_top = inner.y0 + self.bands.title_h + ROW_GAP;
        let middle_bot = inner.y1 - self.bands.button_h - ROW_GAP;
        let middle_h = (middle_bot - middle_top).max(0.0);

        // Cap to the card band (`CARD_H` at 1×, taller with the type); if the
        // middle band is shorter, use what's there so the picker still
        // renders cleanly inside small modals.
        let card_h = self.bands.card_h.min(middle_h.max(40.0));

        let gap = ROW_GAP;
        let available_w = inner.width();
        let total_f = card_count_f64(total_cards);
        let side_by_side_card_w = (available_w - gap * (total_f - 1.0).max(0.0)) / total_f;

        if available_w >= NARROW_THRESHOLD && side_by_side_card_w >= MIN_CARD_W {
            let mut x = inner.x0;
            for _ in 0..total_cards {
                rects.push(Rect::new(
                    x,
                    middle_top,
                    x + side_by_side_card_w,
                    middle_top + card_h,
                ));
                x += side_by_side_card_w + gap;
            }
        } else {
            // Stacked layout: cap per-card height so the stack still fits the middle band.
            let stacked_card_h =
                ((middle_h - gap * (total_f - 1.0).max(0.0)) / total_f).min(card_h);
            let mut y = middle_top;
            for _ in 0..total_cards {
                rects.push(Rect::new(inner.x0, y, inner.x1, y + stacked_card_h));
                y += stacked_card_h + gap;
            }
        }
        rects
    }

    /// Rect of the `[Pick neither (keep current)]` button. Anchored
    /// to the bottom of the inner rect, centred horizontally.
    pub fn keep_current_rect(&self) -> Rect {
        let inner = self.inner_rect();
        let w = self.bands.keep_w.min(inner.width() - 2.0 * PADDING);
        let cx = f64::midpoint(inner.x0, inner.x1);
        let y1 = inner.y1;
        Rect::new(cx - w / 2.0, y1 - self.bands.button_h, cx + w / 2.0, y1)
    }

    fn inner_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y1 - PADDING,
        )
    }

    fn title_rect(&self) -> Rect {
        let inner = self.inner_rect();
        Rect::new(inner.x0, inner.y0, inner.x1, inner.y0 + self.bands.title_h)
    }

    /// Per-alternative pick-button rect at card index `i`. The merged
    /// card, when present, occupies the trailing index and comes back
    /// through here too.
    pub fn pick_button_rect(&self, i: usize) -> Option<Rect> {
        let cards = self.card_rects();
        let card = cards.get(i)?;
        let inset = 8.0;
        let band = self.bands.button_h;
        let pill = self.bands.pill_h;
        Some(Rect::new(
            card.x0 + inset,
            card.y1 - band + (band - pill) / 2.0,
            card.x1 - inset,
            card.y1 - (band - pill) / 2.0,
        ))
    }

    /// Index into `card_rects()` of the card containing `p`, or `None`.
    pub fn card_at(&self, p: Point) -> Option<usize> {
        for (i, r) in self.card_rects().iter().enumerate() {
            if r.contains(p) {
                return Some(i);
            }
        }
        None
    }
}

/// Geometry + hover state for one card, bundled so `draw_card` doesn't need
/// three separate parameters for it.
struct CardChrome {
    card: Rect,
    hovered: bool,
    pick_button: Rect,
}

/// Text content for one alternative/merged card, bundled so `draw_card`
/// doesn't need five separate string parameters.
struct CardContent<'a> {
    label: &'a str,
    author: &'a str,
    timestamp_label: &'a str,
    summary: &'a str,
    button_label: &'a str,
}

/// Paint one alternative card: title + author + timestamp + pick button.
/// A pure function of its inputs — no `self` needed.
fn draw_card(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    chrome: &CardChrome,
    content: &CardContent<'_>,
) {
    let card = chrome.card;

    // Hovered cards lift via accent border; at rest the edge derives from
    // the card fill (HIG §9.5) as a crisp 1 px inset stroke.
    let radius = f64::from(theme.tokens.radii.control);
    let fill = theme.tokens.colors.surfaces.base;
    if chrome.hovered {
        let body = RoundedRect::from_rect(card, radius);
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &body);
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.tokens.colors.border_focus,
            None,
            &body,
        );
    } else {
        crate::surface::card(scene, transform, card, radius, fill);
    }

    let inner = Rect::new(
        card.x0 + CARD_PAD_X,
        card.y0 + CARD_PAD_Y,
        card.x1 - CARD_PAD_X,
        card.y1 - CARD_PAD_Y,
    );
    let text_size_sm = theme.text(TextRole::Caption);
    let text_size_md = theme.text(TextRole::Body);
    // Author/timestamp and label/summary are two tight pairs separated by one
    // group gap; each line advances by its measured line box.
    let tight = f64::from(theme.tokens.spacing.s1);
    let group = f64::from(theme.tokens.spacing.s2);

    let (_, author_h) = text_renderer.measure_style(content.author, None, &text_size_md);
    text_renderer.draw_style(
        scene,
        content.author,
        theme.tokens.colors.text_primary,
        translated(transform, inner.x0, inner.y0),
        None,
        &text_size_md,
    );
    let ts_y = inner.y0 + f64::from(author_h) + tight;
    let timestamp_style = text_size_sm.clone().numeric();
    let (_, ts_h) = text_renderer.measure_style(content.timestamp_label, None, &timestamp_style);
    text_renderer.draw_style(
        scene,
        content.timestamp_label,
        theme.tokens.colors.text_dim,
        translated(transform, inner.x0, ts_y),
        None,
        &timestamp_style,
    );

    let label_y = ts_y + f64::from(ts_h) + group;
    let (_, label_h) = text_renderer.measure_style(content.label, None, &text_size_md);
    text_renderer.draw_style(
        scene,
        content.label,
        theme.tokens.colors.text_primary,
        translated(transform, inner.x0, label_y),
        None,
        &text_size_md,
    );

    // Single-line, not wrapped — matches the picker's compact shape.
    let summary_y = label_y + f64::from(label_h) + tight;
    text_renderer.draw_style(
        scene,
        content.summary,
        theme.tokens.colors.text_dim,
        translated(transform, inner.x0, summary_y),
        None,
        &text_size_sm,
    );

    let pick_button = chrome.pick_button;
    let btn_shape = RoundedRect::from_rect(pick_button, f64::from(theme.tokens.radii.detail));
    // A primary action: the emphasis fill under its own label
    // (`intent.on_inverted`), resolved against the fill it is painted on.
    let fill = theme.tokens.colors.intent.inverted;
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        fill,
        None,
        &btn_shape,
    );
    let btn_text_style = theme.text(TextRole::Label);
    let (bw, bh) = text_renderer.measure_style(content.button_label, None, &btn_text_style);
    text_renderer.draw_style(
        scene,
        content.button_label,
        theme.tokens.colors.ink_on(
            fill,
            theme.tokens.colors.intent.on_inverted,
            ON_FILL_MIN_CONTRAST,
        ),
        translated(
            transform,
            pick_button.x0 + (pick_button.width() - f64::from(bw)) / 2.0,
            pick_button.y0 + (pick_button.height() - f64::from(bh)) / 2.0,
        ),
        None,
        &btn_text_style,
    );
}

impl Widget for ConflictPicker {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.bands = PickerBands::resolve(theme, self.bands.keep_w);
        self.label_style = theme.text(TextRole::Label);
        self.keep_pad = 2.0 * f64::from(theme.tokens.spacing.s3);
        // Sized by the host: expands to fill whatever rect the parent
        // assigns — typical use is a centered modal, e.g. 720×360.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// The keep button's label is measured here, during layout, so the
    /// width `keep_current_rect` hit-tests is the width paint draws. The
    /// button holds its label plus a sibling step each side, floored at the
    /// pinned width so the 1× label keeps 240 px.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        self.keep_label = text.measure_style(KEEP_LABEL, None, &self.label_style);
        self.bands.keep_w = text_band(KEEP_W, f64::from(self.keep_label.0), self.keep_pad);
        self.adjust_layout_style(taffy, node)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let prev = self.rect;
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if prev != self.rect {
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if matches!(self.kind, ConflictKind::Auto) || self.alternatives().is_empty() {
            // Nothing to pick — degrade gracefully (paint nothing).
            self.dirty = false;
            return;
        }

        // The widget tree doesn't know the screen dims here, so the scrim
        // fills a very large rect around the modal instead; downstream
        // chrome clips it to the viewport in a full-screen container.
        let scrim_color = Color::from_rgba8(0, 0, 0, SCRIM_ALPHA);
        let scrim_rect = self.rect.inflate(2000.0, 2000.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            scrim_color,
            None,
            &scrim_rect,
        );

        let body = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.control));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &body,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.intent.inverted,
            None,
            &body,
        );

        let title = self.title_rect();
        let title_text = format!("{} conflict", self.family);
        let title_style = theme.text(TextRole::Heading);
        let (_tw, th) = text_renderer.measure_style(&title_text, None, &title_style);
        text_renderer.draw_style(
            scene,
            &title_text,
            theme.tokens.colors.text_primary,
            translated(
                transform,
                title.x0,
                title.y0 + (title.height() - f64::from(th)) / 2.0,
            ),
            None,
            &title_style,
        );

        self.draw_cards(scene, transform, text_renderer, theme);
        self.draw_keep_button(scene, transform, text_renderer, theme);

        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if matches!(self.kind, ConflictKind::Auto) {
            return;
        }
        let alt_count = self.alternatives().len();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = p;
                let card_idx = self.card_at(p);
                let new_alt = card_idx.filter(|&i| i < alt_count);
                let new_merged = card_idx.is_some_and(|i| i == alt_count && self.has_merged_card());
                let new_keep = self.keep_current_rect().contains(p);
                if new_alt != self.hover_alt
                    || new_merged != self.hover_merged
                    || new_keep != self.hover_keep
                {
                    self.hover_alt = new_alt;
                    self.hover_merged = new_merged;
                    self.hover_keep = new_keep;
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Buttons sit *inside* the card rect, so a click on a button
                // is also a click on the card — test buttons first.
                let total = alt_count.saturating_add(usize::from(self.has_merged_card()));
                for i in 0..total {
                    if let Some(btn) = self.pick_button_rect(i)
                        && btn.contains(self.last_cursor)
                    {
                        if i < alt_count {
                            self.fire(PickerOutcome::Picked {
                                alternative_index: i,
                            });
                        } else {
                            self.fire(PickerOutcome::PickedMerged);
                        }
                        *ctx.handled = true;
                        return;
                    }
                }
                if self.keep_current_rect().contains(self.last_cursor) {
                    self.fire(PickerOutcome::KeepCurrent);
                    *ctx.handled = true;
                    return;
                }
                // Click on the scrim (outside the modal rect) cancels.
                if !self.rect.contains(self.last_cursor) {
                    self.fire(PickerOutcome::Cancel);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                let KeyEvent {
                    logical_key,
                    state: element_state,
                    ..
                } = key_event;
                if *element_state == ElementState::Pressed
                    && let Key::Named(NamedKey::Escape) = logical_key
                {
                    self.fire(PickerOutcome::Cancel);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ConflictPicker"
    }

    /// `Role::Group` named for the family; the description names the
    /// conflict kind so an AT can announce what choice is being asked for.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(
            AccessNode::new(Role::Group)
                .with_name(format!("Resolve conflict: {}", self.family))
                .with_description(format!("{:?}", self.kind)),
        )
    }

    fn test_id(&self) -> Option<&str> {
        Some("conflict_picker")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl ConflictPicker {
    /// Fire the pick callback. Centralised so the event handler
    /// stays compact.
    fn fire(&mut self, outcome: PickerOutcome) {
        if let Some(cb) = self.on_pick.as_mut() {
            cb(outcome);
        }
    }

    /// Paints each alternative/merged card in `card_rects()` order.
    fn draw_cards(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let cards = self.card_rects();
        let alt_count = self.alternatives().len();
        let now = crate::time::wall();
        for (i, card) in cards.iter().enumerate() {
            let hovered = self.hover_alt == Some(i)
                || (i == alt_count && self.has_merged_card() && self.hover_merged);
            let Some(pick_btn) = self.pick_button_rect(i) else {
                continue;
            };
            let chrome = CardChrome {
                card: *card,
                hovered,
                pick_button: pick_btn,
            };
            if let Some(alt) = self.alternatives().get(i) {
                let ts_label = crate::widget::history::format_relative_time(alt.timestamp, now);
                let button_label = format!("Pick {}'s", alt.author);
                let content = CardContent {
                    label: &alt.label,
                    author: &alt.author,
                    timestamp_label: &ts_label,
                    summary: &alt.summary,
                    button_label: &button_label,
                };
                draw_card(scene, transform, text_renderer, theme, &chrome, &content);
            } else {
                // `draw_card` paints the standard accent pick button; a
                // success-tinted overlay layers on top so merge reads distinct.
                let preview = self.merge_preview().unwrap_or("");
                let content = CardContent {
                    label: "Merged preview",
                    author: "auto",
                    timestamp_label: "concatenated",
                    summary: preview,
                    button_label: "Use merged",
                };
                draw_card(scene, transform, text_renderer, theme, &chrome, &content);
                draw_merged_overlay(scene, transform, text_renderer, theme, pick_btn);
            }
        }
    }

    /// Paints the `[Pick neither (keep current)]` button at the bottom.
    fn draw_keep_button(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let keep = self.keep_current_rect();
        let keep_radius = f64::from(theme.tokens.radii.detail);
        // A Neutral button face (HIG §9.5): the raised tier at rest, its
        // half lift when hovered, edge derived from the face — not
        // a white veneer that is invisible on a light palette.
        let keep_body = if self.hover_keep {
            theme
                .tokens
                .colors
                .lift_on(theme.tokens.colors.surfaces.raised, 0.5)
        } else {
            theme.tokens.colors.surfaces.raised
        };
        crate::surface::card(scene, transform, keep, keep_radius, keep_body);
        let keep_style = theme.text(TextRole::Caption);
        // Centre on the measurement the button was sized from (see
        // `adjust_layout_with_measure`); a host that laid out without the
        // renderer-aware hook measures here instead.
        let (kw, kh) = if self.keep_label.0 > 0.0 {
            self.keep_label
        } else {
            text_renderer.measure_style(KEEP_LABEL, None, &keep_style)
        };
        text_renderer.draw_style(
            scene,
            KEEP_LABEL,
            theme.tokens.colors.text_primary,
            translated(
                transform,
                keep.x0 + (keep.width() - f64::from(kw)) / 2.0,
                keep.y0 + (keep.height() - f64::from(kh)) / 2.0,
            ),
            None,
            &keep_style,
        );
    }
}

/// Layers the success-tinted `[Use merged]` button on top of the card
/// `draw_card` just painted, so the merged option reads visually distinct.
/// A pure function of its inputs — no `self` needed.
fn draw_merged_overlay(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    pick_btn: Rect,
) {
    let btn_shape = RoundedRect::from_rect(pick_btn, f64::from(theme.tokens.radii.detail));
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        theme.tokens.colors.semantic.success,
        None,
        &btn_shape,
    );
    let bl = "Use merged";
    let bsize = theme.text(TextRole::Caption);
    let (bw, bh) = text_renderer.measure_style(bl, None, &bsize);
    let colors = &theme.tokens.colors;
    text_renderer.draw_style(
        scene,
        bl,
        // Same tone convention as the pick buttons and `Button`: white is the
        // candidate ink, resolved by the theme against the fill it sits on.
        colors.ink_on(
            colors.semantic.success,
            vello::peniko::Color::WHITE,
            ON_FILL_MIN_CONTRAST,
        ),
        translated(
            transform,
            pick_btn.x0 + (pick_btn.width() - f64::from(bw)) / 2.0,
            pick_btn.y0 + (pick_btn.height() - f64::from(bh)) / 2.0,
        ),
        None,
        &bsize,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dispatch;
    use crate::time::{Duration, SystemTime};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    fn alt(label: &str, author: &str) -> Alternative {
        Alternative {
            label: label.into(),
            summary: format!("{author} chose {label}"),
            author: author.into(),
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(60))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            payload: Arc::from(Vec::<u8>::new()),
        }
    }

    fn picker_at(rect: Rect, kind: ConflictKind) -> ConflictPicker {
        let mut p = ConflictPicker::new("Kanban move", kind);
        p.rect = rect;
        p
    }

    #[test]
    fn picker_renders_alternatives_and_fits_narrow_viewports() {
        fn accessibility_reports_group_role_name_and_kind() {
            use crate::a11y::Role;
            let kind = ConflictKind::PickOne {
                alternatives: vec![alt("Done", "Alice"), alt("In progress", "Bob")],
            };
            let p = picker_at(Rect::new(0.0, 0.0, 500.0, 300.0), kind);
            let node = Widget::accessibility(&p).expect("picker is not presentational");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name.as_deref(), Some("Resolve conflict: Kanban move"));
            // The description names the conflict kind (Debug form).
            assert!(
                node.description.as_deref().unwrap().starts_with("PickOne"),
                "description should name the conflict kind, got {:?}",
                node.description
            );
        }

        fn picker_renders_two_alternatives_side_by_side() {
            // 1280-wide typical layout — two cards lay out side-by-side
            // and both rects fit inside the picker.
            let kind = ConflictKind::PickOne {
                alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
            };
            let p = picker_at(Rect::new(0.0, 0.0, 1280.0, 360.0), kind);
            let cards = p.card_rects();
            assert_eq!(cards.len(), 2);
            // Side-by-side: card[1] starts to the right of card[0].
            assert!(cards[1].x0 > cards[0].x0);
            assert!(p.rect.contains(cards[0].center()));
            assert!(p.rect.contains(cards[1].center()));
        }

        fn picker_merged_button_only_renders_when_preview_present() {
            // MergeOrPick with `merge_preview: None` produces no merged
            // card; with `Some(_)` it adds a trailing card.
            let kind_none = ConflictKind::MergeOrPick {
                alternatives: vec![alt("Rename to A", "Alice")],
                merge_preview: None,
            };
            let p_none = picker_at(Rect::new(0.0, 0.0, 1280.0, 360.0), kind_none);
            assert!(!p_none.has_merged_card());
            assert_eq!(p_none.card_rects().len(), 1);

            let kind_some = ConflictKind::MergeOrPick {
                alternatives: vec![alt("Rename to A", "Alice"), alt("Rename to B", "Bob")],
                merge_preview: Some("Combined: A / B".into()),
            };
            let p_some = picker_at(Rect::new(0.0, 0.0, 1280.0, 360.0), kind_some);
            assert!(p_some.has_merged_card());
            assert_eq!(p_some.card_rects().len(), 3);
        }

        fn picker_hover_alt_tracks_cursor_in_alternative_card() {
            let kind = ConflictKind::PickOne {
                alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
            };
            let mut p = picker_at(Rect::new(0.0, 0.0, 1280.0, 360.0), kind);
            let cards = p.card_rects();
            dispatch(
                &mut p,
                &cursor_moved(cards[0].center().x, cards[0].center().y),
            );
            assert_eq!(p.hover_alt(), Some(0));
            dispatch(
                &mut p,
                &cursor_moved(cards[1].center().x, cards[1].center().y),
            );
            assert_eq!(p.hover_alt(), Some(1));
            // Cache the bounds first so the borrow checker doesn't see a live
            // `&p` while we hand `&mut p` to `dispatch`.
            let outside_x = p.rect.x1 + 200.0;
            let outside_y = p.rect.y1 + 200.0;
            dispatch(&mut p, &cursor_moved(outside_x, outside_y));
            assert_eq!(p.hover_alt(), None);
        }

        fn picker_fits_in_narrow_viewport_via_stack() {
            // At 800 px wide with three total cards (2 alternatives + merged
            // preview), the picker must wrap to a vertical stack rather than
            // producing sub-MIN_CARD_W cards that would be unreadable.
            let kind = ConflictKind::MergeOrPick {
                alternatives: vec![alt("A", "Alice"), alt("B", "Bob")],
                merge_preview: Some("merged".into()),
            };
            let p = picker_at(Rect::new(0.0, 0.0, 800.0, 600.0), kind);
            let cards = p.card_rects();
            assert_eq!(cards.len(), 3);
            for c in &cards {
                assert!(c.width() > MIN_CARD_W, "stacked cards must be wide");
            }
            assert!(cards[1].y0 > cards[0].y0);
            assert!(cards[2].y0 > cards[1].y0);
        }

        accessibility_reports_group_role_name_and_kind();
        picker_renders_two_alternatives_side_by_side();
        picker_merged_button_only_renders_when_preview_present();
        picker_hover_alt_tracks_cursor_in_alternative_card();
        picker_fits_in_narrow_viewport_via_stack();
    }

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;

    use crate::synthetic_input::named_key;

    #[test]
    fn picker_buttons_and_dismissal_fire_the_matching_outcome() {
        fn picker_pick_button_fires_with_correct_index() {
            // Click the second alternative's button — outcome must be Picked { alternative_index: 1 }.
            let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
            let outcomes_clone = outcomes.clone();
            let kind = ConflictKind::PickOne {
                alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
            };
            let mut p = ConflictPicker::new("Kanban move", kind)
                .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
            p.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

            let btn = p.pick_button_rect(1).expect("button 1 must exist");
            dispatch(&mut p, &cursor_moved(btn.center().x, btn.center().y));
            dispatch(&mut p, &mouse_press());
            assert_eq!(outcomes.borrow().len(), 1);
            assert_eq!(
                outcomes.borrow()[0],
                PickerOutcome::Picked {
                    alternative_index: 1
                }
            );
        }

        fn picker_keep_current_fires_keep_current() {
            // Click the bottom-centre "Pick neither" button.
            let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
            let outcomes_clone = outcomes.clone();
            let kind = ConflictKind::PickOne {
                alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
            };
            let mut p = ConflictPicker::new("Kanban move", kind)
                .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
            p.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

            let keep = p.keep_current_rect();
            dispatch(&mut p, &cursor_moved(keep.center().x, keep.center().y));
            dispatch(&mut p, &mouse_press());
            assert_eq!(outcomes.borrow().len(), 1);
            assert_eq!(outcomes.borrow()[0], PickerOutcome::KeepCurrent);
        }

        fn picker_esc_fires_cancel() {
            // Esc → Cancel, driving the chrome's "changed their mind" path.
            let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
            let outcomes_clone = outcomes.clone();
            let kind = ConflictKind::PickOne {
                alternatives: vec![alt("Move to Done", "Alice")],
            };
            let mut p = ConflictPicker::new("Kanban move", kind)
                .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
            p.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

            dispatch(&mut p, &named_key(NamedKey::Escape));
            assert_eq!(outcomes.borrow().len(), 1);
            assert_eq!(outcomes.borrow()[0], PickerOutcome::Cancel);
        }

        fn picker_merged_pick_button_fires_picked_merged() {
            // Click on the merged card's `[Use merged]` button —
            // outcome must be PickedMerged (not Picked{2}).
            let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
            let outcomes_clone = outcomes.clone();
            let kind = ConflictKind::MergeOrPick {
                alternatives: vec![alt("Rename to A", "Alice"), alt("Rename to B", "Bob")],
                merge_preview: Some("Combined: A / B".into()),
            };
            let mut p = ConflictPicker::new("Rename collision", kind)
                .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
            p.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

            // The merged card is at trailing index 2 (alt_count = 2).
            let btn = p.pick_button_rect(2).expect("merged button must exist");
            dispatch(&mut p, &cursor_moved(btn.center().x, btn.center().y));
            dispatch(&mut p, &mouse_press());
            assert_eq!(outcomes.borrow().len(), 1);
            assert_eq!(outcomes.borrow()[0], PickerOutcome::PickedMerged);
        }

        fn picker_scrim_click_fires_cancel() {
            // Click outside the modal rect — the scrim absorbs the click as Cancel.
            let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
            let outcomes_clone = outcomes.clone();
            let kind = ConflictKind::PickOne {
                alternatives: vec![alt("A", "Alice")],
            };
            let mut p = ConflictPicker::new("Kanban move", kind)
                .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
            p.rect = Rect::new(200.0, 200.0, 1080.0, 560.0);

            dispatch(&mut p, &cursor_moved(50.0, 50.0));
            dispatch(&mut p, &mouse_press());
            assert_eq!(outcomes.borrow().len(), 1);
            assert_eq!(outcomes.borrow()[0], PickerOutcome::Cancel);
        }

        fn banner_to_picker_round_trip_outcome() {
            // Combined host-flow test: chrome wires a banner's `[Review]`
            // callback to open a picker. Drive the banner click, observe
            // Review fires, then dispatch a Pick click and observe Picked.
            use crate::widget::conflict_banner::{BANNER_HEIGHT, ConflictBanner};

            // 1. Banner click drives the host to open a picker.
            let review_fired: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
            let rf = review_fired.clone();
            let mut banner =
                ConflictBanner::new("Kanban move").on_review(move || *rf.borrow_mut() = true);
            banner.set_rect_for_test(Rect::new(0.0, 0.0, 600.0, BANNER_HEIGHT));
            banner.set_conflict_count(2);

            let banner_chip = banner.review_chip_rect().unwrap();

            dispatch(
                &mut banner,
                &cursor_moved(banner_chip.center().x, banner_chip.center().y),
            );
            dispatch(&mut banner, &mouse_press());
            assert!(*review_fired.borrow(), "banner [Review] must fire");

            // 2. Host opens the picker and the user picks alternative 0.
            let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
            let outcomes_clone = outcomes.clone();
            let kind = ConflictKind::PickOne {
                alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
            };
            let mut picker = ConflictPicker::new("Kanban move", kind)
                .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
            picker.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

            let btn = picker.pick_button_rect(0).unwrap();
            dispatch(&mut picker, &cursor_moved(btn.center().x, btn.center().y));
            dispatch(&mut picker, &mouse_press());
            assert_eq!(
                outcomes.borrow()[0],
                PickerOutcome::Picked {
                    alternative_index: 0
                }
            );
        }

        picker_pick_button_fires_with_correct_index();
        picker_keep_current_fires_keep_current();
        picker_esc_fires_cancel();
        picker_merged_pick_button_fires_picked_merged();
        picker_scrim_click_fires_cancel();
        banner_to_picker_round_trip_outcome();
    }
}

//! Small composition helpers shared across the settings panels.
//!
//! These build on `ui`'s widgets — they don't reimplement rendering, they
//! just bundle the recurring shapes (a titled card, a label/value row, a
//! status pill) so every panel reads consistently and the panels stay
//! short. Nothing here is settings-specific beyond the visual grammar.
//!
//! Grouped surfaces come from the shared [`ui::widget::Card`], so every card in
//! the OS reads identically and tracks the theme; the helpers here just bundle
//! the settings-specific row/section shapes on top of it.
//!
//! ## Spacing vocabulary
//!
//! Every gap here is one of three named steps, so a pane reads as groups
//! rather than as a wall of equally spaced lines (HIG §7a):
//!
//! - **caption gap** ([`spacing`]`().s1`) — a description hugs the label it
//!   explains;
//! - **[`ROW_GAP`]** — related rows inside one card (the scale's `s3`);
//! - **[`CARD_GAP`]** — one grouped surface to the next (the scale's `s6`,
//!   the "before the next group" step).
//!
//! Helpers run before a `Theme` exists (a pane composes its tree and the host
//! resolves the theme at layout time), so every measure here comes from the
//! toolkit's *default* spacing scale via [`spacing`]. A custom theme may
//! retune `s2`–`s4`; those retuned values reach the shared widgets' own
//! padding at layout time but not these composition gaps, which is a known
//! limit of theme-less composition (recorded for the toolkit's layout unit).

use std::cell::Cell;

use ui::icons::Icon;
use ui::tokens::{Spacing, SpacingStep};
pub use ui::widget::BadgeTone as StatusTone;
use ui::widget::{Badge, Column, Container, IconWidget, Row, Spacer, Text, Widget};

/// Type alias for the boxed-widget currency the `ui` tree speaks.
pub type El = Box<dyn Widget>;

/// The toolkit's default spacing scale (`s1` = 4, `s2` = 8, `s3` = 12,
/// `s4` = 16, `s6` = 24 …). Settings composes its tree before a `Theme` is
/// resolved, so this is the one scale every helper (and the sidebar's row
/// geometry) reads; keeping them on one source is what lets the pill, the
/// section headers and the shell's search field share edges.
///
/// Where a gap is a lone spacer rather than a column's rhythm, prefer
/// [`gap`]: it names a [`SpacingStep`] the toolkit resolves against the
/// *live* theme at layout time, so a retuned custom scale reaches it.
pub fn spacing() -> Spacing {
    Spacing::default()
}

/// Vertical gap (logical px) between stacked cards in a panel. The page
/// heading and this deliberate air do the grouping work, so card outlines do
/// not have to carry every level of hierarchy.
///
/// The scale's "before the next group" step (HIG §7b): a card is one grouped
/// surface, the next card is the next group, so the boundary between them is
/// `s6`. It is also the page's vertical gutter, so the first card sits the
/// same distance under the heading block as it does above its neighbour.
/// The earlier 20 px value was authored off the scale to sit between `s4`
/// and `s6`; a step that belongs to no relationship is a defect under the
/// one-rhythm rule, not a nuance.
pub const CARD_GAP: f32 = Spacing::BASE.s6;

/// Vertical gap (logical px) between rows inside a single card — the toolkit's
/// `s3` step. Smaller than [`CARD_GAP`] so rows feel related within a group
/// while still having visible separation.
pub const ROW_GAP: f32 = Spacing::BASE.s3;

/// One spacing step of empty space, resolved against the theme the host
/// hands the tree at layout time (so it follows `ui_scale` and a custom
/// theme's spacing preference — the one composition gap here that does).
/// Reads as a vertical gap in a `Column` and a horizontal one in a `Row`.
///
/// Stacks of cards and rows do **not** need this: an intrinsic-height
/// [`Column`] honours its `with_gap` between every child, and [`cards`] /
/// [`rows`] rely on exactly that. Reach for `gap` only where a single gap is
/// deliberately asymmetric (air above a lone caption, a gutter between an
/// icon and its label). A `Row` already separates its children by the theme
/// gap; a `gap` inside one adds to that rather than replacing it.
pub fn gap(step: SpacingStep) -> El {
    Box::new(Spacer::token(step))
}

/// Stack a list of cards with a deliberate [`CARD_GAP`] of air between each
/// one. This is the standard body for a panel.
pub fn cards(items: Vec<El>) -> El {
    Box::new(stack(items, CARD_GAP))
}

/// A vertical stack of pre-built rows — the standard body for a card. Rows are
/// separated by [`ROW_GAP`], the "related rows" step of the scale.
pub fn rows(items: Vec<El>) -> El {
    Box::new(stack(items, ROW_GAP))
}

/// An intrinsic-height column that owns one uniform `gap` between its
/// children and no padding of its own. The shared building block behind
/// [`cards`] and [`rows`]: the surface (card or page) supplies the inset, the
/// stack supplies only the rhythm.
fn stack(items: Vec<El>, gap: f32) -> Column {
    let mut col = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(gap);
    for item in items {
        col = col.push(item);
    }
    col
}

/// The unboxed heading at the start of every settings destination.
///
/// A category is a real page, not merely the selected row in a navigation
/// list. Keeping its identity in plain type (rather than another raised card)
/// establishes where the person is while avoiding the old card-within-page
/// treatment. The small "Settings" eyebrow also makes the shared shell's
/// arrow unambiguous on a narrow content screen: it returns to the Settings
/// list, not to a previous app/window.
pub fn page_heading(title: &str, subtitle: &str) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing().s1)
            .push(Box::new(
                Text::new(&crate::i18n::t("settings-page-eyebrow"))
                    .with_scaled_size(12.0)
                    .dimmed(),
            ))
            .push(Box::new(Text::new(title).with_scaled_size(24.0)))
            .push(Box::new(
                Text::new(subtitle).with_scaled_size(13.0).dimmed(),
            )),
    )
}

/// A titled "card": a heading line followed by a bordered, padded surface
/// holding `body`. The Tilo grouping primitive — every panel is a stack of
/// these. Delegates to the shared [`ui::widget::Card`] so every card in the OS
/// reads identically and tracks the theme (radius/border/fill all from
/// `Theme`); the heading sits inside the surface, above the body.
pub fn card(title: &str, body: El) -> El {
    Box::new(ui::widget::Card::titled(title, body))
}

/// A card whose title row also carries a trailing element (e.g. an "Add"
/// button or a count badge) on the same line as the title. Delegates to the
/// shared [`ui::widget::Card`].
pub fn card_with_action(title: &str, action: El, body: El) -> El {
    Box::new(ui::widget::Card::titled_with_action(title, action, body))
}

/// A label-on-the-left, control-on-the-right row — the workhorse settings
/// line. The `Spacer` pushes the control to the trailing edge, and the `Row`
/// centres label and control on one line whatever the control's height.
pub fn setting_row(label: &str, control: El) -> El {
    Box::new(
        Row::new()
            .push(Box::new(Text::new(label).with_scaled_size(13.0)))
            .push(Box::new(Spacer::new()))
            .push(control),
    )
}

/// A label/value row where the value is plain read-only text.
pub fn value_row(label: &str, value: &str) -> El {
    setting_row(label, Box::new(Text::new(value).with_size(13.0)))
}

/// A label with a short helper line beneath it, paired with a trailing
/// control. Used where a setting needs a one-line explanation.
///
/// The label and control sit inline on the top line, with the description on
/// its own line beneath. A single leading `Text` keeps the control on the
/// label's line at the right edge — a multi-line block on the left would seize
/// the row and wrap the control below it, dropping it out of alignment.
///
/// The description hugs its row by the caption gap (`s1`), so it reads as
/// belonging to the label above it rather than floating halfway to the next
/// row, which sits a full [`ROW_GAP`] further down.
pub fn setting_row_described(label: &str, description: &str, control: El) -> El {
    let top = Row::new()
        .push(Box::new(Text::new(label).with_scaled_size(13.0)))
        .push(Box::new(Spacer::new()))
        .push(control);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing().s1)
            .push(Box::new(top))
            .push(Box::new(
                Text::new(description).with_scaled_size(12.0).dimmed(),
            )),
    )
}

/// A dim helper-text line, for captions and "best-effort" notes.
pub fn caption(text: &str) -> El {
    Box::new(Text::new(text).with_scaled_size(12.0).dimmed())
}

/// Default width (logical px) a compact right-aligned control (dropdown,
/// segmented picker) is pinned to, so a short-option control reads as a
/// trailing chooser rather than a full-width form field. Wide enough for the
/// longest option label the settings dropdowns offer.
pub const COMPACT_CONTROL_W: f32 = 220.0;

thread_local! {
    static COMPOSE_TEXT_GROWTH: Cell<f32> = const { Cell::new(1.0) };
}

/// Record the live theme's [`ui::Theme::text_growth`] before a pane
/// composes. Compact choosers are authored at 1×; large type has to grow
/// that pin or option labels clip inside a 220 px box.
pub fn set_compose_text_growth(growth: f32) {
    let growth = if growth.is_finite() {
        growth.clamp(1.0, 4.0)
    } else {
        1.0
    };
    COMPOSE_TEXT_GROWTH.set(growth);
}

fn compose_text_growth() -> f32 {
    COMPOSE_TEXT_GROWTH.get()
}

/// [`COMPACT_CONTROL_W`] grown for the current compose-time text scale.
pub fn compact_chooser_width() -> f32 {
    COMPACT_CONTROL_W * compose_text_growth()
}

/// Pin a trailing control to a fixed width so it aligns to the row's right
/// edge instead of stretching across the whole card. `ui`'s `Dropdown` fills
/// its parent (`percent(1.0)` width); wrapping it here gives every settings
/// chooser the same compact, right-aligned footprint — the GNOME/Adwaita
/// look — rather than a form field that spans the card.
pub fn compact_control(control: El) -> El {
    compact_control_w(compact_chooser_width(), control)
}

/// [`compact_control`] with an explicit width, for controls that need more
/// (long device names) or less room.
pub fn compact_control_w(width: f32, control: El) -> El {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(width)
            .with_child(control),
    )
}

/// A settings row whose trailing control is a compact, right-aligned chooser
/// (see [`compact_control`]). Use for dropdown/segmented settings so the
/// label stays left and the control sits at the row's right edge on one line,
/// rather than the control stretching full-width and pushing the label above
/// it.
pub fn choice_row(label: &str, control: El) -> El {
    Box::new(
        Row::new()
            .with_gap(spacing().s1)
            .push(Box::new(Text::new(label).with_scaled_size(13.0)))
            .push(Box::new(Spacer::new()))
            .push(compact_control(control)),
    )
}

/// [`choice_row`] with a helper line under the label.
pub fn choice_row_described(label: &str, description: &str, control: El) -> El {
    setting_row_described(label, description, compact_control(control))
}

/// The narrow-screen form of [`choice_row`]: the label on its own line and
/// the chooser beneath it, spanning the card. A phone-width card cannot seat
/// a label beside a compact chooser, and letting each `Row` wrap on its own
/// leaves one short-labelled row inline while its neighbours stack — so a
/// pane switches a whole card to this form at once (see
/// [`choice_rows_stack_below`]), and every row in the card reads the same
/// way. Label and control are one caption gap (`s1`) apart, the same rhythm
/// a described row uses for text that belongs to its label.
///
/// This is the app-side composition; the toolkit's `Field` does not yet
/// offer a trailing-control placement that switches per section (recorded
/// for the composition unit), which is where this policy ultimately belongs.
pub fn choice_row_stacked(label: &str, control: El) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing().s1)
            .push(Box::new(Text::new(label).with_scaled_size(13.0)))
            .push(control),
    )
}

/// The card width (logical px) below which a pane composes its choice rows
/// with [`choice_row_stacked`] instead of [`choice_row`]: the compact
/// chooser, one `s3` gap, room for a typical settings label at its
/// [`choice_row`] type size, and the card's own `s3` inset on both sides.
/// Above it every row's label and chooser fit on one line; below it none
/// are asked to.
pub fn choice_rows_stack_below() -> f32 {
    /// Room reserved for the label beside a compact chooser: the longest
    /// label the Formats card uses after the row's own text measurement, so a
    /// card stacks only when the chooser genuinely no
    /// longer fits beside any of its labels — a 368 px card (a 440 px window)
    /// stays inline, while the smaller phone card stacks.
    const CHOICE_LABEL_RESERVE: f32 = 100.0;
    let sp = spacing();
    compact_chooser_width() + sp.s3 + CHOICE_LABEL_RESERVE + 2.0 * sp.s3
}

/// A small rounded status chip (e.g. "Connected", "Paired", "Offline").
/// Backed by the toolkit's [`Badge::pill`] so the tint carries the meaning
/// and the shape is consistent with the rest of the system.
pub fn status_pill(label: &str, tint: vello::peniko::Color) -> El {
    Box::new(Badge::pill(label, tint))
}

/// A status pill that resolves its colour from the active theme.
pub fn semantic_status_pill(label: &str, tone: StatusTone) -> El {
    Box::new(Badge::semantic_pill(label, tone))
}

/// Glyph size (logical px) of the [`hero`] identity icon.
pub const HERO_ICON_SIZE: f32 = 34.0;

/// A prominent identity block for a pane's first card: a large icon with a
/// bold title and a dim one-line subtitle stacked *beside* it. Used where a
/// pane benefits from a hero (About's device identity, Power's battery).
///
/// The text block sits in a flex-growing container so it takes the width the
/// icon leaves over instead of claiming the whole row and wrapping underneath
/// the glyph; title and subtitle are one caption gap (`s1`) apart. The icon
/// keeps the theme's primary ink rather than a colour frozen from one palette,
/// so the block reads correctly in light and high-contrast as well.
pub fn hero(icon: Icon, title: &str, subtitle: &str) -> El {
    let text_block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(spacing().s1)
        .push(Box::new(Text::new(title).with_scaled_size(20.0)))
        .push(Box::new(
            Text::new(subtitle).with_scaled_size(12.0).dimmed(),
        ));
    let row = Row::new()
        .no_wrap()
        .push(Box::new(IconWidget::new(icon, HERO_ICON_SIZE)))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(text_block)),
        ));
    Box::new(ui::widget::Card::new(Box::new(row)))
}

/// Horizontal spacing inside a control row.
pub fn hgap(px: f32) -> El {
    Box::new(Container::new().with_no_padding().with_size(px, 1.0))
}

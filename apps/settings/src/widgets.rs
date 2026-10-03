//! Small composition helpers shared across the settings panels.
//!
//! These build on `ui`'s widgets — they don't reimplement rendering, they
//! just bundle the recurring shapes (a titled section, a label/value row, a
//! status badge) so every panel reads consistently and the panels stay
//! short. Nothing here is settings-specific beyond the visual grammar.
//!
//! A pane is a stack of sections on the window ground, separated by space
//! and type weight rather than boxes (design language, "Edges"): a section
//! is a [`ui::widget::Section`] with a `Heading` title over its rows.
//!
//! ## Spacing vocabulary
//!
//! Every gap here is one of three named steps, so a pane reads as groups
//! rather than as a wall of equally spaced lines:
//!
//! - **caption gap** ([`spacing`]`().s1`) — a description hugs the label it
//!   explains;
//! - **[`ROW_GAP`]** — related rows inside one section (the scale's `s3`);
//! - **[`SECTION_GAP`]** — one section to the next (the scale's `s6`).
//!
//! Helpers run before a `Theme` exists (a pane composes its tree and the host
//! resolves the theme at layout time), so every measure here comes from the
//! toolkit's *default* spacing scale via [`spacing`].

use std::cell::Cell;

use ui::icons::Icon;
use ui::tokens::{Metrics, Spacing, SpacingStep, TextRole};
pub use ui::widget::Tone as StatusTone;
use ui::widget::{
    Badge, ChipStyle, Column, Container, IconWidget, Row, Section, Spacer, Text, Widget,
};

/// Type alias for the boxed-widget currency the `ui` tree speaks.
pub type El = Box<dyn Widget>;

/// The toolkit's default spacing scale (`s1` = 4, `s2` = 8, `s3` = 12,
/// `s4` = 16, `s6` = 24 …). Settings composes its tree before a `Theme` is
/// resolved, so this is the one scale every helper reads.
///
/// Where a gap is a lone spacer rather than a column's rhythm, prefer
/// [`gap`]: it names a [`SpacingStep`] the toolkit resolves against the
/// *live* theme at layout time, so a retuned custom scale reaches it.
pub fn spacing() -> Spacing {
    Spacing::default()
}

/// Vertical gap (logical px) between stacked sections in a pane: the scale's
/// "before the next group" step.
pub const SECTION_GAP: f32 = Spacing::BASE.s6;

/// Vertical gap (logical px) between rows inside a single section — the
/// toolkit's `s3` step. Smaller than [`SECTION_GAP`] so rows feel related
/// within a group while still having visible separation.
pub const ROW_GAP: f32 = Spacing::BASE.s3;

/// One spacing step of empty space, resolved against the theme the host
/// hands the tree at layout time (so it follows `ui_scale` and a custom
/// theme's spacing preference).
pub fn gap(step: SpacingStep) -> El {
    Box::new(Spacer::token(step))
}

/// Stack a list of sections with a deliberate [`SECTION_GAP`] of air between
/// each one. This is the standard body for a pane.
pub fn cards(items: Vec<El>) -> El {
    Box::new(stack(items, SECTION_GAP))
}

/// A vertical stack of pre-built rows — the standard body for a section.
/// Rows are separated by [`ROW_GAP`], the "related rows" step of the scale.
pub fn rows(items: Vec<El>) -> El {
    Box::new(stack(items, ROW_GAP))
}

/// An intrinsic-height column that owns one uniform `gap` between its
/// children and no padding of its own. The shared building block behind
/// [`cards`] and [`rows`]: the pane supplies the inset, the stack supplies
/// only the rhythm.
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

/// A titled section: a `Heading` line followed by `body`, sitting on the
/// ground. Groups are separated by space, never boxed.
pub fn card(title: &str, body: El) -> El {
    Box::new(Section::titled(title, vec![body]))
}

/// A section whose title row also carries a trailing element (a master
/// switch, a status badge or an "Add" button) on the same line as the title.
pub fn card_with_action(title: &str, action: El, body: El) -> El {
    let header = Row::new()
        .no_wrap()
        .with_align_items(taffy::AlignItems::Center)
        .push(Box::new(
            Text::new(title)
                .with_role(TextRole::Heading)
                .with_heading_level(2),
        ))
        .push(Box::new(Spacer::new()))
        .push(action);
    Box::new(Section::new(vec![Box::new(header), body]))
}

/// A label-on-the-left, control-on-the-right row — the workhorse settings
/// line. The `Spacer` pushes the control to the trailing edge, and the `Row`
/// centres label and control on one line whatever the control's height.
pub fn setting_row(label: &str, control: El) -> El {
    Box::new(
        Row::new()
            .push(Box::new(Text::new(label)))
            .push(Box::new(Spacer::new()))
            .push(control),
    )
}

/// A label/value row where the value is plain read-only text.
pub fn value_row(label: &str, value: &str) -> El {
    setting_row(label, Box::new(Text::new(value).numeric().dimmed()))
}

/// A label with a short helper line beneath it, paired with a trailing
/// control. Used where a setting needs a one-line explanation.
///
/// The label and control sit inline on the top line, with the description on
/// its own line beneath. The description hugs its row by the caption gap
/// (`s1`), so it reads as belonging to the label above it rather than
/// floating halfway to the next row.
pub fn setting_row_described(label: &str, description: &str, control: El) -> El {
    let top = Row::new()
        .push(Box::new(Text::new(label)))
        .push(Box::new(Spacer::new()))
        .push(control);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing().s1)
            .push(Box::new(top))
            .push(caption(description)),
    )
}

/// A dim helper-text line, for captions and "best-effort" notes.
pub fn caption(text: &str) -> El {
    Box::new(Text::new(text).with_role(TextRole::Caption).dimmed())
}

thread_local! {
    static COMPOSE_TEXT_GROWTH: Cell<f32> = const { Cell::new(1.0) };
}

/// Record the live theme's [`ui::Theme::text_growth`] before a pane
/// composes. Compact choosers are authored at 1×; large type has to grow
/// that pin or option labels clip inside the chooser.
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

/// The width of a compact right-aligned chooser (dropdown, segmented
/// picker): the toolkit's utility-panel width grown for the current
/// compose-time text scale, so a short-option control reads as a trailing
/// chooser rather than a full-width form field.
pub fn compact_chooser_width() -> f32 {
    Metrics::BASE.panel_width_utility * compose_text_growth()
}

/// Pin a trailing control to the compact chooser width so it aligns to the
/// row's right edge instead of stretching across the whole section. `ui`'s
/// `Dropdown` fills its parent; wrapping it here gives every settings
/// chooser the same compact, right-aligned footprint.
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
/// label stays left and the control sits at the row's right edge on one line.
pub fn choice_row(label: &str, control: El) -> El {
    Box::new(
        Row::new()
            .with_gap(spacing().s1)
            .push(Box::new(Text::new(label)))
            .push(Box::new(Spacer::new()))
            .push(compact_control(control)),
    )
}

/// [`choice_row`] with a helper line under the label.
pub fn choice_row_described(label: &str, description: &str, control: El) -> El {
    setting_row_described(label, description, compact_control(control))
}

/// The narrow-screen form of [`choice_row`]: the label on its own line and
/// the chooser beneath it, spanning the section. A phone-width section
/// cannot seat a label beside a compact chooser, so a pane switches a whole
/// section to this form at once (see [`choice_rows_stack_below`]).
pub fn choice_row_stacked(label: &str, control: El) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing().s1)
            .push(Box::new(Text::new(label)))
            .push(control),
    )
}

/// The section width (logical px) below which a pane composes its choice
/// rows with [`choice_row_stacked`] instead of [`choice_row`]: the compact
/// chooser, one `s3` gap and room for a typical settings label.
pub fn choice_rows_stack_below() -> f32 {
    compact_chooser_width() + spacing().s3 + Metrics::BASE.sidebar_min_width / 2.0
}

/// A small status badge (e.g. "Connected", "Paired", "Offline"). The tone
/// carries the meaning; neutral is quiet.
pub fn status_pill(label: &str, tone: StatusTone) -> El {
    Box::new(Badge::text(label).style(ChipStyle::Tinted(tone)))
}

/// A status badge that resolves its colour from the active theme.
pub fn semantic_status_pill(label: &str, tone: StatusTone) -> El {
    status_pill(label, tone)
}

/// A prominent identity block for a pane's first section: a large icon with
/// a `Title` and a `Caption` stacked *beside* it. Used where a pane benefits
/// from a hero (About's device identity, Power's battery).
///
/// The text block sits in a flex-growing container so it takes the width the
/// icon leaves over instead of claiming the whole row and wrapping underneath
/// the glyph.
pub fn hero(icon: Icon, title: &str, subtitle: &str) -> El {
    let text_block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(spacing().s1)
        .push(Box::new(Text::new(title).with_role(TextRole::Title)))
        .push(caption(subtitle));
    let row = Row::new()
        .no_wrap()
        .push(Box::new(IconWidget::new(icon, Metrics::BASE.icon_lg)))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(text_block)),
        ));
    Box::new(row)
}

/// Horizontal spacing inside a control row.
pub fn hgap(px: f32) -> El {
    Box::new(Container::new().with_no_padding().with_size(px, 1.0))
}

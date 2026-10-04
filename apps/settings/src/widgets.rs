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
//! Every gap here is a relationship ([`Rhythm`]), resolved against the live
//! theme at layout time, so a pane reads as groups rather than as a wall of
//! equally spaced lines:
//!
//! - **tight** — a description hugs the label it explains;
//! - **sibling** — related rows inside one section;
//! - **group** — one section to the next.

use std::cell::Cell;

use ui::icons::Icon;
use ui::tokens::{Metrics, Rhythm, Spacing, SpacingStep, TextRole};
pub use ui::widget::Tone as StatusTone;
use ui::widget::{
    Badge, Button, ChipStyle, Column, Container, ControlSize, IconSize, IconWidget, Row, Section,
    Spacer, Text, Widget,
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

/// One spacing step of empty space, resolved against the theme the host
/// hands the tree at layout time (so it follows `ui_scale` and a custom
/// theme's spacing preference).
pub fn gap(step: SpacingStep) -> El {
    Box::new(Spacer::token(step))
}

/// Stack a list of sections a group apart. This is the standard body for a
/// pane.
pub fn cards(items: Vec<El>) -> El {
    Box::new(stack(items, Rhythm::Group))
}

/// A vertical stack of pre-built rows — the standard body for a section.
/// Rows are siblings: closer than sections, still visibly apart.
pub fn rows(items: Vec<El>) -> El {
    Box::new(stack(items, Rhythm::Sibling))
}

/// An intrinsic-height column that owns one uniform `gap` between its
/// children and no padding of its own. The shared building block behind
/// [`cards`] and [`rows`]: the pane supplies the inset, the stack supplies
/// only the rhythm.
fn stack(items: Vec<El>, gap: Rhythm) -> Column {
    let mut col = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(gap);
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

/// The label of a row: it takes the width its control leaves over and wraps
/// there, so a long label never pushes the control off the row.
fn row_label(label: &str) -> El {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_flex_grow()
            .with_child(Box::new(Text::new(label))),
    )
}

/// A label with a description hugging it, as one block of a row. The
/// description sits one caption gap under the label, so it reads as part of
/// its row rather than floating between two.
fn row_label_described(label: &str, description: &str) -> El {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_flex_grow()
            .with_child(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_rhythm(Rhythm::Tight)
                    .push(Box::new(Text::new(label)))
                    .push(caption(description)),
            )),
    )
}

/// One line of a settings list: `lead` takes the room, `control` keeps its
/// own size at the trailing edge and is centred on the line.
fn trailing_row(lead: El, control: El) -> El {
    Box::new(
        Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .push(lead)
            .push(control),
    )
}

/// A label-on-the-left, control-on-the-right row — the workhorse settings
/// line. The label grows to the control and the control stays at the
/// trailing edge, centred on the line whatever its height.
pub fn setting_row(label: &str, control: El) -> El {
    trailing_row(row_label(label), control)
}

/// A label/value row where the value is plain read-only text.
pub fn value_row(label: &str, value: &str) -> El {
    setting_row(label, Box::new(Text::new(value).numeric().dimmed()))
}

/// A label with a short helper line beneath it, paired with a trailing
/// control. The helper line is part of the label block and the control is
/// centred on the pair, so the description groups with its own row.
pub fn setting_row_described(label: &str, description: &str, control: El) -> El {
    trailing_row(row_label_described(label, description), control)
}

/// A row for a continuous control (a slider) that wants the whole measure:
/// the label on its own line and the control spanning the section.
pub fn slider_row(label: &str, control: El) -> El {
    choice_row_stacked(label, control)
}

/// A row for a text field: the label on its own line and the field beneath
/// it spanning the section, as every form in Settings lays its fields out.
pub fn field_row(label: &str, control: El) -> El {
    choice_row_stacked(label, control)
}

/// [`slider_row`] with a helper line under the label.
pub fn slider_row_described(label: &str, description: &str, control: El) -> El {
    stacked_block(
        vec![
            Box::new(Text::new(label)) as El,
            caption(description),
            control,
        ],
        Rhythm::Tight,
    )
}

/// One entry of a list of things (a printer, a user, a device): its name with
/// any status marks on the top line, what is worth knowing about it hugging
/// the name below, and the actions that apply to it under the text. Every
/// such list reads the same way, whether or not an entry has actions.
pub fn item_row(title: &str, status: Vec<El>, details: &[String], actions: Vec<Button>) -> El {
    let mut top = Row::new()
        .no_wrap()
        .with_align_items(taffy::AlignItems::Center)
        .push(row_label(title));
    for mark in status {
        top = top.push(mark);
    }
    let mut block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Tight)
        .push(Box::new(top));
    for detail in details {
        block = block.push(caption(detail));
    }
    if actions.is_empty() {
        return Box::new(block);
    }
    let mut buttons = Row::new().with_rhythm(Rhythm::Related);
    for action in actions {
        buttons = buttons.push(Box::new(action.control_size(ControlSize::Small)));
    }
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(block))
            .push(Box::new(buttons)),
    )
}

/// A row for a thing with an icon (a network, a device): the icon, its name
/// with a line of detail hugging it, and a trailing control. The control
/// keeps its place however narrow the window gets.
pub fn icon_row(icon: Icon, title: &str, detail: &str, control: El) -> El {
    let text = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Tight)
        .push(Box::new(Text::new(title)))
        .push(caption(detail));
    let lead = Row::new()
        .no_wrap()
        .with_align_items(taffy::AlignItems::Center)
        .with_rhythm(Rhythm::Sibling)
        .push(Box::new(IconWidget::new(icon)))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(text)),
        ));
    trailing_row(
        Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(lead)),
        ),
        control,
    )
}

/// A dim helper-text line, for captions and "best-effort" notes.
pub fn caption(text: &str) -> El {
    Box::new(Text::new(text).with_role(TextRole::Caption).dimmed())
}

thread_local! {
    static COMPOSE_TEXT_GROWTH: Cell<f32> = const { Cell::new(1.0) };
    static COMPOSE_WIDTH: Cell<Option<f32>> = const { Cell::new(None) };
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

/// Record the width (logical px) the page gives the pane being composed, or
/// `None` before the page has been laid out. Every choice row of the pane
/// then settles on one form together ([`choices_stack`]).
pub fn set_compose_width(width: Option<f32>) {
    COMPOSE_WIDTH.set(width.filter(|w| w.is_finite()));
}

/// Whether the pane composes its choice rows with the chooser beneath the
/// label. One decision for the whole page, so a narrow window never shows
/// some rows inline and their neighbours stacked.
pub fn choices_stack() -> bool {
    COMPOSE_WIDTH
        .get()
        .is_some_and(|width| width < choice_rows_stack_below())
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
/// (see [`compact_control`]). The label stays left and the chooser sits at
/// the row's right edge on one line; in a narrow page the whole pane's
/// choosers go beneath their labels instead ([`choices_stack`]).
pub fn choice_row(label: &str, control: El) -> El {
    if choices_stack() {
        return choice_row_stacked(label, control);
    }
    trailing_row(row_label(label), compact_control(control))
}

/// [`choice_row`] with a helper line under the label. The description spans
/// the whole row, hugging it, so a long one is not squeezed into the label's
/// column beside the chooser.
pub fn choice_row_described(label: &str, description: &str, control: El) -> El {
    if choices_stack() {
        return stacked_block(
            vec![
                Box::new(Text::new(label)) as El,
                caption(description),
                control,
            ],
            Rhythm::Tight,
        );
    }
    stacked_block(
        vec![
            trailing_row(row_label(label), compact_control(control)),
            caption(description),
        ],
        Rhythm::Tight,
    )
}

fn stacked_block(items: Vec<El>, gap: Rhythm) -> El {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(gap);
    for item in items {
        column = column.push(item);
    }
    Box::new(column)
}

/// The narrow form of [`choice_row`]: the label on its own line and the
/// chooser beneath it, spanning the section.
pub fn choice_row_stacked(label: &str, control: El) -> El {
    stacked_block(
        vec![Box::new(Text::new(label)) as El, control],
        Rhythm::Tight,
    )
}

/// The section width (logical px) below which a pane composes its choice
/// rows with the chooser beneath the label: the compact chooser, one `s3`
/// gap and room for a typical settings label.
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

/// A prominent identity block for a pane's first section: a large icon
/// beside a `Title` (About's device name).
pub fn hero(icon: Icon, title: &str) -> El {
    let row = Row::new()
        .no_wrap()
        .with_align_items(taffy::AlignItems::Center)
        .with_rhythm(Rhythm::Sibling)
        .push(Box::new(IconWidget::new(icon).sized(IconSize::Large)))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(Text::new(title).with_role(TextRole::Title))),
        ));
    Box::new(row)
}

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
use ui::tokens::{Rhythm, Spacing, SpacingStep, TextRole};
pub use ui::widget::Tone as StatusTone;
use ui::widget::{
    Badge, ChipStyle, Column, Container, Field, FormSection, IconSize, IconWidget, Row, Section,
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
/// It is a form section, so its labelled rows ([`setting_row`],
/// [`choice_row`] and the like) share one label column and decide together,
/// at layout and from the width they really get, whether their controls
/// stand beside their labels or beneath them.
pub fn rows(items: Vec<El>) -> El {
    // A settings list: every labelled row a grouped row high, its control
    // on the trailing edge, a separator between rows.
    Box::new(
        items
            .into_iter()
            .fold(FormSection::untitled().grouped(), FormSection::content),
    )
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

/// A labelled row of a settings section: a [`Field`] whose control keeps its
/// own width at the trailing edge beside the label, and takes the full width
/// beneath it once the section is too narrow for the pair.
fn labelled_row(label: &str, description: Option<&str>, control: El) -> El {
    let field = Field::custom(label, control).with_trailing_control();
    Box::new(match description {
        Some(description) => field.with_help(description),
        None => field,
    })
}

/// A label-on-the-left, control-on-the-right row — the workhorse settings
/// line. The label grows to the control and the control stays at the
/// trailing edge, centred on the line whatever its height.
pub fn setting_row(label: &str, control: El) -> El {
    labelled_row(label, None, control)
}

/// A label/value row where the value is plain read-only text.
pub fn value_row(label: &str, value: &str) -> El {
    setting_row(label, Box::new(Text::new(value).numeric().dimmed()))
}

/// A label with a short helper line beneath it, paired with a trailing
/// control. The helper line is part of the label block and the control is
/// centred on the pair, so the description groups with its own row.
pub fn setting_row_described(label: &str, description: &str, control: El) -> El {
    labelled_row(label, Some(description), control)
}

/// A row for a text field: the label on its own line and the field beneath
/// it spanning the section, as every form in Settings lays its fields out.
pub fn field_row(label: &str, control: El) -> El {
    choice_row_stacked(label, control)
}

/// A dim helper-text line, for captions and "best-effort" notes.
pub fn caption(text: &str) -> El {
    Box::new(Text::new(text).with_role(TextRole::Caption).dimmed())
}

/// A reading the pane could not make, said as a failure. `message` is the
/// whole sentence ("Can't read network status: …"); it replaces the value
/// the pane would otherwise show, so a failed read never looks like a state
/// of the device ("Not connected", "Off", an empty list). Screen readers
/// announce it when it appears.
pub fn error_caption(message: &str) -> El {
    Box::new(
        Text::new(message)
            .with_role(TextRole::Caption)
            .with_weight(ui::text_render::TextWeight::Semibold)
            .with_live(ui::a11y::Politeness::Polite),
    )
}

/// A label whose value could not be read: the label keeps its place in the
/// card and the failure stands where the value would be.
pub fn error_row(label: &str, message: &str) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing().s1)
            .push(setting_row(
                label,
                Box::new(
                    Text::new(&crate::i18n::t("settings-read-error"))
                        .with_role(TextRole::Label)
                        .with_weight(ui::text_render::TextWeight::Semibold),
                ),
            ))
            .push(error_caption(message)),
    )
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

/// A settings row whose trailing control is a compact chooser (dropdown,
/// segmented picker): at least the compact chooser width at the row's right
/// edge beside the label. When the section is too narrow for its labels and
/// choosers, every row of it puts its chooser beneath the label, full width.
pub fn choice_row(label: &str, control: El) -> El {
    labelled_row(label, None, control)
}

/// [`choice_row`] with a helper line under the label.
pub fn choice_row_described(label: &str, description: &str, control: El) -> El {
    labelled_row(label, Some(description), control)
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

/// Every name and value a screen reader would announce in `widget`'s tree,
/// in tree order: what a pane says, for tests to read back.
#[cfg(test)]
pub fn announced_text(widget: &dyn Widget) -> Vec<String> {
    fn walk(widget: &dyn Widget, out: &mut Vec<String>) {
        if let Some(node) = widget.accessibility() {
            out.extend(node.name);
            out.extend(node.value);
        }
        for child in widget.children() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(widget, &mut out);
    out
}

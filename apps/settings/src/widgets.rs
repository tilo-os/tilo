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
    Badge, Button, ChipStyle, Column, Container, ControlSize, Field, Form, FormSection, IconSize,
    IconWidget, LabelPlacement, Row, Spacer, Text, Widget,
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

/// One entry of a settings section. A section's layout (label column, row
/// height, dividers, responsive stacking) belongs to the toolkit's
/// [`FormSection`], which only understands rows; so a pane can only hand a
/// section the things a section knows how to lay out, never a bare widget:
///
/// - a labelled row ([`setting_row`], [`choice_row`], [`value_row`], ...);
/// - a free row ([`row`]): a line of buttons, a list entry, a status line;
/// - a block ([`block`]): content that is not a row (a chart, a preview);
/// - a [`note`]: the sentence under the group that explains it.
///
/// `row` and `block` are the explicit, greppable escape hatches.
pub struct Item(ItemKind);

enum ItemKind {
    Field(Field),
    Row(El),
    Block(El),
    Note(String),
}

/// The entries of one section: what [`rows`] collects.
pub struct Body(Vec<Item>);

impl Body {
    #[must_use]
    pub fn push(mut self, item: Item) -> Self {
        self.0.push(item);
        self
    }
}

impl From<Vec<Item>> for Body {
    fn from(items: Vec<Item>) -> Self {
        Self(items)
    }
}

/// One group of a settings page: an optional heading (with an optional
/// action on its trailing edge) over its entries. Built by [`card`],
/// [`card_with_action`] and [`block_section`], assembled into a page by
/// [`cards`].
pub struct SettingsSection {
    heading: Heading,
    body: Vec<Item>,
}

enum Heading {
    None,
    Title(String),
    Action(String, El),
}

impl SettingsSection {
    fn into_form_section(self) -> FormSection {
        let mut section = match self.heading {
            Heading::None => FormSection::untitled(),
            Heading::Title(title) => FormSection::new(title),
            Heading::Action(title, action) => FormSection::with_action(title, action),
        }
        .grouped();
        let mut body = self.body.into_iter().peekable();
        // An opening note reads straight under the heading, before the rows.
        let mut intro = Vec::new();
        while let Some(Item(ItemKind::Note(_))) = body.peek() {
            if let Some(Item(ItemKind::Note(text))) = body.next() {
                intro.push(text);
            }
        }
        if !intro.is_empty() {
            section = section.content(notes_block(&intro));
        }
        // Every other note stands under the group, after its last row.
        let mut notes = Vec::new();
        for item in body {
            match item.0 {
                ItemKind::Note(text) => notes.push(text),
                ItemKind::Field(field) => section = section.field(field),
                ItemKind::Row(control) => section = section.control_row(padded(control)),
                ItemKind::Block(content) => section = section.content(padded(content)),
            }
        }
        if !notes.is_empty() {
            section = section.content(notes_block(&notes));
        }
        section
    }
}

/// A block's own vertical breathing room inside a grouped (zero-gap) list.
fn padded(content: El) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_vertical_padding(spacing().s2)
            .with_intrinsic_height()
            .push(content),
    )
}

fn notes_block(notes: &[String]) -> El {
    let mut column = Column::new()
        .with_no_padding()
        .with_vertical_padding(spacing().s2)
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Tight);
    for note in notes {
        column = column.push(caption(note));
    }
    Box::new(column)
}

/// The whole page: sections one group apart in a single [`Form`], so every
/// section shares one label column and one beside/stacked decision, and the
/// frame (readable width, scrolling, gutters) is the shell's.
pub fn cards(sections: Vec<SettingsSection>) -> El {
    Box::new(sections.into_iter().fold(
        // Sections sit one group step apart, the pane rhythm.
        Form::new().with_section_spacing(Some(spacing().rhythm.group)),
        |form, section| form.section(section.into_form_section()),
    ))
}

/// The entries of a section, in order.
pub fn rows(items: Vec<Item>) -> Body {
    Body(items)
}

/// A free row of a section: it spans the label and control columns and has
/// the height and dividers of the rows around it (a line of buttons, a
/// list entry, a status line).
pub fn row(content: El) -> Item {
    Item(ItemKind::Row(content))
}

/// A block of a section that is not a row (a chart, a preview, a table):
/// full width, spaced from the rows around it.
pub fn block(content: El) -> Item {
    Item(ItemKind::Block(content))
}

/// The sentence under a section that explains it: dim, wrapped, after the
/// last row (or before the first when it opens the section).
pub fn note(text: &str) -> Item {
    Item(ItemKind::Note(text.to_owned()))
}

/// A titled section: a `Heading` line over `body`, sitting on the ground.
/// Groups are separated by space, never boxed.
pub fn card(title: &str, body: Body) -> SettingsSection {
    SettingsSection {
        heading: Heading::Title(title.to_owned()),
        body: body.0,
    }
}

/// A section whose title row also carries a trailing element (a master
/// switch, a status badge or an "Add" button) on the same line as the title.
pub fn card_with_action(title: &str, action: El, body: Body) -> SettingsSection {
    SettingsSection {
        heading: Heading::Action(title.to_owned(), action),
        body: body.0,
    }
}

/// A section holding one free-standing block (a pane's identity header, a
/// preview), with no heading.
pub fn block_section(content: El) -> SettingsSection {
    untitled(rows(vec![block(content)]))
}

/// A section without a heading: the opening group of a pane whose title
/// already says what it is.
pub fn untitled(body: Body) -> SettingsSection {
    SettingsSection {
        heading: Heading::None,
        body: body.0,
    }
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

/// One line of a settings list: `lead` takes the room, `control` keeps its
/// own size at the trailing edge and is centred on the line, which is at
/// least a control high so every row of a list keeps one rhythm.
fn trailing_row(lead: El, control: El) -> El {
    Box::new(
        Row::new()
            .no_wrap()
            .with_min_height(ControlSize::Regular)
            .with_align_items(taffy::AlignItems::Center)
            .push(lead)
            .push(control),
    )
}

/// A labelled row of a settings section: a [`Field`] whose control keeps its
/// own width at the trailing edge beside the label, and takes the full width
/// beneath it once the section is too narrow for the pair.
fn labelled_row(label: &str, description: Option<&str>, control: El) -> Item {
    let field = Field::custom(label, control).with_trailing_control();
    Item(ItemKind::Field(match description {
        Some(description) => field.with_help(description),
        None => field,
    }))
}

/// A label-on-the-left, control-on-the-right row — the workhorse settings
/// line. The label grows to the control and the control stays at the
/// trailing edge, centred on the line whatever its height.
pub fn setting_row(label: &str, control: El) -> Item {
    labelled_row(label, None, control)
}

/// A label/value row where the value is plain read-only text.
pub fn value_row(label: &str, value: &str) -> Item {
    // A reading takes the room the label leaves, flush with the trailing
    // edge, instead of the narrow column a chooser gets, so a long value
    // wraps only when the row is really full.
    Item(ItemKind::Field(Field::value(label, value)))
}

/// A label with a short helper line beneath it, paired with a trailing
/// control. The helper line is part of the label block and the control is
/// centred on the pair, so the description groups with its own row.
pub fn setting_row_described(label: &str, description: &str, control: El) -> Item {
    labelled_row(label, Some(description), control)
}

/// A row for a continuous control (a slider) that wants the whole measure:
/// the label on its own line and the control spanning the section.
pub fn slider_row(label: &str, control: El) -> Item {
    choice_row_stacked(label, control)
}

/// A row for a text field: the label on its own line and the field beneath
/// it spanning the section, as every form in Settings lays its fields out.
pub fn field_row(label: &str, control: El) -> Item {
    choice_row_stacked(label, control)
}

/// One entry of a list of things (a printer, a user, a device): its name with
/// any status marks on the top line, what is worth knowing about it hugging
/// the name below, and the actions that apply to it under the text. Every
/// such list reads the same way, whether or not an entry has actions.
pub fn item_row(title: &str, status: Vec<El>, details: &[String], actions: Vec<Button>) -> Item {
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
        return row(Box::new(block));
    }
    let mut buttons = Row::new().with_rhythm(Rhythm::Related);
    for action in actions {
        buttons = buttons.push(Box::new(action.control_size(ControlSize::Small)));
    }
    row(Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(block))
            .push(Box::new(buttons)),
    ))
}

/// A row for a thing with an icon (a network, a device): the icon, its name
/// with a line of detail hugging it, and a trailing control. The control
/// keeps its place however narrow the window gets.
pub fn icon_row(icon: Icon, title: &str, detail: &str, control: El) -> Item {
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
    row(trailing_row(
        Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(lead)),
        ),
        control,
    ))
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
pub fn error_row(label: &str, message: &str) -> Item {
    Item(ItemKind::Field(
        Field::custom(
            label,
            Box::new(
                Text::new(&crate::i18n::t("settings-read-error"))
                    .with_role(TextRole::Label)
                    .with_weight(ui::text_render::TextWeight::Semibold),
            ),
        )
        .with_trailing_control()
        .with_error(message),
    ))
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
            .with_child(Box::new(
                Row::new()
                    .no_wrap()
                    .with_align_items(taffy::AlignItems::Center)
                    .with_justify(taffy::JustifyContent::FlexEnd)
                    .push(control),
            )),
    )
}

/// A settings row whose trailing control is a compact chooser (dropdown,
/// segmented picker): at least the compact chooser width at the row's right
/// edge beside the label. When the section is too narrow for its labels and
/// choosers, every row of it puts its chooser beneath the label, full width.
pub fn choice_row(label: &str, control: El) -> Item {
    labelled_row(label, None, control)
}

/// [`choice_row`] with a helper line under the label.
pub fn choice_row_described(label: &str, description: &str, control: El) -> Item {
    labelled_row(label, Some(description), control)
}

/// A labelled row whose control takes the full width beneath its label, at
/// every width: a slider, a text field.
pub fn choice_row_stacked(label: &str, control: El) -> Item {
    Item(ItemKind::Field(
        Field::custom(label, control).with_label_placement(Some(LabelPlacement::Above)),
    ))
}

/// [`choice_row_stacked`] with a helper line under the label: the sentence
/// belongs to this row, not to the group.
pub fn field_row_described(label: &str, description: &str, control: El) -> Item {
    Item(ItemKind::Field(
        Field::custom(label, control)
            .with_label_placement(Some(LabelPlacement::Above))
            .with_help(description),
    ))
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

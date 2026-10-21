//! The settings pages' vocabulary, on the design system's primitives
//! (`docs/design-language.md`, "Primitives").
//!
//! A page is a stack of groups on the window ground, separated by space and
//! type weight rather than boxes: each group is a [`ui::widget::Section`]
//! with a `Heading` title over its rows. A setting is a
//! [`ui::widget::ListRow`]: its label, an optional one-line explanation
//! under it, and its control on the trailing edge. Consecutive rows are one
//! list, edge to edge with an inset separator between them. A reading is a
//! row whose value trails; a thing you act on is a row of its own (a
//! device, an account), its secondary actions in its [`ui::widget::RowMenu`].
//!
//! ## Spacing vocabulary
//!
//! Every gap here is a relationship ([`Rhythm`]), resolved against the live
//! theme at layout time:
//!
//! - **tight** — a description hugs the label it explains;
//! - **sibling** — a note under a group's rows, a block among them;
//! - **group** — one group to the next.

use std::cell::Cell;

use ui::icons::Icon;
use ui::tokens::{Metrics, Rhythm, Spacing, SpacingStep, TextRole};
pub use ui::widget::Tone as StatusTone;
use ui::widget::{
    Badge, Button, ChipStyle, Column, Container, ControlSize, IconSize, IconWidget, ListRow, Row,
    Section, SectionHeader, Spacer, Text, TextStack, Widget,
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

/// One entry of a group:
///
/// - a row ([`setting_row`], [`value_row`], [`thing_row`], or any list row
///   handed to [`row`]): consecutive rows are one list;
/// - a block ([`block`], [`field_row`]): content that is not a row (a chart,
///   a preview, a field with its label above it);
/// - a [`note`]: the sentence under the group that explains it.
pub struct Item(ItemKind);

enum ItemKind {
    Row(El),
    Block(El),
    Note(String),
}

/// The entries of one group: what [`rows`] collects.
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
/// element on its trailing edge) over its entries. Built by [`section`],
/// [`section_with`], [`block_section`] and [`untitled`], assembled into a
/// page by [`page`].
pub struct SettingsSection {
    heading: Heading,
    body: Vec<Item>,
}

enum Heading {
    None,
    Title(String),
    Trailing(String, El),
    /// Boxed: a button is many times the other headings' size.
    Action(String, Box<Button>),
}

impl SettingsSection {
    /// The group as the toolkit's [`Section`]. Notes that open the group
    /// read under its heading, before the rows; every other note stands
    /// under the group, after its last row.
    #[must_use]
    pub fn into_section(self) -> Section {
        let mut body = self.body.into_iter().peekable();
        let mut items: Vec<El> = Vec::new();
        let mut intro = Vec::new();
        while let Some(Item(ItemKind::Note(_))) = body.peek() {
            if let Some(Item(ItemKind::Note(text))) = body.next() {
                intro.push(text);
            }
        }
        if !intro.is_empty() {
            items.push(notes_block(&intro));
        }
        let mut notes = Vec::new();
        for item in body {
            match item.0 {
                ItemKind::Note(text) => notes.push(text),
                ItemKind::Row(content) | ItemKind::Block(content) => items.push(content),
            }
        }
        if !notes.is_empty() {
            items.push(notes_block(&notes));
        }
        let section = Section::new(items);
        match self.heading {
            Heading::None => section,
            Heading::Title(title) => section.with_header(SectionHeader::heading(title)),
            Heading::Trailing(title, trailing) => {
                section.with_header(SectionHeader::heading(title).trailing(trailing))
            }
            Heading::Action(title, action) => {
                section.with_header(SectionHeader::heading(title).action(*action))
            }
        }
    }
}

fn notes_block(notes: &[String]) -> El {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Tight);
    for note in notes {
        column = column.push(caption(note));
    }
    Box::new(column)
}

/// The whole page: its groups one `group` apart. The frame (readable width,
/// scrolling, gutters) is the shell's.
pub fn page(sections: Vec<SettingsSection>) -> El {
    Box::new(
        sections.into_iter().fold(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(Rhythm::Group),
            |page, section| page.push(Box::new(section.into_section())),
        ),
    )
}

/// The entries of a group, in order.
pub fn rows(items: Vec<Item>) -> Body {
    Body(items)
}

/// A row of a group: a [`ListRow`] (or a [`ui::widget::DisclosureRow`])
/// built by the page, which joins the rows around it into one list.
pub fn row(content: El) -> Item {
    Item(ItemKind::Row(content))
}

/// A block of a group that is not a row (a chart, a preview, a status
/// line): full width, `sibling` apart from the rows around it.
pub fn block(content: El) -> Item {
    Item(ItemKind::Block(content))
}

/// The sentence under a group that explains it: dim, wrapped, after the
/// last row (or before the first when it opens the group).
pub fn note(text: &str) -> Item {
    Item(ItemKind::Note(text.to_owned()))
}

/// A titled group: a `Heading` line over `body`, on the ground. Groups are
/// separated by space, never boxed.
pub fn section(title: &str, body: Body) -> SettingsSection {
    SettingsSection {
        heading: Heading::Title(title.to_owned()),
        body: body.0,
    }
}

/// A group whose heading also carries one element on its trailing edge:
/// the switch for the whole group, its status as a badge, or the one
/// action that adds to it.
pub fn section_with(title: &str, trailing: El, body: Body) -> SettingsSection {
    SettingsSection {
        heading: Heading::Trailing(title.to_owned(), trailing),
        body: body.0,
    }
}

/// A group whose heading carries the one text action that acts on the
/// whole group ("Look for devices", "Add printer"), its label on the
/// trailing edge.
pub fn section_action(title: &str, action: Button, body: Body) -> SettingsSection {
    SettingsSection {
        heading: Heading::Action(title.to_owned(), Box::new(action)),
        body: body.0,
    }
}

/// A group holding one free-standing block (a pane's identity header, a
/// preview), with no heading.
pub fn block_section(content: El) -> SettingsSection {
    untitled(rows(vec![block(content)]))
}

/// A group without a heading: the opening group of a pane whose title
/// already says what it is.
pub fn untitled(body: Body) -> SettingsSection {
    SettingsSection {
        heading: Heading::None,
        body: body.0,
    }
}

/// The list row of a setting labelled `label`: the label, `description`
/// under it, `control` on the trailing edge (under the text when the row is
/// too narrow for both).
fn setting_list_row(label: &str, description: Option<&str>, control: El) -> ListRow {
    let row = ListRow::new(format!("setting/{label}"), label).trailing(control);
    match description {
        Some(description) => row.secondary(description).two_line_secondary(),
        None => row,
    }
}

/// A setting: its label, its control on the trailing edge. The workhorse
/// settings line.
pub fn setting_row(label: &str, control: El) -> Item {
    row(Box::new(setting_list_row(label, None, control)))
}

/// A reading: a label with its value trailing, in secondary ink.
pub fn value_row(label: &str, value: &str) -> Item {
    row(Box::new(
        ListRow::new(format!("setting/{label}"), label).value(value),
    ))
}

/// A setting with one line explaining it under its label, its control on
/// the trailing edge.
pub fn setting_row_described(label: &str, description: &str, control: El) -> Item {
    row(Box::new(setting_list_row(
        label,
        Some(description),
        control,
    )))
}

/// A setting whose control wants the whole measure (a slider, a text
/// field): the label (and its explanation) over the control.
fn stacked(label: &str, description: Option<&str>, control: El) -> Item {
    let text = match description {
        Some(description) => TextStack::new(label).secondary(description),
        None => TextStack::new(label),
    };
    block(Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(text))
            .push(control),
    ))
}

/// A slider that wants the whole measure: the label over it.
pub fn slider_row(label: &str, control: El) -> Item {
    stacked(label, None, control)
}

/// A text field: the label over it, the field spanning the group.
pub fn field_row(label: &str, control: El) -> Item {
    stacked(label, None, control)
}

/// A thing you act on (a printer, a user, a device): its name, what is
/// worth knowing about it under the name, its status marks, and one
/// control on the trailing edge (its main action, or the [`ui::widget::RowMenu`]
/// of its secondary ones). `id` names the row across rebuilds.
pub fn thing_row(id: &str, title: &str, details: &[String], trailing: Option<El>) -> ListRow {
    let mut row = ListRow::new(id, title);
    match details {
        [] => {}
        [only] => row = row.secondary(only.as_str()),
        // Each fact on a line of its own, never joined into one string.
        lines => {
            row = row
                .secondary(lines.join("\n"))
                .secondary_lines(u8::try_from(lines.len()).unwrap_or(u8::MAX));
        }
    }
    match trailing {
        Some(control) => row.trailing(control),
        None => row,
    }
}

/// One entry of a list of things, with its status marks and the buttons
/// that act on it. Pages built on the primitives use [`thing_row`] with a
/// [`ui::widget::RowMenu`] instead; this stays for the pages that still hand
/// over a line of buttons.
pub fn item_row(title: &str, status: Vec<El>, details: &[String], actions: Vec<Button>) -> Item {
    let mut text = TextStack::new(title);
    if let Some(first) = details.first() {
        text = text.secondary(first.as_str());
    }
    if let Some(second) = details.get(1) {
        text = text.tertiary(second.as_str());
    }
    let mut line = Row::new()
        .no_wrap()
        .with_align_items(taffy::AlignItems::Center)
        .with_rhythm(Rhythm::Related)
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(text)),
        ));
    for mark in status {
        line = line.push(mark);
    }
    for action in actions {
        line = line.push(Box::new(action.control_size(ControlSize::Small)));
    }
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Tight)
        .push(Box::new(line));
    for detail in details.iter().skip(2) {
        column = column.push(caption(detail));
    }
    block(Box::new(column))
}

/// A row for a thing with an icon (a network, a device): the icon, its name
/// with a line of detail under it, and a trailing control.
pub fn icon_row(icon: Icon, title: &str, detail: &str, control: El) -> Item {
    row(Box::new(
        ListRow::new(format!("setting/{title}"), title)
            .icon(icon)
            .secondary(detail)
            .trailing(control),
    ))
}

/// A dim helper-text line, for captions and "best-effort" notes.
pub fn caption(text: &str) -> El {
    Box::new(Text::new(text).with_role(TextRole::Caption).dimmed())
}

/// A failure's message ("Can't read time settings: timedatectl exited …")
/// split into what failed, in the person's words, and the technical reason
/// after the first ": ", which the screen keeps behind "Details".
fn split_failure(message: &str) -> (&str, Option<&str>) {
    match message.split_once(": ") {
        Some((what, reason)) if !what.trim().is_empty() && !reason.trim().is_empty() => {
            (what, Some(reason))
        }
        _ => (message, None),
    }
}

/// The technical reason of a failure, behind a closed "Details" disclosure.
fn failure_details(reason: &str) -> El {
    Box::new(
        Section::new(vec![Box::new(
            Text::new(reason).with_role(TextRole::Caption).dimmed(),
        )])
        .with_header(SectionHeader::group(crate::i18n::t(
            "settings-failure-details",
        )))
        .collapsed(),
    )
}

/// A reading the pane could not make, said as a failure. `message` is the
/// whole sentence ("Can't read network status: …"); it replaces the value
/// the pane would otherwise show, so a failed read never looks like a state
/// of the device ("Not connected", "Off", an empty list). What failed shows;
/// the technical reason waits behind "Details". Screen readers announce the
/// whole sentence when it appears.
pub fn error_caption(message: &str) -> El {
    let (what, reason) = split_failure(message);
    let headline = Text::new(what)
        .with_role(TextRole::Caption)
        .with_weight(ui::text_render::TextWeight::Semibold)
        .with_live(ui::a11y::Politeness::Polite)
        .with_spoken(message);
    match reason {
        None => Box::new(headline),
        Some(reason) => Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(headline))
                .push(failure_details(reason)),
        ),
    }
}

/// A label whose value could not be read: the label keeps its place in the
/// group, "Can't read" stands where the value would be, and why under it,
/// said to assistive technology as it appears.
pub fn error_row(label: &str, message: &str) -> Item {
    block(Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Tight)
            .push(Box::new(
                ListRow::new(format!("setting/{label}"), label)
                    .value(crate::i18n::t("settings-read-error"))
                    .error(),
            ))
            .push(error_caption(message)),
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
/// row's right edge instead of stretching across the whole group.
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

/// The width of a slider on a row's trailing edge: one for every slider of
/// a page, so they line up.
pub const TRAILING_SLIDER_WIDTH: f32 = 220.0;
/// The least a trailing slider shrinks to in a narrow window.
const TRAILING_SLIDER_MIN_WIDTH: f32 = 120.0;

/// A slider (or a slider with its value and a button beside it) on a row's
/// trailing edge, at [`TRAILING_SLIDER_WIDTH`].
pub fn trailing_slider(control: El) -> El {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(TRAILING_SLIDER_WIDTH)
            .with_min_width(TRAILING_SLIDER_MIN_WIDTH)
            .with_child(control),
    )
}

/// A setting whose control takes the full width beneath its label, at
/// every width: a slider, a text field.
pub fn choice_row_stacked(label: &str, control: El) -> Item {
    stacked(label, None, control)
}

/// [`choice_row_stacked`] with a line explaining it under the label.
pub fn field_row_described(label: &str, description: &str, control: El) -> Item {
    stacked(label, Some(description), control)
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

/// A prominent identity block for a pane's first group: a large icon
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
#[cfg(any(test, feature = "test-support"))]
pub fn announced_text(widget: &dyn Widget) -> Vec<String> {
    fn walk(widget: &dyn Widget, out: &mut Vec<String>) {
        if let Some(node) = widget.accessibility() {
            out.extend(node.name);
            out.extend(node.value);
        }
        // A row's menu says its entries when it opens.
        if let Some(menu) = widget
            .as_any()
            .and_then(|any| any.downcast_ref::<ui::widget::RowMenu>())
        {
            out.extend(menu.labels().into_iter().map(str::to_owned));
        }
        for child in widget.children() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(widget, &mut out);
    out
}

// The names the Network page still uses while it moves with its owner.

/// [`section`], by its old name.
pub fn card(title: &str, body: Body) -> SettingsSection {
    section(title, body)
}

/// [`section_with`], by its old name.
pub fn card_with_action(title: &str, action: El, body: Body) -> SettingsSection {
    section_with(title, action, body)
}

/// [`page`], by its old name.
pub fn cards(sections: Vec<SettingsSection>) -> El {
    page(sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failed reading shows what failed; the command's own words wait
    /// behind a closed "Details", and a screen reader hears the whole
    /// sentence.
    #[test]
    fn a_failed_reading_shows_what_failed_and_keeps_the_reason_behind_details() {
        let message = "Can't read time settings: timedatectl exited with status 1";
        let caption = error_caption(message);
        let spoken = announced_text(caption.as_ref());
        assert!(spoken.iter().any(|line| line == message), "{spoken:?}");
        assert!(
            spoken
                .iter()
                .any(|line| *line == crate::i18n::t("settings-failure-details")),
            "{spoken:?}"
        );
        assert!(
            !spoken
                .iter()
                .any(|line| line == "timedatectl exited with status 1"),
            "the reason is closed until opened: {spoken:?}"
        );
        assert_eq!(
            split_failure("Nothing to split"),
            ("Nothing to split", None)
        );
    }
}

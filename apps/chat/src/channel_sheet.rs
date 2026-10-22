//! Making a channel or a category, and changing a channel: the sheet that
//! asks what kind of channel, its name, where it goes and what it is about.
//!
//! The sheet only gathers the answers. What it settles on is applied by the
//! chat as its record's own verbs (`create-channel`, `create-category`) and
//! field edits, the same path an agent or a script takes.

use vello::Scene;
use vello::kurbo::{Point, Rect, Vec2};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::PresenceCtx;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    ChipFace, ChipLead, ListRowFace, ModalAction, ModalSheet, ModalSpec, SheetResponse, TextInput,
    Widget,
};

use crate::model::{ChannelKind, Id};

/// Longest channel or category name the sheet takes.
const MAX_NAME_CHARS: usize = 100;
/// Longest topic the sheet takes.
const MAX_TOPIC_CHARS: usize = 1024;

/// What the sheet makes or changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SheetMode {
    NewChannel,
    NewCategory,
    /// An existing channel: its name, category and topic.
    EditChannel(Id),
}

/// What the person settled on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SheetAnswer {
    pub mode: SheetMode,
    pub kind: ChannelKind,
    /// The name as it will be kept: a text channel's normalised.
    pub name: String,
    /// The category it goes under; `None` for none.
    pub category: Option<Id>,
    pub topic: String,
}

/// How a routed event left the sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SheetOutcome {
    /// Still open.
    Open,
    /// Closed without changing anything.
    Cancelled,
    /// Closed with these answers to apply.
    Done(SheetAnswer),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Topic,
}

/// What a press inside the sheet's body lands on.
#[derive(Clone, Debug, PartialEq)]
enum BodyHit {
    Kind(ChannelKind),
    Category(Option<Id>),
    Field(Field),
}

/// The sheet's state while it is open.
pub(crate) struct ChannelSheet {
    mode: SheetMode,
    kind: ChannelKind,
    category: Option<Id>,
    name: TextInput,
    topic: TextInput,
    focus: Field,
    sheet: ModalSheet,
    hits: Vec<(Rect, BodyHit)>,
    pointer: Option<Point>,
}

/// A text channel's name as it is kept: lower case, words joined by `-`,
/// only letters, digits, `-` and `_`. `typing` keeps a trailing `-` so the
/// next word can follow it.
pub(crate) fn channel_slug(name: &str, typing: bool) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.trim_start().chars() {
        let c = if c.is_whitespace() { '-' } else { c };
        if c == '-' {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else if c.is_alphanumeric() || c == '_' {
            out.extend(c.to_lowercase());
        }
    }
    if !typing {
        while out.ends_with('-') {
            out.pop();
        }
    }
    out.chars().take(MAX_NAME_CHARS).collect()
}

impl ChannelSheet {
    fn new(mode: SheetMode) -> Self {
        let mut sheet = Self {
            mode,
            kind: ChannelKind::Text,
            category: None,
            name: TextInput::local(),
            topic: TextInput::local(),
            focus: Field::Name,
            sheet: ModalSheet::new(),
            hits: Vec::new(),
            pointer: None,
        };
        sheet.retitle();
        sheet
    }

    /// A new channel of `kind`, under `category` (`""` for none).
    pub(crate) fn new_channel(category: &str, kind: ChannelKind) -> Self {
        let mut sheet = Self::new(SheetMode::NewChannel);
        sheet.kind = kind;
        sheet.category = (!category.is_empty()).then(|| category.to_owned());
        sheet.retitle();
        sheet
    }

    pub(crate) fn new_category() -> Self {
        Self::new(SheetMode::NewCategory)
    }

    /// Change `channel`.
    pub(crate) fn edit_channel(channel: &crate::model::Channel) -> Self {
        let mut sheet = Self::new(SheetMode::EditChannel(channel.id.clone()));
        sheet.kind = channel.kind;
        sheet.category = (!channel.category.is_empty()).then(|| channel.category.clone());
        sheet.name.set_text(&channel.name);
        sheet.name.set_cursor_pos(channel.name.len());
        sheet.topic.set_text(&channel.topic);
        sheet.topic.set_cursor_pos(channel.topic.len());
        sheet.retitle();
        sheet
    }

    /// Type `name` and `topic` in, as the person would.
    pub(crate) fn fill(&mut self, name: &str, topic: &str) {
        self.name.set_text(name);
        self.name.set_cursor_pos(name.len());
        self.topic.set_text(topic);
        self.topic.set_cursor_pos(topic.len());
        if self.kind == ChannelKind::Text && self.shows_channel_fields() {
            self.normalise_name();
        }
    }

    /// The fields' hints, which follow the kind of channel.
    fn retitle(&mut self) {
        let name_hint = match (&self.mode, self.kind) {
            (SheetMode::NewCategory, _) => crate::i18n::t("chat-sheet-category-placeholder"),
            (_, ChannelKind::Text) => crate::i18n::t("chat-sheet-text-placeholder"),
            (_, ChannelKind::Voice) => crate::i18n::t("chat-sheet-voice-placeholder"),
        };
        self.name.set_placeholder(&name_hint);
        self.topic
            .set_placeholder(&crate::i18n::t("chat-sheet-topic-placeholder"));
        if self.kind == ChannelKind::Text && self.mode != SheetMode::NewCategory {
            self.normalise_name();
        }
    }

    /// Keep a text channel's name in its kept form while it is typed.
    fn normalise_name(&mut self) {
        let typed = self.name.value().to_owned();
        let slug = channel_slug(&typed, true);
        if slug != typed {
            let caret = self.name.cursor_pos().min(typed.len());
            let kept_before = channel_slug(typed.get(..caret).unwrap_or(&typed), true).len();
            self.name.set_text(&slug);
            self.name.set_cursor_pos(kept_before.min(slug.len()));
        }
    }

    fn shows_kind(&self) -> bool {
        self.mode == SheetMode::NewChannel
    }

    fn shows_channel_fields(&self) -> bool {
        self.mode != SheetMode::NewCategory
    }

    /// The answers as they would be applied, when they are complete.
    pub(crate) fn answer(&self) -> Option<SheetAnswer> {
        let name = if self.shows_channel_fields() && self.kind == ChannelKind::Text {
            channel_slug(self.name.value(), false)
        } else {
            self.name
                .value()
                .trim()
                .chars()
                .take(MAX_NAME_CHARS)
                .collect()
        };
        if name.is_empty() {
            return None;
        }
        Some(SheetAnswer {
            mode: self.mode.clone(),
            kind: self.kind,
            name,
            category: self.category.clone(),
            topic: self
                .topic
                .value()
                .trim()
                .chars()
                .take(MAX_TOPIC_CHARS)
                .collect(),
        })
    }

    fn title(&self) -> String {
        match self.mode {
            SheetMode::NewChannel => crate::i18n::t("chat-sheet-new-channel"),
            SheetMode::NewCategory => crate::i18n::t("chat-sheet-new-category"),
            SheetMode::EditChannel(_) => crate::i18n::t("chat-sheet-edit-channel"),
        }
    }

    fn confirm_label(&self) -> String {
        match self.mode {
            SheetMode::NewChannel => crate::i18n::t("chat-sheet-create-channel"),
            SheetMode::NewCategory => crate::i18n::t("chat-sheet-create-category"),
            SheetMode::EditChannel(_) => crate::i18n::t("chat-sheet-save"),
        }
    }

    fn focused_input(&mut self) -> &mut TextInput {
        match self.focus {
            Field::Name => &mut self.name,
            Field::Topic => &mut self.topic,
        }
    }

    fn set_focus(&mut self, field: Field, host_focused: bool) {
        self.focus = field;
        Widget::set_focus(&mut self.name, host_focused && field == Field::Name);
        Widget::set_focus(&mut self.topic, host_focused && field == Field::Topic);
    }

    /// Paint the sheet over `window`; `categories` are the room's, in
    /// sidebar order.
    #[allow(
        clippy::too_many_lines,
        reason = "one paint pass lays out and paints the sheet's sections in order"
    )]
    pub(crate) fn paint(
        &mut self,
        paint: PaintCtx<'_>,
        presence: &PresenceCtx,
        window: Rect,
        categories: &[(Id, String)],
        host_focused: bool,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let title = self.title();
        let confirm = self.confirm_label();
        let cancel = crate::i18n::t("chat-sheet-cancel");
        let actions = [
            ModalAction::primary(&confirm).enabled(self.answer().is_some()),
            ModalAction::secondary(&cancel),
        ];
        let probe = ModalSpec::new(&title).actions(&actions).max_width(480.0);
        let width = ui::widget::modal_dialog::layout(window, &probe, tr, theme)
            .body
            .width();
        let none = crate::i18n::t("chat-sheet-no-category");
        let body_h = self.body_height(tr, theme, width, categories, &none);
        let spec = probe.body_height(body_h);
        self.set_focus(self.focus, host_focused);
        let mut hits = Vec::new();
        let Self {
            mode,
            kind,
            category,
            name,
            topic,
            sheet,
            pointer,
            ..
        } = self;
        let pointer = *pointer;
        let mode = mode.clone();
        let kind = *kind;
        let category = category.clone();
        sheet.paint(
            scene,
            tf,
            tr,
            theme,
            window,
            &spec,
            |scene, tf, tr, theme, body| {
                let mut y = body.y0;
                let sp = &theme.tokens.spacing;
                let section = |scene: &mut Scene, tr: &mut TextRenderer, y: f64, label: &str| {
                    let style = theme.text(TextRole::Label);
                    tr.draw_style(
                        scene,
                        label,
                        theme.text_dim,
                        tf.pre_translate(Vec2::new(body.x0, y)),
                        None,
                        &style,
                    );
                    y + f64::from(style.line_height_px()) + f64::from(sp.s2)
                };
                if mode == SheetMode::NewChannel {
                    y = section(scene, tr, y, &crate::i18n::t("chat-sheet-kind"));
                    for (option, icon, label, about) in [
                        (
                            ChannelKind::Text,
                            Icon::Hash,
                            crate::i18n::t("chat-sheet-text"),
                            crate::i18n::t("chat-sheet-text-about"),
                        ),
                        (
                            ChannelKind::Voice,
                            Icon::Volume2,
                            crate::i18n::t("chat-sheet-voice"),
                            crate::i18n::t("chat-sheet-voice-about"),
                        ),
                    ] {
                        let face = ListRowFace::new(&label)
                            .leading(ui::widget::RowLeading::Icon(icon))
                            .secondary(&about)
                            .checked(kind == option)
                            .selected(kind == option);
                        let h = face.height(tr, theme);
                        let row = Rect::new(body.x0, y, body.x1, y + h);
                        let hovered = pointer.is_some_and(|p| row.contains(p));
                        let face = face.interaction(ui::widget::Interaction {
                            hovered,
                            ..ui::widget::Interaction::default()
                        });
                        face.paint(&mut PaintCtx::new(scene, tf, tr, theme), row);
                        hits.push((row, BodyHit::Kind(option)));
                        y = row.y1 + f64::from(sp.s1);
                    }
                    y += f64::from(sp.s4);
                }
                let name_label = if mode == SheetMode::NewCategory {
                    crate::i18n::t("chat-sheet-category-name")
                } else {
                    crate::i18n::t("chat-sheet-channel-name")
                };
                y = section(scene, tr, y, &name_label);
                let field_h = field_height(theme);
                let field = Rect::new(body.x0, y, body.x1, y + field_h);
                name.apply_layout_external(field);
                name.draw(scene, tf, tr, theme, presence);
                hits.push((field, BodyHit::Field(Field::Name)));
                y = field.y1 + f64::from(sp.s6);
                if mode != SheetMode::NewCategory {
                    y = section(scene, tr, y, &crate::i18n::t("chat-sheet-category"));
                    let chip_h = ChipFace::height(theme);
                    let gap = f64::from(sp.s2);
                    let mut x = body.x0;
                    let choices = std::iter::once((None, none.as_str())).chain(
                        categories
                            .iter()
                            .map(|(id, name)| (Some(id), name.as_str())),
                    );
                    for (id, label) in choices {
                        let w = ChipFace::intrinsic_width(label, ChipLead::None, false, tr, theme);
                        if x > body.x0 && x + w > body.x1 {
                            x = body.x0;
                            y += chip_h + gap;
                        }
                        let chip = Rect::new(x, y, (x + w).min(body.x1), y + chip_h);
                        let chosen = category.as_ref() == id;
                        ChipFace::new(chip, label)
                            .selected(chosen)
                            .hovered(pointer.is_some_and(|p| chip.contains(p)))
                            .paint(scene, tf, tr, theme);
                        hits.push((chip, BodyHit::Category(id.cloned())));
                        x = chip.x1 + gap;
                    }
                    y += chip_h + f64::from(sp.s6);
                    y = section(scene, tr, y, &crate::i18n::t("chat-sheet-topic"));
                    let field = Rect::new(body.x0, y, body.x1, y + field_h);
                    topic.apply_layout_external(field);
                    topic.draw(scene, tf, tr, theme, presence);
                    hits.push((field, BodyHit::Field(Field::Topic)));
                }
            },
        );
        self.hits = hits;
    }

    /// The body's height at `width`, as [`Self::paint`] lays it out.
    fn body_height(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        categories: &[(Id, String)],
        none: &str,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let section = f64::from(theme.text(TextRole::Label).line_height_px()) + f64::from(sp.s2);
        let field = field_height(theme);
        let mut h = section + field;
        if self.shows_kind() {
            let about = crate::i18n::t("chat-sheet-text-about");
            let row = ListRowFace::new("Text")
                .leading(ui::widget::RowLeading::Icon(Icon::Hash))
                .secondary(&about)
                .checked(true)
                .height(tr, theme);
            h += section + 2.0 * (row + f64::from(sp.s1)) + f64::from(sp.s4);
        }
        if self.shows_channel_fields() {
            let chip_h = ChipFace::height(theme);
            let gap = f64::from(sp.s2);
            let mut lines = 1.0;
            let mut x = 0.0;
            for label in std::iter::once(none).chain(categories.iter().map(|(_, n)| n.as_str())) {
                let w = ChipFace::intrinsic_width(label, ChipLead::None, false, tr, theme);
                if x > 0.0 && x + w > width {
                    x = 0.0;
                    lines += 1.0;
                }
                x += w + gap;
            }
            h += f64::from(sp.s6) + section + lines * chip_h + (lines - 1.0) * gap;
            h += f64::from(sp.s6) + section + field;
        }
        h
    }

    /// Route `event` to the sheet; every event stops here while it is open.
    pub(crate) fn handle(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
        host_focused: bool,
    ) -> SheetOutcome {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(Point::new(position.x / scale, position.y / scale));
                let _ = self.sheet.handle(event, scale);
                crate::events::drive_text_input(&mut self.name, event, scale, modifiers, doc);
                crate::events::drive_text_input(&mut self.topic, event, scale, modifiers, doc);
                SheetOutcome::Open
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let hit = self.pointer.and_then(|p| {
                    self.hits
                        .iter()
                        .find(|(rect, _)| rect.contains(p))
                        .map(|(_, hit)| hit.clone())
                });
                match hit {
                    Some(BodyHit::Kind(kind)) => {
                        self.kind = kind;
                        self.retitle();
                        SheetOutcome::Open
                    }
                    Some(BodyHit::Category(category)) => {
                        self.category = category;
                        SheetOutcome::Open
                    }
                    Some(BodyHit::Field(field)) => {
                        self.set_focus(field, host_focused);
                        crate::events::drive_text_input(
                            self.focused_input(),
                            event,
                            scale,
                            modifiers,
                            doc,
                        );
                        SheetOutcome::Open
                    }
                    None => self.respond(event, scale),
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                crate::events::drive_text_input(self.focused_input(), event, scale, modifiers, doc);
                self.respond(event, scale)
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match logical_key {
                Key::Named(NamedKey::Escape) => SheetOutcome::Cancelled,
                Key::Named(NamedKey::Enter) => self.finish(),
                Key::Named(NamedKey::Tab) => {
                    let next = if self.shows_channel_fields() && self.focus == Field::Name {
                        Field::Topic
                    } else {
                        Field::Name
                    };
                    self.set_focus(next, host_focused);
                    SheetOutcome::Open
                }
                _ => {
                    self.type_into_focused(event, scale, modifiers, doc);
                    SheetOutcome::Open
                }
            },
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                self.type_into_focused(event, scale, modifiers, doc);
                SheetOutcome::Open
            }
            _ => SheetOutcome::Open,
        }
    }

    fn type_into_focused(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        // A key reaching the sheet means its window has the keyboard.
        self.set_focus(self.focus, true);
        crate::events::drive_text_input(self.focused_input(), event, scale, modifiers, doc);
        let limit = match self.focus {
            Field::Name => MAX_NAME_CHARS,
            Field::Topic => MAX_TOPIC_CHARS,
        };
        let input = self.focused_input();
        if input.value().chars().count() > limit {
            let kept: String = input.value().chars().take(limit).collect();
            input.set_text(&kept);
            input.set_cursor_pos(kept.len());
        }
        if self.focus == Field::Name
            && self.kind == ChannelKind::Text
            && self.shows_channel_fields()
        {
            self.normalise_name();
        }
    }

    /// What the sheet's own chrome (actions, close, scrim) answers.
    fn respond(&mut self, event: &WindowEvent, scale: f64) -> SheetOutcome {
        match self.sheet.handle(event, scale) {
            Some(SheetResponse::Action(0)) => self.finish(),
            Some(SheetResponse::Action(_) | SheetResponse::Dismiss) => SheetOutcome::Cancelled,
            None => SheetOutcome::Open,
        }
    }

    fn finish(&self) -> SheetOutcome {
        self.answer().map_or(SheetOutcome::Open, SheetOutcome::Done)
    }
}

/// One single-line field's height.
fn field_height(theme: &Theme) -> f64 {
    ui::widget::ButtonFace::height(ui::widget::ControlSize::Large, theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_channel_name_is_kept_lower_case_and_joined_by_dashes() {
        assert_eq!(channel_slug("Design Reviews!", false), "design-reviews");
        assert_eq!(channel_slug("  release  notes ", false), "release-notes");
        assert_eq!(channel_slug("Q3 plan ", true), "q3-plan-");
        assert_eq!(channel_slug("ünïcode_ok", false), "ünïcode_ok");
        assert_eq!(channel_slug("---", false), "");
    }

    #[test]
    fn a_sheet_needs_a_name_and_keeps_where_it_was_opened() {
        let mut sheet = ChannelSheet::new_channel("cat_design", ChannelKind::Voice);
        assert_eq!(sheet.answer(), None, "no name yet");
        sheet.name.set_text("Stand Up");
        let answer = sheet.answer().expect("named");
        assert_eq!(answer.name, "Stand Up", "a voice channel keeps its name");
        assert_eq!(answer.category.as_deref(), Some("cat_design"));
        assert_eq!(answer.kind, ChannelKind::Voice);
        sheet.kind = ChannelKind::Text;
        assert_eq!(sheet.answer().expect("named").name, "stand-up");
    }
}

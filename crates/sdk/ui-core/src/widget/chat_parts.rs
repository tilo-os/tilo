//! The pieces of a conversation as described components, over the widgets native Chat already
//! uses: the reactions on a message (`ui.Reactions`, the [`ReactionStripWidget`]) and a button
//! that opens the emoji picker (`ui.EmojiPicker`, the shared [`EmojiPicker`] in a popover).
//! Both report the emoji as data (`on toggle(e)`, `on pick(e)`); the app's action does the rest.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tilo_resource::{ReactionGroup, Reactor};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use crate::component::{Component, Described, Event, Person, Value};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{
    Button, Decorator, EmojiPicker, EventCtx, PickerEvent, Popover, ReactionStripWidget, Widget,
};

/// The reactions on something: a pill per emoji with how many reacted, yours drawn on;
/// pressing one adds or takes back your reaction, and the add button opens the emoji picker for
/// another. Each reports the emoji (`on toggle(e)`).
#[derive(Component, Default)]
#[component(name = "Reactions", role = "Toolbar")]
pub struct ReactionsProps {
    /// Who reacted, by emoji: the reactions as the message holds them
    /// (`Map<Text, Set<Principal>>`).
    pub reactions: BTreeMap<String, BTreeSet<Person>>,
    /// The emoji reacted with, in the order they were first used: the older form, with `by`,
    /// read while `reactions` is not given.
    #[component(positional)]
    pub emoji: Vec<String>,
    /// Who reacted with each of `emoji`, in the same order.
    pub by: Vec<Vec<Person>>,
    /// Whether the person may react; they may unless it says not.
    pub can_react: Option<bool>,
    /// An emoji was pressed or picked: carries it. Your reaction with it is added, or taken
    /// back when you had reacted with it.
    #[component(event(value = String))]
    pub toggle: Event,
}

impl ReactionsProps {
    /// Each emoji with who reacted with it, in the order shown: the map's, else the older
    /// form's.
    fn groups(
        reactions: BTreeMap<String, BTreeSet<Person>>,
        emoji: Vec<String>,
        by: Vec<Vec<Person>>,
    ) -> Vec<(String, Vec<Person>)> {
        if reactions.is_empty() {
            let mut by = by.into_iter();
            emoji
                .into_iter()
                .map(|emoji| (emoji, by.next().unwrap_or_default()))
                .collect()
        } else {
            reactions
                .into_iter()
                .map(|(emoji, people)| (emoji, people.into_iter().collect()))
                .collect()
        }
    }
}

impl Described for ReactionStripWidget {
    type Props = ReactionsProps;

    fn from_props(key: &str, props: ReactionsProps) -> Self {
        let ReactionsProps {
            reactions,
            emoji,
            by,
            can_react,
            toggle,
        } = props;
        let mut names = std::collections::HashMap::new();
        let groups = ReactionsProps::groups(reactions, emoji, by)
            .into_iter()
            .enumerate()
            .map(|(at, (emoji, people))| ReactionGroup {
                emoji,
                reactors: people
                    .into_iter()
                    .map(|person| {
                        let reactor = Reactor {
                            by: person.identity.clone(),
                            persona: None,
                            name: None,
                        };
                        names.insert(person.identity, person.name);
                        reactor
                    })
                    .collect(),
                first_at: i64::try_from(at).unwrap_or(i64::MAX),
            })
            .collect();
        // Who "you" are is the window's own participant, read when it draws.
        Self::new(groups, String::new())
            .with_names(move |reactor: &Reactor| {
                names
                    .get(&reactor.by)
                    .cloned()
                    .unwrap_or_else(|| reactor.by.clone())
            })
            .can_react(can_react.unwrap_or(true))
            .on_toggle(move |picked| toggle.fire(Some(Value::Text(picked.emoji))))
            .with_id(key)
    }
}

/// A button that opens the emoji picker beside it; the emoji picked is reported (`on pick(e)`).
#[derive(Component, Default)]
#[component(name = "EmojiPicker", role = "Button", label = "label")]
pub struct EmojiPickerProps {
    /// What the button is called; "Add reaction" unless it says.
    pub label: Option<String>,
    /// An emoji was picked: carries it.
    #[component(event(value = String))]
    pub pick: Event,
}

/// See [`EmojiPickerProps`].
pub struct EmojiPickerButton {
    button: Button,
    picker: Popover<EmojiPicker>,
    pressed: Rc<Cell<bool>>,
    pick: Event,
}

impl EmojiPickerButton {
    /// The button `key`, called `label`, reporting each pick to `pick`.
    #[must_use]
    pub fn new(key: &str, label: &str, pick: Event) -> Self {
        let pressed = Rc::new(Cell::new(false));
        let press = Rc::clone(&pressed);
        let button = Button::icon_only(key, crate::icons::Icon::SmilePlus, label)
            .on_click(move || press.set(true));
        Self {
            button,
            picker: Popover::with_content(EmojiPicker::new())
                .with_label(crate::i18n::widget_format("emoji-chooser-title", &[])),
            pressed,
            pick,
        }
    }

    /// Whether the picker is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }
}

impl Described for EmojiPickerButton {
    type Props = EmojiPickerProps;

    fn from_props(key: &str, props: EmojiPickerProps) -> Self {
        let label = props
            .label
            .unwrap_or_else(|| crate::i18n::I18nManager::lookup_plain("emoji-picker-add-reaction"));
        Self::new(key, &label, props.pick)
    }
}

impl Decorator for EmojiPickerButton {
    type Inner = Button;

    fn inner(&self) -> &Button {
        &self.button
    }

    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }

    fn name(&self) -> &'static str {
        "EmojiPicker"
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if self.picker.is_open() {
            self.picker.handle_event(event, ctx);
            for happened in self.picker.content_mut().take_events() {
                if let PickerEvent::Picked(emoji) = happened {
                    self.pick.fire(Some(Value::Text(emoji)));
                }
                self.picker.close();
            }
            if !self.picker.content().is_open() {
                self.picker.close();
            }
            return;
        }
        self.button.handle_event(event, ctx);
        if self.pressed.replace(false) {
            let anchor: Rect = self.button.get_rect();
            self.picker.set_anchor(anchor);
            self.picker.content_mut().reopen();
            self.picker.set_open(true);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.button
            .draw_overlay(scene, transform, text, theme, presence);
        self.picker
            .draw_overlay(scene, transform, text, theme, presence);
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.picker.modal_capture()
    }

    fn needs_animation_frame(&self) -> bool {
        self.button.needs_animation_frame() || self.picker.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.button.needs_redraw() || self.picker.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.button.mark_drawn();
        self.picker.mark_drawn();
    }
}

/// A message quoted where another answers it, or in the reply banner over the composer: a rule
/// at its leading edge, who wrote it and the start of what it says. With `dismiss` it has a
/// close button (the banner: "stop replying").
#[derive(Component, Default)]
#[component(name = "Quote", role = "Group", label = "excerpt")]
pub struct QuoteProps {
    /// The start of what the quoted message says.
    #[component(required, positional)]
    pub excerpt: String,
    /// Who wrote it.
    pub author: Option<Person>,
    /// What the quote is to the message (`Replying to`), shown before the author.
    pub caption: Option<String>,
    /// The close button was pressed. Without it there is no close button.
    #[component(event)]
    pub dismiss: Event,
}

/// See [`QuoteProps`].
pub struct Quote {
    row: crate::widget::arrange::Row,
}

impl Described for Quote {
    type Props = QuoteProps;

    fn from_props(key: &str, props: QuoteProps) -> Self {
        use crate::tokens::{Rhythm, TextRole};
        use crate::widget::arrange::{Fit, Row, Stack};
        let QuoteProps {
            excerpt,
            author,
            caption,
            dismiss,
        } = props;
        let head = match (caption, author) {
            (Some(caption), Some(author)) => Some(format!("{caption} {}", author.name)),
            (Some(caption), None) => Some(caption),
            (None, Some(author)) => Some(author.name),
            (None, None) => None,
        };
        let mut said = Stack::stack().gap(Rhythm::Tight);
        if let Some(head) = head {
            said = said.push(Box::new(
                crate::widget::Text::new(&head).with_role(TextRole::Label),
            ));
        }
        said = said.push(Box::new(
            crate::widget::Text::new(&excerpt)
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
        let mut row = Row::row()
            .gap(Rhythm::Related)
            .push(Box::new(crate::widget::Divider::vertical()))
            .push_fit(Box::new(said), Fit::fill(1.0));
        if let Some(close) = dismiss.handler() {
            let mut close = close;
            let label = crate::i18n::I18nManager::lookup_plain("quote-dismiss");
            row = row.push(Box::new(
                crate::widget::Button::icon_only(
                    &format!("{key}/dismiss"),
                    crate::icons::Icon::X,
                    &label,
                )
                .on_click(move || close()),
            ));
        }
        Self {
            row: row.with_id(key),
        }
    }
}

impl Decorator for Quote {
    type Inner = crate::widget::arrange::Row;

    fn inner(&self) -> &crate::widget::arrange::Row {
        &self.row
    }

    fn inner_mut(&mut self) -> &mut crate::widget::arrange::Row {
        &mut self.row
    }

    fn name(&self) -> &'static str {
        "Quote"
    }
}

#[cfg(test)]
#[path = "chat_parts_tests.rs"]
mod tests;

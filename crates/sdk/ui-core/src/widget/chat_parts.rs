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
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
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
            .on_view(move || press.set(true));
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
        // The button publishes its press; this widget hosts it, so the
        // press is answered here, before the picker is opened from it.
        let path = ctx.path;
        crate::widget::controls::start_pending_control_invocations(
            &mut self.button,
            &mut ctx.edits(),
            path,
        );
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
        use crate::tokens::Rhythm;
        use crate::widget::arrange::{Fit, Row};
        let QuoteProps {
            excerpt,
            author,
            caption,
            dismiss,
        } = props;
        let said = QuoteLines {
            caption,
            author: author.map(|author| author.name),
            excerpt,
            rect: Rect::ZERO,
            dirty: true,
            layout_theme: Theme::dark(),
        };
        let mut row = Row::row()
            .gap(Rhythm::Related)
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
                .on_effect(move || close()),
            ));
        }
        Self {
            row: row.with_id(key),
        }
    }
}

/// What a quote says, behind a rule at its leading edge. Quoted in a message it is one line:
/// who wrote it, then the start of what it says in secondary ink, cut short at the line's end,
/// as Slack and Discord quote a reply. With a caption (the reply banner) the caption and the
/// author are a line over the excerpt.
struct QuoteLines {
    caption: Option<String>,
    author: Option<String>,
    excerpt: String,
    rect: Rect,
    dirty: bool,
    /// The theme the last layout was built with, for measuring.
    layout_theme: Theme,
}

impl QuoteLines {
    /// The rule's width: half the tight step.
    fn rule(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.tight) / 2.0
    }

    /// The text's leading edge from the rule's: the rule and the related step.
    fn inset(theme: &Theme) -> f64 {
        Self::rule(theme) + f64::from(theme.tokens.spacing.rhythm.related)
    }

    /// The gap between the author and the excerpt on one line, and between two lines.
    fn gap(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.tight)
    }

    /// The author's style and the excerpt's: both captions, the author's medium.
    fn styles(theme: &Theme) -> (TextStyle, TextStyle) {
        (
            theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Medium),
            theme.text(TextRole::Caption),
        )
    }

    /// The head: the caption and the author, or the author alone.
    fn head(&self) -> Option<String> {
        match (&self.caption, &self.author) {
            (Some(caption), Some(author)) => Some(format!("{caption} {author}")),
            (Some(caption), None) => Some(caption.clone()),
            (None, Some(author)) => Some(author.clone()),
            (None, None) => None,
        }
    }

    /// Whether the excerpt stands on a line of its own, under the head.
    fn stacked(&self) -> bool {
        self.caption.is_some()
    }

    fn line(text: &mut TextRenderer, theme: &Theme) -> f64 {
        f64::from(text.measure_style("Ag", None, &Self::styles(theme).1).1)
    }

    fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let line = Self::line(text, theme);
        if self.stacked() {
            line * 2.0 + Self::gap(theme)
        } else {
            line
        }
    }

    fn natural_width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let (strong, plain) = Self::styles(theme);
        let head = self.head().map_or(0.0, |head| {
            f64::from(text.measure_style(&head, None, &strong).0)
        });
        let excerpt = f64::from(text.measure_style(&self.excerpt, None, &plain).0);
        let lines = if self.stacked() {
            head.max(excerpt)
        } else if head > 0.0 {
            head + Self::gap(theme) + excerpt
        } else {
            excerpt
        };
        (Self::inset(theme) + lines).ceil()
    }

    fn spoken(&self) -> String {
        [self.head(), Some(self.excerpt.clone())]
            .into_iter()
            .flatten()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// `s` in `style` and `ink` at `at`, cut short at `right`; how wide it was drawn.
fn draw_line(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    (s, style, ink): (&str, &TextStyle, vello::peniko::Color),
    at: (f64, f64),
    right: f64,
) -> f64 {
    let room = crate::num::f32_of_f64((right - at.0).max(0.0));
    let shown = text.elide_to_width_style(s, room, style);
    text.draw_style(
        scene,
        &shown,
        ink,
        transform.pre_translate(at.into()),
        None,
        style,
    );
    f64::from(text.measure_style(&shown, None, style).0)
}

impl Widget for QuoteLines {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        use crate::widget::TaffyTreeExt as _;
        if self.layout_theme != *theme {
            self.layout_theme = theme.clone();
            self.dirty = true;
        }
        let style = taffy::Style {
            size: taffy::Size::auto(),
            flex_shrink: 1.0,
            min_size: taffy::Size {
                width: taffy::Dimension::length(0.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let theme = self.layout_theme.clone();
        let natural = crate::num::f32_of_f64(self.natural_width(text, &theme));
        let width = match available.width {
            taffy::AvailableSpace::Definite(w) => natural.min(w),
            _ => natural,
        };
        taffy::Size {
            width: known.width.unwrap_or(width),
            height: known
                .height
                .unwrap_or(crate::num::f32_of_f64(self.height(text, &theme))),
        }
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let rect = self.rect;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let height = self.height(text, theme);
        let top = rect.y0 + (rect.height() - height) / 2.0;
        // The rule runs the height of what is quoted, rounded at its ends.
        let rule = vello::kurbo::RoundedRect::from_rect(
            Rect::new(rect.x0, top, rect.x0 + Self::rule(theme), top + height),
            Self::rule(theme) / 2.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.text_tertiary,
            None,
            &rule,
        );
        let (strong, plain) = Self::styles(theme);
        let x = rect.x0 + Self::inset(theme);
        let head = self.head();
        if self.stacked() {
            if let Some(head) = &head {
                draw_line(
                    scene,
                    transform,
                    text,
                    (head, &strong, colors.text_primary),
                    (x, top),
                    rect.x1,
                );
            }
            let below = top + Self::line(text, theme) + Self::gap(theme);
            draw_line(
                scene,
                transform,
                text,
                (&self.excerpt, &plain, colors.text_dim),
                (x, below),
                rect.x1,
            );
        } else {
            let mut at = x;
            if let Some(head) = &head {
                at += draw_line(
                    scene,
                    transform,
                    text,
                    (head, &strong, colors.text_primary),
                    (x, top),
                    rect.x1,
                ) + Self::gap(theme);
            }
            if at < rect.x1 {
                draw_line(
                    scene,
                    transform,
                    text,
                    (&self.excerpt, &plain, colors.text_dim),
                    (at, top),
                    rect.x1,
                );
            }
        }
    }

    crate::redraw_flag!();

    fn is_layout_dirty(&self) -> bool {
        self.dirty
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "QuoteLines"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::StaticText).with_name(self.spoken()))
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
